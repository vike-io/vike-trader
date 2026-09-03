//! Emulator-journal PR-1 gate: an emulated conditional's FIRE is journaled, and a session that
//! fires one replays to an IDENTICAL `state_hash`.
//!
//! Before this, `replay.rs`'s module doc named `submit_fired` as an unjournaled residual: the
//! trigger is a runtime reaction to a bar/tick, market data is deliberately never journaled, so
//! the released order simply did not exist in the record — the determinism fence caught the
//! divergence as `ReplayError::HashMismatch`. `conditional_fire_session_replays_deterministically`
//! is that scenario, green; `an_unjournaled_fire_would_have_mismatched` documents WHY it used to
//! fail by fencing the same journal with the fire record filtered out.
//!
//! The companion guards: a session that arms NOTHING writes neither new record kind (the
//! byte-identity posture — the two variants only ever appear for sessions that actually use
//! conditionals), and the ARM writes its RESOLVED terms (`ConditionalArmed`) including a trailing
//! arm's mark-seeded extreme, which the write-ahead `Cmd` for the intent cannot carry.
//!
//! Both firing call sites are covered: the TICK path (`conditionals_on_ticks`, most tests here)
//! and the CLOSED-BAR path (`the_bar_path_journals_its_fire_and_replays`). And
//! `arm_ids_do_not_collide_across_a_restart` fences the arm-id uniqueness the restored coid
//! session would otherwise break.
//!
//! Emulator PR-2 adds the DISARM half: `the_disarm_is_journaled_and_the_session_replays`
//! (the `ConditionalDisarmed` record + the determinism fence over a disarm session),
//! `a_disarmed_conditional_stays_disarmed_across_a_restart` (the restart sentinel), and
//! `arm_ids_survive_a_prune_then_restart_without_colliding` (the prune-safe `Snap.arm_seq`
//! resume — the record-count formula this replaces undercounted after
//! `prune_before_latest_snap` deleted early segments).
//!
//! Emulator PR-3 adds RE-ARM-ON-RESTORE: the books ride `Snap.conditionals` and come back via
//! `RestoredState::conditionals` -> `CoreConfig::conditionals`.
//! `armed_conditionals_survive_a_restart_with_the_trailing_extreme_preserved` is the flipped
//! sentinel (the restart used to DROP armed conditionals; now both a fixed and a trailing stop —
//! ratcheted extreme included — survive and fire identically), and
//! `a_crash_tail_fire_and_arm_fold_into_the_restored_books` gates the crash-tail record fold
//! (a fired arm never resurrects; a tail arm re-arms from its `ConditionalArmed`).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{spawn_core, CoreConfig, JournalConfig};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, BarUpdate, Command, ConditionalIntent, ExecutionEngine, OrderIntent,
    QuoteUpdate, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};
use vike_model::{Bar, Clock, QuoteTick};

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("cond-{tag}"))
}

/// A bare external fill (empty coid) — the journaled `Ingest::Event` that folds into position/pnl
/// with no client involvement, so replay reproduces it exactly.
fn fill(tid: &'static str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    })
}

fn quote(px: f64, ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: ts,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        },
    }
}

fn core_config(dir: &Path) -> CoreConfig {
    // self-advancing clock: message k dispatches at now_ms == k, so a wrong QueueClock replay of
    // the released order's `created_ms` (which IS hashed) would break the fence.
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        // the emulated trigger is checked off the tick lanes (no bars in this scenario)
        conditionals_on_ticks: true,
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    }
}

