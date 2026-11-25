//! The FIRE is journaled and replays: the fence, its negative control, the trailing arm, bars.

use std::path::Path;

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, spawn_core};
use vike_execution::{BarUpdate, Command, ConditionalIntent, OrderIntent};
use vike_journal::JournalRecord;

use super::support::{core_config, unique_dir};
use crate::kit::engines::sim_engine;
use crate::kit::events::{ohlc_bar, sim_bare_fill, sim_quote};
use crate::kit::journal::{records, rewrite_journal};

/// The bar-close twin of [`core_config`]: the emulated trigger is checked on CLOSED BARS (the
/// oracle-faithful default), which is a DIFFERENT call site (`fire_conditionals_bar`) from the
/// tick path every other test here drives.
fn core_config_bars(dir: &Path) -> CoreConfig {
    CoreConfig { conditionals_on_ticks: false, ..core_config(dir) }
}

/// 4 bare warmup fills (the 4th trips the cadence `Snapshot` = the replay BASE), then in the TAIL:
/// arm a fixed sell-stop at 95, a quote at 96 that does NOT cross, and a quote at 94 that DOES —
/// firing the conditional and releasing a market sell through `apply_intent`.
fn build_fired_conditional_journal(dir: &Path) {
    let handle = spawn_core(sim_engine(), core_config(dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snapshot (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
        kind_terms: None,
        account: None,
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
            JournalRecord::OrderEmulationArmed { arm_id, resolved, .. } => Some((arm_id, resolved)),
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
            JournalRecord::OrderEmulationFire {
                arm_id,
                request: req,
                trigger_price: trigger_px,
                ..
            } => Some((arm_id, req, *trigger_px)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the 94.0 tick fired exactly once (one-shot)");
    assert_eq!(fired[0].0, armed[0].0, "the FIRE names the arm it consumed");
    assert_eq!(fired[0].1.order_type, vike_model::OrderType::Market);
    assert_eq!(fired[0].1.side, -1);
    assert_eq!(fired[0].1.qty, 1.0);
    assert!(
        fired[0].1.order_client_id.is_empty(),
        "the release is recorded PRE-mint, so replay re-mints the identical order client id"
    );
    assert_eq!(fired[0].2, 94.0, "the oracle's diagnostic trigger price (a gap fills adverse)");

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A stop larger than its position is two write-ahead records of one arm**, each what is sent, the close flagged and the
/// rest plain, and the session still re-folds to a bit-identical `state_hash`.
#[test]
fn a_stop_larger_than_the_position_journals_both_orders_and_replays() {
    let dir = unique_dir("fire-past-position");
    let handle = spawn_core(sim_engine(), core_config(&dir));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap(); // 4th -> cadence Snapshot (BASE)
    }
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 6.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
        kind_terms: None,
        account: None,
    })));
    handle.tick_sender().quote(sim_quote(94.0, 6)).unwrap(); // crosses: FIRE
    handle.shutdown_and_join();

    let fired: Vec<(f64, bool)> = records(&dir)
        .iter()
        .filter_map(|r| match r {
            JournalRecord::OrderEmulationFire { request: req, .. } => {
                Some((req.qty, req.reduce_only))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fired, vec![(4.0, true), (2.0, false)]);
    let out = replay_offline(&dir).expect("the two releases replay");
    assert_eq!(out.snaps_compared, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A rest the gate refuses is on the record as nothing**: one write-ahead record, the close's, and the replay re-applies
/// that record and never re-decides the fire, so it reaches the same state with no mark to judge the rest on.
#[test]
fn a_stop_whose_rest_is_refused_journals_the_close_alone_and_replays() {
    let dir = unique_dir("fire-rest-refused");
    let mut engine = sim_engine();
    engine.gate.limits.max_total_exposure = Some(150.0); // the rest of 2 ends short about 190
    let handle = spawn_core(engine, core_config(&dir));
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap(); // 4th -> cadence Snapshot (BASE)
    }
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 6.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
        kind_terms: None,
        account: None,
    })));
    handle.tick_sender().quote(sim_quote(96.0, 5)).unwrap(); // prices the engine, no fire
    handle.tick_sender().quote(sim_quote(94.0, 6)).unwrap(); // crosses: FIRE
    handle.shutdown_and_join();

    let fired: Vec<(f64, bool)> = records(&dir)
        .iter()
        .filter_map(|r| match r {
            JournalRecord::OrderEmulationFire { request: req, .. } => {
                Some((req.qty, req.reduce_only))
            }
            _ => None,
        })
        .collect();
    assert_eq!(fired, vec![(4.0, true)]);
    let out = replay_offline(&dir).expect("the close alone replays");
    assert_eq!(out.snaps_compared, 1);
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

/// The negative control that proves the test above is load-bearing: strip the `OrderEmulationFire`
/// records out of the journal (exactly the pre-PR-1 shape — the release was never recorded) and
/// the SAME fence rejects it. Rebuilt through a fresh `Journal` so the frames stay valid;
/// only the fire record is dropped.
#[test]
fn an_unjournaled_fire_would_have_mismatched() {
    let src = unique_dir("fire-negative-src");
    build_fired_conditional_journal(&src);

    let stripped = unique_dir("fire-negative-stripped");
    rewrite_journal(
        &stripped,
        records(&src)
            .into_iter()
            // the pre-PR-1 hole (no FIRE record); `GtdExpire` was never re-emitted either
            .filter(|r| {
                !matches!(
                    r,
                    JournalRecord::OrderEmulationFire { .. } | JournalRecord::GtdExpire { .. }
                )
            })
            .collect(),
    );

    assert!(
        replay_offline(&stripped).is_err(),
        "without the FIRE record the release vanishes from the replay — the residual PR-1 closes"
    );

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&stripped);
}

/// A TRAILING arm's extreme is seeded from the standing MARK — market data, never journaled — so
/// the write-ahead `Command` for the intent cannot carry it. `OrderEmulationArmed` records the resolved
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
        kind_terms: None,
        account: None,
    })));
    handle.shutdown_and_join();

    let armed: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::OrderEmulationArmed { resolved, .. } => Some(resolved),
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
            JournalRecord::OrderEmulationArmed { .. } | JournalRecord::OrderEmulationFire { .. }
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
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snapshot (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
        kind_terms: None,
        account: None,
    })));
    let bars = handle.bar_sender();
    let close = |b: vike_marketdata::Bar| {
        bars.close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: b,
            sides: None,
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
            JournalRecord::OrderEmulationFire { arm_id, request: req, .. } => Some((arm_id, req)),
            _ => None,
        })
        .collect();
    assert_eq!(fired.len(), 1, "the second bar fired exactly once (one-shot), off the BAR path");
    assert_eq!(fired[0].1.order_type, vike_model::OrderType::Market);
    assert_eq!(fired[0].1.side, -1);
    assert!(fired[0].1.order_client_id.is_empty(), "recorded pre-mint, as on the tick path");
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::OrderEmulationArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1);
    assert_eq!(fired[0].0, armed[0], "the FIRE names the arm it consumed");

    replay_offline(&dir).expect("a bar-fired session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}
