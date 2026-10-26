//! Arm ids across a restart, the journaled DISARM, and armed or disarmed books across a restart.

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, ConditionalIntent, ExecutionEngine, OrderIntent};
use vike_journal::JournalRecord;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

use super::support::{core_config, unique_dir};
use crate::kit::engines::{engine_on, sim_engine};
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::journal::records;

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
        let handle = spawn_core(sim_engine(), core_config(&dir));
        handle.event_sender().blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
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
    let handle = spawn_core(sim_engine(), core_config(&dir));
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
    // an unknown id first: a loud no-op that must journal NOTHING
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da999".into(),
    }));
    // the real disarm: the session is pinned ("cafef00d") and this is its first arm -> a0
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da0".into(),
    }));
    handle.tick_sender().quote(sim_quote(94.0, 7)).unwrap(); // would have crossed the 95 stop
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
    let handle = spawn_core(sim_engine(), core_config(&dir));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
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
    handle.tick_sender().quote(sim_quote(94.0, 10)).unwrap(); // the price that would cross the stop
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
    let handle = spawn_core(sim_engine(), core_config(&dir));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
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
    handle.tick_sender().quote(sim_quote(110.0, 2)).unwrap(); // ratchets a1's extreme 100 -> 110
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
    handle.tick_sender().quote(sim_quote(106.0, 10)).unwrap(); // above trigger 105: must NOT fire
    handle.tick_sender().quote(sim_quote(104.0, 11)).unwrap(); // crosses 105: trailing (a1) fires
    handle.tick_sender().quote(sim_quote(94.0, 12)).unwrap(); // crosses 95: fixed (a0) fires
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

// ── an operator's CORE-HELD reduce-only stop (a venue with no native stop: polymarket's row) ────

/// A bare fill on `polymarket` (the sim fill, re-addressed): +1 at 100.
fn held_venue_fill(tid: &str) -> Event {
    let mut ev = sim_bare_fill(tid, 1.0, 100.0);
    if let Event::Fill(f) = &mut ev {
        f.venue = "polymarket".into();
    }
    ev
}

/// A polymarket/BTCUSDT quote at `px`.
fn held_venue_quote(px: f64, ts: i64) -> vike_exec::QuoteUpdate {
    let mut q = sim_quote(px, ts);
    q.venue = "polymarket".into();
    q
}

/// `submit polymarket BTCUSDT sell 5 --type stop --trigger 95 --reduce-only`, as the node receives it.
fn core_held_stop_submit() -> Command {
    Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        venue: "polymarket".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 5.0,
        order_type: OrderType::Stop,
        trigger_price: Some(95.0),
        reduce_only: true,
        ..Default::default()
    })))
}