/// The bar-close twin of [`core_config`]: the emulated trigger is checked on CLOSED BARS (the
/// oracle-faithful default), which is a DIFFERENT call site (`fire_conditionals_bar`) from the
/// tick path every other test here drives.
fn core_config_bars(dir: &Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// 4 bare warmup fills (the 4th trips the cadence `Snap` = the replay BASE), then in the TAIL:
/// arm a fixed sell-stop at 95, a quote at 96 that does NOT cross, and a quote at 94 that DOES —
/// firing the conditional and releasing a market sell through `apply_intent`.
fn build_fired_conditional_journal(dir: &Path) {
    let handle = spawn_core(engine(), core_config(dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.tick_sender().quote(quote(96.0, 5)).unwrap(); // above the stop: no fire
    handle.tick_sender().quote(quote(94.0, 6)).unwrap(); // crosses: FIRE -> released market sell
    handle.shutdown_and_join();
}

fn records(dir: &Path) -> Vec<JournalRecord> {
    CommandJournal::read_all(dir).unwrap()
}

#[test]
fn the_fire_is_journaled_with_its_arm_id_and_released_request() {
    let dir = unique_dir("fire-record");
    build_fired_conditional_journal(&dir);
    let recs = records(&dir);

    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, resolved, .. } => Some((arm_id, resolved)),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1, "exactly one ARM was applied");
    assert_eq!(armed[0].1.price, Some(95.0));
    assert_eq!(armed[0].1.side, -1);
    assert_eq!(armed[0].1.trail, None);
    assert_eq!(armed[0].1.extreme, None, "a FIXED stop has no trailing extreme");

    let fired: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { arm_id, req, trigger_px, .. } => {
                Some((arm_id, req, *trigger_px))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the 94.0 tick fired exactly once (one-shot)");
    assert_eq!(fired[0].0, armed[0].0, "the FIRE names the arm it consumed");
    assert_eq!(fired[0].1.order_type, "market");
    assert_eq!(fired[0].1.side, -1);
    assert_eq!(fired[0].1.qty, 1.0);
    assert!(
        fired[0].1.client_order_id.is_empty(),
        "the release is recorded PRE-mint, so replay re-mints the identical coid"
    );
    assert_eq!(fired[0].2, 94.0, "the oracle's diagnostic trigger price (a gap fills adverse)");

    let _ = std::fs::remove_dir_all(&dir);
}

/// THE gate: the fence that used to fail. A session whose only tail write is an emulated
/// conditional's release re-folds to a bit-identical `state_hash`.
#[test]
fn conditional_fire_session_replays_deterministically() {
    let dir = unique_dir("fire-replay");
    build_fired_conditional_journal(&dir);
    let out = replay_offline(&dir).expect("a fired-conditional session must replay");
    assert_eq!(out.snaps_compared, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The negative control that proves the test above is load-bearing: strip the `ConditionalFire`
/// records out of the journal (exactly the pre-PR-1 shape — the release was never recorded) and
/// the SAME fence rejects it. Rebuilt through a fresh `CommandJournal` so the frames stay valid;
/// only the fire record is dropped.
#[test]
fn an_unjournaled_fire_would_have_mismatched() {
    let src = unique_dir("fire-negative-src");
    build_fired_conditional_journal(&src);

    let stripped = unique_dir("fire-negative-stripped");
    let mut j = CommandJournal::open(&stripped, JournalFileConfig::default()).unwrap();
    for r in records(&src) {
        match r {
            JournalRecord::ConditionalFire { .. } => {} // the pre-PR-1 hole
            JournalRecord::GtdExpire { .. } => {}
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(now_ms, &msg).unwrap();
            }
            JournalRecord::Snap {
                now_ms,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                conditionals,
                contingencies,
                mount_attr,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    &conditionals,
                    &contingencies,
                    &mount_attr,
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent).unwrap();
            }
            JournalRecord::ConditionalArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::ConditionalDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::PortfolioSnap { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, req, mount_id, .. } => {
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref()).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);

    assert!(
        replay_offline(&stripped).is_err(),
        "without the FIRE record the release vanishes from the replay — the residual PR-1 closes"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&stripped);
}

/// A TRAILING arm's extreme is seeded from the standing MARK — market data, never journaled — so
/// the write-ahead `Cmd` for the intent cannot carry it. `ConditionalArmed` records the resolved
/// terms instead (the `MintedSubmit` precedent), which is what a future restore/replay re-arms from.
#[test]
fn a_trailing_arm_records_its_mark_seeded_extreme() {
    let dir = unique_dir("trailing-armed");
    let handle = spawn_core(engine(), core_config(&dir));
    // a quote first: the trailing arm is REFUSED without a mark to seed from
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        price: None,
        trail: Some(5.0),
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let armed: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1);
    assert_eq!(armed[0].trail, Some(5.0));
    assert_eq!(armed[0].extreme, Some(100.0), "seeded from the standing mark");
    assert_eq!(armed[0].price, None);
    assert_eq!(armed[0].qty, 2.0);

    let _ = std::fs::remove_dir_all(&dir);
}

/// The byte-identity posture: the two new record kinds appear ONLY for sessions that actually use
/// conditionals. A session that arms nothing writes exactly the records it always did.
#[test]
fn a_session_with_no_conditionals_writes_no_conditional_records() {
    let dir = unique_dir("no-conditionals");
    let handle = spawn_core(engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    handle.tick_sender().quote(quote(101.0, 3)).unwrap();
    handle.shutdown_and_join();

    let recs = records(&dir);
    assert!(!recs.is_empty(), "the scenario did journal");
    assert!(
        !recs.iter().any(|r| matches!(
            r,
            JournalRecord::ConditionalArmed { .. } | JournalRecord::ConditionalFire { .. }
        )),
        "no conditional was armed, so no conditional record may exist"
    );
    replay_offline(&dir).expect("and it still replays exactly as before");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The BAR path (`fire_conditionals_bar`) journals its FIRE too, and that session replays.
///
/// Every other test here drives the TICK path (`fire_conditionals_at_price`, the opt-in
/// `conditionals_on_ticks` upgrade). Both funnel through `submit_fired`, but "both paths are
/// covered" was previously an argument, not a test — this is the closed-bar half, on the
/// oracle-faithful default (`conditionals_on_ticks: false`), so a regression that journals only on
/// the tick lane cannot pass.
#[test]
fn the_bar_path_journals_its_fire_and_replays() {
    let dir = unique_dir("fire-bar");
    let handle = spawn_core(engine(), core_config_bars(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    let bars = handle.bar_sender();
    let close = |b: vike_model::Bar| {
        bars.close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
    };
    close(bar(60_000, 100.0, 101.0, 99.0, 100.0)); // low 99 > 95: no cross
    close(bar(120_000, 100.0, 100.5, 94.0, 96.0)); // low 94 crosses the stop: FIRE
    handle.shutdown_and_join();

    let recs = records(&dir);
    let fired: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { arm_id, req, .. } => Some((arm_id, req)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the second bar fired exactly once (one-shot), off the BAR path");
    assert_eq!(fired[0].1.order_type, "market");
    assert_eq!(fired[0].1.side, -1);
    assert!(fired[0].1.client_order_id.is_empty(), "recorded pre-mint, as on the tick path");
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1);
    assert_eq!(fired[0].0, armed[0], "the FIRE names the arm it consumed");

    replay_offline(&dir).expect("a bar-fired session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Arm one conditional, restart the core the way a real crash-restart does (restore the engine +
/// the COID SESSION from the journal, then keep APPENDING to the same journal directory), arm
/// another — the two arm ids must differ.
///
/// This is the collision the arm-id doc used to deny: `arm_id` is `{coid_session}a{arm_seq}`, the
/// session is deliberately restored, and both runs write into ONE log — so a counter that restarts
/// at 0 re-emits `<same-session>a0`. Harmless while the id is diagnostic, NOT harmless in PR-2,
/// where `arm_id` is the disarm key and `ConditionalBook::disarm` drops EVERY match. The fix is
/// resuming the counter from `RestoredState::arm_seq`; the second half of this test pins the
/// documented failure mode (restore the session but NOT the counter ⇒ ids collide), so the
/// "restore both or neither" contract is test-backed in both directions.
#[test]
fn arm_ids_do_not_collide_across_a_restart() {
    let ids_for = |resume_arm_seq: bool| -> Vec<String> {
        let dir = unique_dir(if resume_arm_seq { "arm-restart" } else { "arm-restart-noresume" });
        // Run 1: arm a stop that never fires, then shut down (the exit Snap is the restore base).
        let handle = spawn_core(engine(), core_config(&dir));
        handle.event_sender().blocking_send(fill("t0", 1.0, 100.0)).unwrap();
        handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        })));
        handle.shutdown_and_join();

        // Restart: restore engine + coid session (+ optionally the arm counter) onto the SAME dir.
        let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote a Snap");
        let cfg = CoreConfig {
            seed_cash: state.engines[0].equity_seed,
            coid_session: Some((state.coid_session.clone(), state.coid_seq)),
            arm_seq: if resume_arm_seq { state.arm_seq } else { 0 },
            ..core_config(&dir)
        };
        let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
        let handle = spawn_core(engine2, cfg);
        handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            price: Some(90.0),
            trail: None,
            trigger_by: None,
        })));
        handle.shutdown_and_join();

        let ids: Vec<String> = records(&dir)
            .into_iter()
            .filter_map(|r| match r {
                JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
                _ => None,
            })
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        ids
    };

    let resumed = ids_for(true);
    assert_eq!(resumed.len(), 2, "one arm per run, both in the one appended-to journal");
    assert_ne!(
        resumed[0], resumed[1],
        "a restored coid session + a resumed arm counter still yields DISTINCT arm ids"
    );

    let not_resumed = ids_for(false);
    assert_eq!(not_resumed.len(), 2);
    assert_eq!(
        not_resumed[0], not_resumed[1],
        "and this is WHY the counter must be resumed: restoring the session alone re-emits the id"
    );
}

