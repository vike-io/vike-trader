//! The FIRE is journaled and replays: the fence, its negative control, the trailing arm, bars.

use std::path::Path;

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, spawn_core};
use vike_exec::{BarUpdate, Command, ConditionalIntent, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};

use super::support::{core_config, unique_dir};
use crate::kit::engines::sim_engine;
use crate::kit::events::{ohlc_bar, sim_bare_fill, sim_quote};
use crate::kit::journal::records;

/// The bar-close twin of [`core_config`]: the emulated trigger is checked on CLOSED BARS (the
/// oracle-faithful default), which is a DIFFERENT call site (`fire_conditionals_bar`) from the
/// tick path every other test here drives.
fn core_config_bars(dir: &Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

/// 4 bare warmup fills (the 4th trips the cadence `Snap` = the replay BASE), then in the TAIL:
/// arm a fixed sell-stop at 95, a quote at 96 that does NOT cross, and a quote at 94 that DOES —
/// firing the conditional and releasing a market sell through `apply_intent`.
fn build_fired_conditional_journal(dir: &Path) {
    let handle = spawn_core(sim_engine(), core_config(dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.tick_sender().quote(sim_quote(96.0, 5)).unwrap(); // above the stop: no fire
    handle.tick_sender().quote(sim_quote(94.0, 6)).unwrap(); // crosses: FIRE -> released market sell
    handle.shutdown_and_join();
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
                j.append_minted_submit(now_ms, &req, None).unwrap();
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
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref(), None).unwrap();
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
    let handle = spawn_core(sim_engine(), core_config(&dir));
    // a quote first: the trailing arm is REFUSED without a mark to seed from
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
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
    let handle = spawn_core(sim_engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    handle.tick_sender().quote(sim_quote(101.0, 3)).unwrap();
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
    let handle = spawn_core(sim_engine(), core_config_bars(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

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
    close(ohlc_bar(60_000, 100.0, 101.0, 99.0, 100.0)); // low 99 > 95: no cross
    close(ohlc_bar(120_000, 100.0, 100.5, 94.0, 96.0)); // low 94 crosses the stop: FIRE
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