/// The profile's `guards.conditionals_on_ticks` is OFF here, on purpose: a core-held stop switches the
/// tick lane on for itself, and that must hold through the journal and a restart too.
fn core_held_config(dir: &std::path::Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

/// **ARMED, JOURNALED, REPLAYED, RESTORED, FIRED CAPPED.** Four warm-up fills (the 4th trips the cadence
/// `Snap` = the replay BASE) leave +4; the operator's reduce-only stop for 5 is HELD (a
/// `ConditionalArmed` carrying the flag, no `MintedSubmit`: nothing was minted or refused) and the
/// session replays to a bit-identical fence. A restart from the `Snap` puts the arm back WITH its flag
/// (a single-account box: no route key, no mount), and the crossing quote fires a market of the 4 the
/// book holds, not the 5 it was armed for, `reduce_only`, under the arm's own id.
#[test]
fn a_core_held_stop_replays_survives_a_restart_and_fires_capped_to_the_position() {
    let dir = unique_dir("core-held-restart");
    let held_venue = || engine_on("polymarket", "BTCUSDT", RecordingClient::default());

    let handle = spawn_core(held_venue(), core_held_config(&dir));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(held_venue_fill(tid)).unwrap();
    }
    handle.send_command(core_held_stop_submit());
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, resolved, .. } => Some((arm_id, resolved)),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1, "the submit was ARMED, once");
    assert!(armed[0].1.reduce_only, "…as a reduce-only arm");
    assert_eq!((armed[0].1.side, armed[0].1.qty, armed[0].1.price), (-1, 5.0, Some(95.0)));
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::MintedSubmit { .. })),
        "nothing was minted: it was held, not refused"
    );
    replay_offline(&dir)
        .expect("the session replays: the arm is a function of the journaled submit");

    let state = restore_from_journal(&dir).unwrap().expect("the exit Snap");
    assert_eq!(state.conditionals.len(), 1, "the arm is in the restored books");
    assert!(state.conditionals[0].terms.reduce_only, "…with its flag: the Snap carries it");

    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_held_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.tick_sender().quote(held_venue_quote(96.0, 30)).unwrap(); // above the stop: nothing
    handle.tick_sender().quote(held_venue_quote(94.0, 31)).unwrap(); // crosses it
    handle.shutdown_and_join();

    let fired: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { arm_id, req, .. } => Some((arm_id, req)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the restored stop fired exactly once");
    assert_eq!(fired[0].0, armed[0].0.as_str(), "under its original id");
    let req = &fired[0].1;
    assert_eq!(
        (req.side, req.qty, req.reduce_only, &req.order_type),
        (-1, 4.0, true, &OrderType::Market),
        "capped to the restored +4 position, reduce-only"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A FIRED core-held stop replays deterministically**: the journaled fire carries the CAPPED request
/// and the flag, so the replay core re-applies exactly that market and never re-evaluates a trigger.
#[test]
fn a_fired_core_held_stop_replays_deterministically() {
    let dir = unique_dir("core-held-fire");
    let handle = spawn_core(
        engine_on("polymarket", "BTCUSDT", RecordingClient::default()),
        core_held_config(&dir),
    );
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(held_venue_fill(tid)).unwrap();
    }
    handle.send_command(core_held_stop_submit());
    handle.tick_sender().quote(held_venue_quote(96.0, 5)).unwrap(); // above the stop: no fire
    handle.tick_sender().quote(held_venue_quote(94.0, 6)).unwrap(); // crosses: FIRE
    handle.shutdown_and_join();

    let fired: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { req, .. } => Some(req),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the 94 tick fired it once, with the tick lane OFF in the profile");
    assert_eq!((fired[0].qty, fired[0].reduce_only), (4.0, true));
    replay_offline(&dir).expect("a fired core-held stop must replay");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A DROPPED core-held fire replays too**: a BUY stop on a long book would ADD to it, so when the tick
/// crosses it the core sends NOTHING and journals a bare `ConditionalDisarmed` (no write-ahead `Cmd`
/// behind it, unlike a disarm the operator asked for). The record fold removes the arm, so the books the
/// fence compares are empty on both sides and a restart restores no arm; no `ConditionalFire` exists.
#[test]
fn a_dropped_core_held_fire_journals_a_disarm_and_the_session_replays() {
    let dir = unique_dir("core-held-drop");
    let handle = spawn_core(
        engine_on("polymarket", "BTCUSDT", RecordingClient::default()),
        core_held_config(&dir),
    );
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(held_venue_fill(tid)).unwrap();
    }
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(OrderRequest {
        venue: "polymarket".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 5.0,
        order_type: OrderType::Stop,
        trigger_price: Some(105.0),
        reduce_only: true,
        ..Default::default()
    }))));
    handle.tick_sender().quote(held_venue_quote(100.0, 5)).unwrap(); // below the BUY stop: nothing
    handle.tick_sender().quote(held_venue_quote(106.0, 6)).unwrap(); // crosses it: DROPPED
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<&String> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    let disarmed: Vec<&String> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalDisarmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1, "the BUY stop was armed, once");
    assert_eq!(disarmed, armed, "the dropped fire journaled the disarm of that very arm");
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalFire { .. })),
        "nothing was fired: no order was sent"
    );
    replay_offline(&dir).expect("a session with a dropped core-held fire must replay");
    let state = restore_from_journal(&dir).unwrap().expect("the exit Snap");
    assert!(state.conditionals.is_empty(), "a restart restores no arm: it cannot be resurrected");
    let _ = std::fs::remove_dir_all(&dir);
}