/// Emulator PR-2 gate, disarm half: the `DisarmConditional` verb drops the arm live (the quote
/// that would have crossed it releases nothing), journals a `ConditionalDisarmed` record naming
/// the arm — and ONLY for the actual disarm, the unknown-id refusal writes nothing — and the
/// session still replays to a bit-identical `state_hash` (the disarm rides its own write-ahead
/// `Cmd`, so the replay core's book drops the same arm).
#[test]
fn the_disarm_is_journaled_and_the_session_replays() {
    let dir = unique_dir("disarm");
    let handle = spawn_core(engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    // an unknown id first: a loud no-op that must journal NOTHING
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da999".into(),
    }));
    // the real disarm: the session is pinned ("cafef00d") and this is its first arm -> a0
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da0".into(),
    }));
    handle.tick_sender().quote(quote(94.0, 7)).unwrap(); // would have crossed the 95 stop
    handle.shutdown_and_join();

    let recs = records(&dir);
    let disarmed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalDisarmed { arm_id, .. } => Some(arm_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        disarmed,
        vec!["cafef00da0"],
        "exactly ONE ConditionalDisarmed — the real disarm; the unknown-id refusal wrote nothing"
    );
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalFire { .. })),
        "the disarmed conditional must NOT have fired on the crossing quote"
    );

    replay_offline(&dir).expect("a disarm session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The restart sentinel, #460 half: a conditional disarmed before the crash/shutdown STAYS
/// disarmed after a crash-restart onto the same journal — re-feeding the crossing price into the
/// restored core releases nothing and appends no `ConditionalFire`. Since emulator PR-3 the
/// restart SEEDS the restored books (`RestoredState::conditionals` -> `CoreConfig::conditionals`),
/// so this now proves the restore fold consumed the `ConditionalDisarmed` record rather than
/// resurrecting the arm from its `ConditionalArmed` — and the restore-side assertion pins the
/// books coming back EMPTY.
#[test]
fn a_disarmed_conditional_stays_disarmed_across_a_restart() {
    let dir = unique_dir("disarm-restart");
    // Run 1: arm, then DISARM. (Quote first so the mark exists; fixed stops don't need it, but
    // the scenario mirrors the live shape.)
    let handle = spawn_core(engine(), core_config(&dir));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da0".into(),
    }));
    handle.shutdown_and_join();

    // Restart the way a real crash-restart does: restore engine + coid session + arm counter +
    // the conditional books, then keep APPENDING to the same journal directory.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote a Snap");
    assert!(
        state.conditionals.is_empty(),
        "the disarmed arm must NOT come back in the restored books"
    );
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.tick_sender().quote(quote(94.0, 10)).unwrap(); // the price that would cross the stop
    handle.shutdown_and_join();

    let recs = records(&dir);
    assert!(
        recs.iter().any(|r| matches!(r, JournalRecord::ConditionalDisarmed { .. })),
        "precondition: run 1 really did disarm"
    );
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalFire { .. })),
        "the disarmed conditional must stay disarmed across the restart — no fire, ever"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE emulator PR-3 gate — the sentinel above's other half, flipped from documenting the drop to
/// proving the restore: an armed fixed stop AND an armed trailing stop (whose extreme has MOVED
/// off its seed) survive a restart onto the same journal — both come back armed under their
/// original ids, the trailing extreme is the RATCHETED value (not the arm-time seed, and not
/// re-seeded from any post-restart mark), and the fires behave identically post-restore.
///
/// Discriminating price ladder: mark 100 -> arm trailing (seed 100, trail 5) -> quote 110
/// ratchets the extreme to 110 (trigger 105). After the restart, quote 106: NO fire under the
/// preserved extreme (106 > 105) — but a restore that re-seeded the extreme from the first
/// post-restart mark (106 -> trigger 101) or from the arm-time seed (100 -> trigger 95) would
/// ALSO not fire at the next quote 104, which the preserved extreme DOES (104 < 105). The fixed
/// stop then fires at 94, proving the whole book (not just one arm) re-armed.
#[test]
fn armed_conditionals_survive_a_restart_with_the_trailing_extreme_preserved() {
    let dir = unique_dir("rearm-restart");
    // Run 1: mark, arm a fixed sell-stop @95 (a0) + a trailing sell-stop trail 5 (a1, seed 100),
    // ratchet the trailing extreme to 110, shut down (the exit Snap captures the books).
    let handle = spawn_core(engine(), core_config(&dir));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        price: None,
        trail: Some(5.0),
        trigger_by: None,
    })));
    handle.tick_sender().quote(quote(110.0, 2)).unwrap(); // ratchets a1's extreme 100 -> 110
    handle.shutdown_and_join();

    // Restore: BOTH arms come back, ids intact, the trailing extreme is the RATCHETED 110.
    let state = restore_from_journal(&dir).unwrap().expect("run 1 wrote a Snap");
    assert_eq!(state.conditionals.len(), 2, "both arms restored");
    assert_eq!(state.conditionals[0].arm_id, "cafef00da0");
    assert_eq!(state.conditionals[0].terms.price, Some(95.0));
    assert_eq!(state.conditionals[1].arm_id, "cafef00da1");
    assert_eq!(state.conditionals[1].terms.trail, Some(5.0));
    assert_eq!(
        state.conditionals[1].terms.extreme,
        Some(110.0),
        "the Snap captured the RATCHETED extreme, not the arm-time seed (100)"
    );
    assert_eq!(state.arm_seq, 2, "the counter resumes above both restored ids");

    // Run 2: restart with the restored books. 106 must NOT fire (preserved trigger 105); 104
    // fires the trailing arm; 94 fires the fixed stop.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.tick_sender().quote(quote(106.0, 10)).unwrap(); // above trigger 105: must NOT fire
    handle.tick_sender().quote(quote(104.0, 11)).unwrap(); // crosses 105: trailing (a1) fires
    handle.tick_sender().quote(quote(94.0, 12)).unwrap(); // crosses 95: fixed (a0) fires
    handle.shutdown_and_join();

    let fired: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { arm_id, trigger_px, req, .. } => {
                Some((arm_id, trigger_px, req.qty))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 2, "both restored arms fired exactly once, nothing doubled");
    assert_eq!(fired[0].0, "cafef00da1", "the trailing arm fired FIRST — under its original id");
    assert_eq!(
        fired[0].1, 104.0,
        "it fired on the 104 tick — only the preserved extreme (110 -> trigger 105) crosses \
         there; a re-seeded extreme (106 -> 101, or the seed 100 -> 95) would not have"
    );
    assert_eq!(fired[0].2, 2.0, "with its original qty");
    assert_eq!(fired[1].0, "cafef00da0", "the fixed stop fired under its original id");
    assert_eq!(fired[1].2, 1.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The CRASH-TAIL half of re-arm-on-restore: restored book membership is a pure fold of the
/// journal's conditional records over the latest Snap's books — NOT the replay core's own book
/// state, which never fires (its book would still hold an arm the live session consumed) and
/// refuses trailing arms (no mark). Hand-builds the crash shape (records AFTER the last Snap,
/// no exit Snap — a clean shutdown always writes one, so this cannot be produced through a core):
/// base books hold a trailing arm a0 (ratcheted extreme) + a fixed a1; the tail then FIRES a0
/// and ARMS a fixed a2. The restore must hold exactly {a1, a2} — a0 re-armed would DOUBLE an
/// already-taken exit — and resume the counter above a2.
#[test]
fn a_crash_tail_fire_and_arm_fold_into_the_restored_books() {
    use vike_core::journal::{ConditionalRecord, SnapConditional};

    let dir = unique_dir("crash-tail-fold");
    let base_books = vec![
        SnapConditional {
            arm_id: "cafef00da0".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(110.0),
                trigger_by: None,
            },
        },
        SnapConditional {
            arm_id: "cafef00da1".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                extreme: None,
                trigger_by: None,
            },
        },
    ];
    {
        let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
        let engines = vec![engine().snapshot_state()];
        let hash = vike_exec::state_hash(&engines);
        // the base Snap: 2 arms resting, counter at 2
        j.append_snap(100, &engines, "cafef00d", 0, 2, &base_books, &[], &[], hash).unwrap();
        // crash tail: a0 FIRED live (write-ahead record + the released market sell)...
        j.append_conditional_fire(
            101,
            "cafef00da0",
            105.0,
            &vike_model::OrderRequest {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                order_type: "market".into(),
                ts: 101,
                ..Default::default()
            },
        )
        .unwrap();
        // ...then a NEW fixed stop was armed (write-ahead Cmd + its resolved ConditionalArmed)...
        j.append_cmd(
            102,
            &vike_exec::Ingest::Command(Command::Order(OrderIntent::ArmConditional(
                ConditionalIntent {
                    venue: "sim".into(),
                    symbol: "BTCUSDT".into(),
                    side: -1,
                    qty: 3.0,
                    price: Some(90.0),
                    trail: None,
                    trigger_by: None,
                },
            ))),
        )
        .unwrap();
        j.append_conditional_armed(
            102,
            "cafef00da2",
            &ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 3.0,
                price: Some(90.0),
                trail: None,
                extreme: None,
                trigger_by: None,
            },
        )
        .unwrap();
        j.flush().unwrap();
        // ...and the process died here: no exit Snap.
    }

    let state = restore_from_journal(&dir).unwrap().expect("the Snap is the restore base");
    let ids: Vec<&str> = state.conditionals.iter().map(|c| c.arm_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["cafef00da1", "cafef00da2"],
        "the fired arm is CONSUMED (never re-armed — that would double the exit); the tail arm \
         is re-armed from its ConditionalArmed record"
    );
    assert_eq!(state.conditionals[1].terms.price, Some(90.0), "the tail arm's resolved terms");
    assert!(state.arm_seq >= 3, "the counter resumes above every spent id (got {})", state.arm_seq);
    let _ = std::fs::remove_dir_all(&dir);
}

/// #460 post-merge review MAJOR (confirmed by this exact probe): the restored `arm_seq` must
/// come from RECORD TRUTH, never from replay-side re-minting. The trailing branch of
/// `ArmConditional` refuses BEFORE `mint_arm_id` when the symbol has no mark, and replay's marks
/// come only from the base Snap (the tail pumps no market data) — so a cadence Snap taken before
/// the symbol ever ticked (arm_seq=K stamped, NO mark), followed live by a mark + a trailing arm
/// (mints a{K}, journals its `ConditionalArmed`), followed by a crash, used to replay with the
/// counter still at K: the resumed session re-minted a{K}, and a stale `DisarmConditional`
/// carrying the old a{K} would disarm the NEW arm — silently removing a fresh stop-loss. The fix
/// fast-forwards the counter to `max(replayed, snap_seed + tail ConditionalArmed count)`.
#[test]
fn a_refused_tail_trailing_arm_cannot_undercount_the_restored_arm_seq() {
    use vike_core::journal::ConditionalRecord;

    let dir = unique_dir("armseq-undercount");
    const K: u64 = 5;
    {
        let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
        // Base Snap: arm_seq already at K, NO mark for the symbol (a fresh engine snapshot has
        // none), empty books.
        let engines = vec![engine().snapshot_state()];
        let hash = vike_exec::state_hash(&engines);
        j.append_snap(100, &engines, "cafef00d", 0, K, &[], &[], &[], hash).unwrap();
        // Crash tail: live, the mark had arrived (marks are market data — never journaled), so
        // the trailing arm was APPLIED: its write-ahead Cmd + the resolved ConditionalArmed
        // naming a{K}. Replay has no mark and refuses the Cmd — the pre-fix counter never
        // advanced past K.
        j.append_cmd(
            101,
            &vike_exec::Ingest::Command(Command::Order(OrderIntent::ArmConditional(
                ConditionalIntent {
                    venue: "sim".into(),
                    symbol: "BTCUSDT".into(),
                    side: -1,
                    qty: 1.0,
                    price: None,
                    trail: Some(5.0),
                    trigger_by: None,
                },
            ))),
        )
        .unwrap();
        j.append_conditional_armed(
            101,
            &format!("cafef00da{K}"),
            &ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(100.0),
                trigger_by: None,
            },
        )
        .unwrap();
        j.flush().unwrap();
        // crash: no exit Snap.
    }

    let state = restore_from_journal(&dir).unwrap().expect("the Snap is the restore base");
    assert!(
        state.arm_seq > K,
        "the counter must resume ABOVE the spent a{K} (got {}) — record truth, not replay minting",
        state.arm_seq
    );
    // PR-3 bonus, asserted so it can't silently regress: the refused-in-replay trailing arm
    // still RE-ARMS on restore, from its ConditionalArmed record (seed extreme).
    assert_eq!(state.conditionals.len(), 1, "the crash-tail trailing arm is restored");
    assert_eq!(state.conditionals[0].arm_id, format!("cafef00da{K}"));
    assert_eq!(state.conditionals[0].terms.extreme, Some(100.0), "the ARM-record seed extreme");

    // And the next live mint really is a{K+1}, never a duplicate a{K}: restart, arm once, read
    // the new ConditionalArmed id back out of the journal.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let armed_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    let new_id = armed_ids.last().expect("run 2 armed one conditional");
    assert_ne!(new_id, &format!("cafef00da{K}"), "the spent id must never be re-minted");
    assert_eq!(
        armed_ids.iter().filter(|id| *id == new_id).count(),
        1,
        "the new id collides with nothing in the journal"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// FNV-1a32 twin of the journal's frame checksum (the private `journal::fnv1a32`) — needed by
/// the byte-surgery test below to re-frame a doctored payload.
fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// #460 post-merge review MINOR: the pre-v7 fallback branch (`Snap.arm_seq` absent -> seed =
/// max-global-record-seq + 1) had zero coverage. Byte-surgery a real journal into the pre-v7
/// shape (strip `arm_seq` — and the v8 `conditionals` — from every Snap payload, restamp the
/// header version to 6), then prove `restore_from_journal` resumes the counter at the fallback
/// bound and a continued session mints ABOVE every pre-existing id.
#[test]
fn a_pre_v7_snap_falls_back_to_max_seq_plus_one_and_never_collides() {
    let dir = unique_dir("pre-v7-fallback");
    // Run 1 (a real core): a mark, one ARM (spends a0), shutdown (exit Snap).
    let handle = spawn_core(engine(), core_config(&dir));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();
    let max_seq = records(&dir)
        .iter()
        .map(|r| match r {
            JournalRecord::Cmd { seq, .. }
            | JournalRecord::Snap { seq, .. }
            | JournalRecord::StrategySubmit { seq, .. }
            | JournalRecord::MintedSubmit { seq, .. }
            | JournalRecord::PortfolioSnap { seq, .. }
            | JournalRecord::ConditionalArmed { seq, .. }
            | JournalRecord::ConditionalFire { seq, .. }
            | JournalRecord::ConditionalDisarmed { seq, .. }
            | JournalRecord::MarginCallLiquidate { seq, .. }
            | JournalRecord::GtdExpire { seq, .. }
            | JournalRecord::ScheduleFire { seq, .. } => *seq,
        })
        .max()
        .unwrap();

    // Byte-surgery every segment into the v6 shape: walk the [len][crc][payload] frames, strip
    // `arm_seq`/`conditionals` from each Snap payload, re-frame, restamp the header to 6.
    const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64 (stable format)
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut out = bytes[..HEADER].to_vec();
        out[4..8].copy_from_slice(&6u32.to_le_bytes());
        let mut cur = HEADER;
        while cur + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            if len == 0 || cur + 8 + len > bytes.len() {
                break;
            }
            let payload = &bytes[cur + 8..cur + 8 + len];
            let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
            if let Some(snap) = v.get_mut("Snap").and_then(|s| s.as_object_mut()) {
                snap.remove("arm_seq");
                snap.remove("conditionals");
            }
            let doctored = serde_json::to_vec(&v).unwrap();
            out.extend_from_slice(&(doctored.len() as u32).to_le_bytes());
            out.extend_from_slice(&fnv1a32(&doctored).to_le_bytes());
            out.extend_from_slice(&doctored);
            cur += 8 + len;
        }
        out.resize(bytes.len().max(out.len()), 0);
        std::fs::write(&path, out).unwrap();
    }

    // Restore: the doctored Snap stamps no counter, so the fallback bound applies.
    let state = restore_from_journal(&dir).unwrap().expect("the doctored journal keeps its Snap");
    assert_eq!(state.arm_seq, max_seq + 1, "a pre-v7 base falls back to max-global-record-seq + 1");
    assert!(state.conditionals.is_empty(), "a pre-v7 Snap carries no books: restored EMPTY");

    // Continue on the same dir and arm again: the new id must be above every pre-existing one.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let armed_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(armed_ids.len(), 2, "run 1's arm + run 2's arm");
    assert_eq!(armed_ids[0], "cafef00da0");
    assert_eq!(
        armed_ids[1],
        format!("cafef00da{}", max_seq + 1),
        "the continued session mints at the fallback bound — above every pre-existing id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE prune-safety gate (emulator PR-2, defect A): `RestoredState::arm_seq` must come from the
/// latest `Snap`'s STAMPED counter, not from counting currently-readable records —
/// `prune_before_latest_snap` deletes whole early segments (its own doc prescribes
/// prune-then-restore at restart), so the count lands BELOW the ids already spent and the
/// restored session re-mints duplicate arm ids: the disarm key. Scenario: mint many arms across
/// several small segments, snap, PRUNE, restart, arm again — the new id must be distinct from
/// every pre-prune id.
#[test]
fn arm_ids_survive_a_prune_then_restart_without_colliding() {
    let dir = unique_dir("arm-prune-restart");
    let n_arms: u64 = 40;
    // Small segments so the journal ROLLS and prune has early segments to delete; a generous
    // snapshot cadence so most records between snaps are the arms themselves.
    let cfg = CoreConfig {
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 8 * 1024, flush_every: 8 },
            snapshot_every: 16,
        }),
        ..core_config(&dir)
    };
    let handle = spawn_core(engine(), cfg);
    for i in 0..n_arms {
        handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(1.0 + i as f64), // far below any mark: never fires
            trail: None,
            trigger_by: None,
        })));
    }
    handle.shutdown_and_join();

    let run1_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(run1_ids.len() as u64, n_arms, "every arm journaled its ConditionalArmed");

    // PRUNE, exactly as `prune_before_latest_snap`'s doc prescribes at restart (no live core on
    // the dir). The early, arm-bearing segments are deleted; the survivors must be FEWER records
    // than arms minted, or this scenario would not distinguish the two formulas.
    CommandJournal::prune_before_latest_snap(&dir).unwrap();
    let survivors = records(&dir).len() as u64;
    assert!(
        survivors < n_arms,
        "scenario check: pruning must leave fewer readable records ({survivors}) than arm ids \
         spent ({n_arms}) — the record-count formula would restart the counter INSIDE the spent \
         range and collide"
    );

    // Restore: the counter comes from the Snap's stamped arm_seq, unharmed by the prune.
    let state = restore_from_journal(&dir).unwrap().expect("the pruned journal keeps its Snap");
    assert_eq!(
        state.arm_seq, n_arms,
        "RestoredState::arm_seq is the Snap-stamped counter, not the survivor count"
    );

    // Restart onto the same (pruned) journal and arm ONE more.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(0.5),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let all_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    let new_id = all_ids.last().expect("run 2 armed one conditional").clone();
    assert_eq!(new_id, format!("cafef00da{n_arms}"), "the counter resumed ABOVE every spent id");
    assert!(
        !run1_ids.contains(&new_id),
        "post-restart arm id {new_id} must differ from every pre-prune id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------------------------
// Emulator PR-6 — the WIDENED fence (the epic closer).
//
// Every PR above widened what REPLAYS; none widened what is CHECKED. `replay_offline` fenced
// exactly one thing — `state_hash(&engines)` — while `restore_from_journal` hands a restart three
// more values nothing looked at: the resting books PR-3 made restorable, and the `coid_seq`/
// `arm_seq` counters. The tests below are the two halves of closing that: a NEGATIVE control
// proving a book divergence used to slip through a green fence, and a POSITIVE one proving the
// single documented exclusion (a trailing arm's ratcheted extreme) is a scalpel, not a hole.
// ---------------------------------------------------------------------------------------------

/// Write `recs` verbatim into a fresh journal at `dst` (valid frames, current header version).
/// The doctoring tests below transform a REAL session's records and re-emit them through this, so
/// only the one field under test differs from a journal a live core would have written.
fn rewrite(dst: &Path, recs: Vec<JournalRecord>) {
    let mut j = CommandJournal::open(dst, JournalFileConfig::default()).unwrap();
    for r in recs {
        match r {
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(now_ms, &msg).unwrap();
            }
            JournalRecord::Snap {
                now_ms,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                conditionals,
                contingencies,
                mount_attr,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    &conditionals,
                    &contingencies,
                    &mount_attr,
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent).unwrap();
            }
            JournalRecord::ConditionalArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::ConditionalDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::ConditionalFire { now_ms, arm_id, trigger_px, req, .. } => {
                j.append_conditional_fire(now_ms, &arm_id, trigger_px, &req).unwrap();
            }
            JournalRecord::PortfolioSnap { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, req, mount_id, .. } => {
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref()).unwrap();
            }
            JournalRecord::GtdExpire { now_ms, coid, engine, .. } => {
                j.append_gtd_expire(now_ms, &coid, engine).unwrap();
            }
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);
}

/// A real session that arms a fixed stop and then DISARMS it, ending with the book empty.
fn build_armed_then_disarmed_journal(dir: &Path) {
    let handle = spawn_core(engine(), core_config(dir));
    let sender = handle.event_sender();
    for t in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(t, 1.0, 100.0)).unwrap();
    }
    // 4th record -> cadence Snap == the replay BASE (books still empty here)
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da0".into(),
    }));
    handle.shutdown_and_join();
}

/// THE PR-6 gate, negative half: a divergence in the RESTORED BOOKS that the engine `state_hash`
/// cannot see is now caught. Strip the `ConditionalDisarmed` record (the pre-PR-2 hole, and the
/// exact shape a `fold_conditionals` bug would produce) and the restore resurrects an arm the live
/// session had consumed — a restart would re-arm a stop the operator explicitly cancelled.
///
/// The disarm moves NO engine state (it mutates the core's book, not the OMS), so `state_hash` is
/// bit-identical either way: before this PR the doctored journal replayed GREEN. The assertion is
/// deliberately on the `conditionals` FIELD, not merely on `is_err()`, so a future change that
/// starts failing this journal for some unrelated reason cannot silently pass as coverage.
#[test]
fn the_book_fence_catches_a_resurrected_arm() {
    let src = unique_dir("pr6-books-src");
    build_armed_then_disarmed_journal(&src);
    let recs = records(&src);
    assert!(
        recs.iter().any(|r| matches!(r, JournalRecord::ConditionalDisarmed { .. })),
        "precondition: the session really disarmed"
    );
    replay_offline(&src).expect("the intact session passes every fence");

    // The control: the SAME journal minus the disarm record. The `Cmd` carrying the
    // `DisarmConditional` intent stays (the fold answers membership from records, by design), so
    // the ONLY difference is the record whose absence the fence must catch.
    let doctored = unique_dir("pr6-books-doctored");
    rewrite(
        &doctored,
        recs.into_iter()
            .filter(|r| !matches!(r, JournalRecord::ConditionalDisarmed { .. }))
            .collect(),
    );

    match replay_offline(&doctored) {
        Err(vike_core::replay::ReplayError::RestoreMismatch { field, .. }) => {
            assert_eq!(field, "conditionals", "the BOOK fence is what rejected it");
        }
        other => panic!(
            "a resurrected arm must fail the widened fence (it passed the engine hash fence \
             before PR-6), got {other:?}"
        ),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&doctored);
}

/// THE PR-6 gate, positive half: the one documented exclusion is load-bearing. A trailing arm's
/// `extreme` ratchets on non-journaled market data, so the reproduced book carries the
/// `ConditionalArmed` SEED where the live exit `Snap` carries the RATCHETED value — a real,
/// expected, safe-direction difference. `conditionals_hash` clears the field, so the session
/// replays green; a fence that hashed the extreme would reject this perfectly valid journal.
///
/// The test asserts the two values genuinely DIFFER first, so it cannot degenerate into proving
/// nothing once the scenario stops ratcheting.
#[test]
fn the_ratcheted_extreme_is_a_documented_exclusion_not_a_hole() {
    let dir = unique_dir("pr6-extreme");
    let handle = spawn_core(engine(), core_config(&dir));
    let sender = handle.event_sender();
    for t in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(t, 1.0, 100.0)).unwrap();
    }
    handle.tick_sender().quote(quote(100.0, 5)).unwrap(); // the mark the arm seeds from
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: None,
        trail: Some(5.0),
        trigger_by: None,
    })));
    handle.tick_sender().quote(quote(110.0, 6)).unwrap(); // ratchets the extreme 100 -> 110
    handle.shutdown_and_join();

    let recs = records(&dir);
    let seeded = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => resolved.extreme,
            _ => None,
        })
        .expect("the arm recorded its mark-seeded extreme");
    let recorded = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { conditionals, .. } => conditionals.first().and_then(|c| {
                c.terms.extreme.map(|e| (e, vike_core::replay::conditionals_hash(conditionals)))
            }),
            _ => None,
        })
        .expect("the exit Snap captured the still-armed trailing stop");
    assert_eq!(seeded, 100.0, "the ARM record holds the seed");
    assert_eq!(recorded.0, 110.0, "the exit Snap holds the RATCHETED extreme");

    // The reproduced book carries the seed, the recorded one the ratchet — same hash regardless.
    let reproduced = restore_from_journal(&dir).unwrap().expect("a Snap exists");
    assert_eq!(reproduced.conditionals.len(), 1, "the trailing arm is restored");
    assert_eq!(
        vike_core::replay::conditionals_hash(&reproduced.conditionals),
        recorded.1,
        "the ratcheted extreme is excluded, so the reproduced book hashes equal to the recorded one"
    );
    replay_offline(&dir).expect("a ratcheting trailing arm must not fail the widened fence");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The counter half of the widened fence, end to end: rewind nothing, but claim in the exit `Snap`
/// that the live session had spent MORE arm ids than the replay reproduces. That is precisely the
/// #460 bug class (a restored counter below the ids already spent re-mints a DISARM KEY), and
/// before PR-6 `replay_offline` never compared the two at all.
#[test]
fn the_counter_fence_catches_a_reproduced_arm_seq_below_the_recorded_one() {
    let src = unique_dir("pr6-armseq-src");
    build_armed_then_disarmed_journal(&src);
    replay_offline(&src).expect("the intact session passes every fence");

    let mut recs = records(&src);
    let last_snap = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snap { .. }))
        .expect("a clean shutdown always writes an exit Snap");
    if let JournalRecord::Snap { arm_seq, .. } = &mut recs[last_snap] {
        *arm_seq = Some(arm_seq.unwrap_or(0) + 1_000); // ids the replay cannot account for
    }
    let doctored = unique_dir("pr6-armseq-doctored");
    rewrite(&doctored, recs);

    match replay_offline(&doctored) {
        Err(vike_core::replay::ReplayError::RestoreMismatch { field, expected, got }) => {
            assert_eq!(field, "arm_seq", "the COUNTER fence is what rejected it");
            assert!(got < expected, "and it rejected for the undercount direction");
        }
        other => panic!("a reproduced arm_seq below the recorded one must be rejected: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&doctored);
}

/// The MARK-lane arm (w2 `trigger_by`): a `Some(Mark)` arm must ignore the LAST lane entirely and
/// fire only off a mark tick — and the resulting session must still replay to a bit-identical
/// `state_hash`. This is the scenario the whole field exists for: last=99, mark=94, SL=95 fires on
/// a mark-triggering venue (hyperliquid) and on no last-triggering one.
///
/// Both halves matter, and the two lanes deliberately cross at DIFFERENT prices so the recorded
/// `trigger_px` NAMES the lane that fired: the LAST quote at 90 crosses the 95 stop and must not
/// fire it (a leak there would record 90), and the later MARK tick at 94 must (recording 94). A
/// fire COUNT alone could not tell the two apart — either lane leaking still yields exactly one
/// fire, since the first crossing consumes the arm.
#[test]
fn a_mark_lane_arm_ignores_last_fires_on_mark_and_replays_deterministically() {
    let dir = unique_dir("mark-lane-fire");
    let handle = spawn_core(engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: Some(vike_model::TriggerBy::Mark),
    })));
    // a LAST print DEEP through the stop: the Mark arm must rest through it (a leak records 90)
    handle.tick_sender().quote(quote(90.0, 5)).unwrap();
    // …and the MARK tick at 94 is what fires it (recording 94)
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 94.0,
        ts: 6,
    });
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1);
    assert_eq!(
        armed[0].trigger_by,
        Some(vike_model::TriggerBy::Mark),
        "the requested source rides the ARM record, so a restore re-arms on the same lane"
    );
    let fired: Vec<f64> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { trigger_px, .. } => Some(*trigger_px),
            _ => None,
        })
        .collect();
    assert_eq!(
        fired,
        vec![94.0],
        "fired exactly once, at the MARK price — 90.0 here would mean the LAST lane leaked"
    );

    replay_offline(&dir).expect("a mark-fired session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The inert-default fence: a `None` (default) arm behaves EXACTLY as it did before `trigger_by`
/// existed — it fires off the LAST lane and a mark tick at the same crossing price never triggers
/// it. Pins that the mark lane added no firing surface to a source-less arm.
#[test]
fn a_default_arm_still_fires_on_last_and_never_on_mark() {
    let dir = unique_dir("default-lane-fire");
    let handle = spawn_core(engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(fill("t3", 1.0, 100.0)).unwrap();

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    // a MARK tick DEEP through the stop: a default (Last) arm must rest through it (leak = 90)
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 90.0,
        ts: 5,
    });
    // …the LAST quote at 94 is what fires it, exactly as before the field existed
    handle.tick_sender().quote(quote(94.0, 6)).unwrap();
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed[0].trigger_by, None, "no source requested — the record stays bare");
    let fired: Vec<f64> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { trigger_px, .. } => Some(*trigger_px),
            _ => None,
        })
        .collect();
    assert_eq!(
        fired,
        vec![94.0],
        "fired exactly once, at the LAST price — 90.0 here would mean the MARK lane leaked \
         into a source-less arm"
    );

    replay_offline(&dir).expect("the default path replays exactly as it always did");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An `Index` arm is REFUSED at apply time (the core has no index lane), so no arm and no
/// `ConditionalArmed` record exist — and the session still replays. Never armed inert, never
/// evaluated off a substituted series.
#[test]
fn an_index_arm_is_refused_and_journals_no_arm() {
    let dir = unique_dir("index-refused");
    let handle = spawn_core(engine(), core_config(&dir));
    handle.tick_sender().quote(quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: Some(vike_model::TriggerBy::Index),
    })));
    // a crossing print on BOTH lanes: a refused arm can fire on neither
    handle.tick_sender().quote(quote(94.0, 2)).unwrap();
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 94.0,
        ts: 3,
    });
    handle.shutdown_and_join();

    let recs = records(&dir);
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalArmed { .. })),
        "the Index request was refused before minting an id — nothing was armed"
    );
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalFire { .. })),
        "and nothing fired on either lane"
    );
    replay_offline(&dir).expect("a refused arm leaves a replayable session");
    let _ = std::fs::remove_dir_all(&dir);
}
