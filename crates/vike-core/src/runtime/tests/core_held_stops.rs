//! The hygiene of an operator's CORE-HELD reduce-only stop (`core_held.rs`): a RESTORED stop is HELD until its
//! account's first reconcile pass, a stop whose position went away is CLEARED, a stop whose close failed is
//! RE-ARMED, and the armed stops are LISTED. White-box, like `nothing_to_reduce_tests` (the stores and the
//! sweep are private to `runtime`), through the real arm (`apply_intent`), the real tick path
//! (`fire_conditionals_at_price`), the real pass (`reconcile_reports`) and the real timer arm
//! (`drive_due_timers`).
//!
//! What each test pins:
//!
//! * a restored stop that a tick crosses before its account's first pass fires NOTHING and stays armed, says so
//!   ONCE, is listed `held`, and fires at the next crossing after THAT account's pass (another account's pass
//!   releases nothing); the hold LAPSES when no pass comes; a stop the operator arms after the restart is not
//!   held;
//! * a covered stop is cleared when its position goes flat or flips, a stop armed on a flat book waits for its
//!   position, a part-closed position keeps its stop, a stopped account's stop is left alone;
//! * a fired stop whose close was refused or part-filled is armed again (at its own trigger, for what is left),
//!   at most `MAX_STOP_REARMS` times, and not after the operator emptied the book;
//! * the listing carries the core-held stops and nothing else;
//! * the sweep is a timer arm of the stuck-order clock.

use super::*;
use std::time::Duration;
use vike_exec::testing::RecordingClient;
use vike_exec::{OrderIntent, PositionEntry};
use vike_model::events::PositionSide;
use vike_model::{OrderRequest, OrderStatus, OrderType};

use crate::runtime::core_held::MAX_STOP_REARMS;
use crate::runtime::test_support::{core_of, engine_on};
use vike_venues::venue_restore::RESTORED_TAG_DENY_WINDOW_MS;

type Core = CoreThread<RecordingClient>;

/// A venue whose row has no reduce-only stop, so the operator's stop is held by the core.
const V: &str = "polymarket";
const SYM: &str = "X";

fn set_position(c: &mut Core, idx: usize, size: f64) {
    let key = (V.into(), SYM.into(), PositionSide::Both);
    let book = &mut c.eng_mut(idx).account.positions;
    if size == 0.0 {
        book.shift_remove(&key);
    } else {
        book.insert(key, PositionEntry { size, avg_px: 100.0, ..Default::default() });
    }
}

/// One account holding `position` of `X`.
fn core(position: f64) -> Core {
    let mut c =
        core_of(engine_on(V, SYM, RecordingClient::default()), Vec::new(), CoreConfig::default());
    set_position(&mut c, 0, position);
    c
}

/// The journaled arm of a core-held stop (what a `Snap` or a crash tail carries): SELL `qty` below `trigger`.
fn snap_arm(
    id: &str,
    route_key: Option<&str>,
    qty: f64,
    trigger: f64,
) -> vike_journal::SnapConditional {
    vike_journal::SnapConditional {
        arm_id: id.into(),
        terms: vike_journal::ConditionalRecord {
            venue: V.into(),
            symbol: SYM.into(),
            side: -1,
            qty,
            price: Some(trigger),
            trail: None,
            extreme: None,
            trigger_by: None,
            reduce_only: true,
        },
        route_key: route_key.map(str::to_string),
    }
}

/// A single-account core that RESTORED the arms, holding `position`.
fn restored(arms: Vec<vike_journal::SnapConditional>, position: f64) -> Core {
    let mut c = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig { conditionals: arms, ..CoreConfig::default() },
    );
    set_position(&mut c, 0, position);
    c
}

/// The operator's `submit polymarket X sell <qty> --type stop --trigger <trigger> --reduce-only`; the arm id.
fn arm(c: &mut Core, qty: f64, trigger: f64, now: i64) -> String {
    let before: std::collections::HashSet<String> = c.cond_engine.keys().cloned().collect();
    c.apply_intent(
        OrderIntent::Submit(Box::new(OrderRequest {
            venue: V.into(),
            symbol: SYM.into(),
            side: -1,
            qty,
            order_type: OrderType::Stop,
            trigger_price: Some(trigger),
            reduce_only: true,
            ..Default::default()
        })),
        now,
    );
    c.cond_engine.keys().find(|k| !before.contains(*k)).cloned().expect("the stop was armed")
}

fn armed(c: &Core) -> usize {
    c.conditional_books.values().map(|b| b.len()).sum()
}

fn tick(c: &mut Core, px: f64, ts: i64) {
    c.fire_conditionals_at_price(V, SYM, px, ts);
}

fn ring(c: &Core, needle: &str) -> usize {
    c.recent.iter().filter(|l| l.contains(needle)).count()
}

fn sent(c: &Core) -> usize {
    c.engine.client.submissions.len()
}

/// One pass of the venue's account `route_key` (`None` = the sole account), empty under `quarantine`.
fn pass(route_key: Option<&str>) -> ReconcileReports {
    ReconcileReports {
        venue: V.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: vike_exec::recon::ReconPolicy {
            default: vike_exec::recon::ReconMode::Quarantine,
            ..Default::default()
        },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: vike_exec::recon::BalanceTol::default(),
        route_key: route_key.map(str::to_string),
        admitted_symbols: None,
        lists_trigger_orders: false,
    }
}

/// End the close the last fire sent the way the venue would have (`status`), with `filled` of it done.
fn end_last_close(c: &mut Core, status: OrderStatus, filled: f64) {
    let coid =
        c.engine.client.submissions.last().expect("a close was sent").client_order_id.clone();
    let mo = c.eng_mut(0).registry.get_mut(&coid).expect("registered");
    mo.status = status;
    mo.filled_qty = filled;
}

// ── (2) a RESTORED stop is HELD until its account's first reconcile pass ─────────────────────────

/// THE CASE. Restored over a position of 2, the stop is crossed by a tick BEFORE its account's first pass: it
/// fires nothing, stays armed (the `Snap` still writes it, `cancel <id>` still disarms it), is listed `held`
/// and says so once however many ticks cross it. The pass for its account releases it, and the next crossing
/// fires the capped, reduce-only close.
#[test]
fn a_restored_stop_is_held_until_its_accounts_first_pass_and_then_fires() {
    let mut c = restored(vec![snap_arm("r0", None, 5.0, 95.0)], 2.0);
    assert!(c.stop_holds.contains_key("r0"), "restore holds it");
    assert!(c.core_held_stop_views()[0].held, "…and the listing says so");

    c.engine.now_ms = 1_000;
    tick(&mut c, 94.0, 1);
    tick(&mut c, 93.0, 2);
    tick(&mut c, 92.0, 3);

    assert_eq!(sent(&c), 0, "nothing fired before the pass");
    assert_eq!(armed(&c), 1, "…and the stop is still armed: a held stop is not a dropped one");
    assert!(
        c.cond_engine.contains_key("r0") && c.tick_conditionals,
        "…with its side record and its lane"
    );
    assert_eq!(ring(&c, "while HELD"), 1, "said ONCE, not per tick: {:?}", c.recent);

    c.reconcile_reports(pass(None));

    assert!(c.stop_holds.is_empty(), "the pass released it");
    assert_eq!(ring(&c, "RELEASED"), 1, "{:?}", c.recent);
    assert!(!c.core_held_stop_views()[0].held);
    tick(&mut c, 94.0, 4);
    let out = &c.engine.client.submissions;
    assert_eq!(out.len(), 1, "it fires now");
    assert_eq!(
        (out[0].side, out[0].qty, out[0].reduce_only),
        (-1, 2.0, true),
        "capped to the book"
    );
}

/// The hold keeps the stop's OWN terms: after being held back it still fires at its original trigger, not at the
/// price that crossed it, and a price on the safe side fires nothing.
#[test]
fn a_held_back_stop_keeps_its_trigger() {
    let mut c = restored(vec![snap_arm("r0", None, 2.0, 95.0)], 2.0);
    tick(&mut c, 80.0, 1); // a gap far through the trigger, held
    c.reconcile_reports(pass(None));

    tick(&mut c, 96.0, 2); // above the trigger: nothing
    assert_eq!(sent(&c), 0);
    let view = &c.core_held_stop_views()[0];
    assert_eq!((view.trigger_price, view.qty), (95.0, 2.0), "the armed terms, not the gap's price");
    tick(&mut c, 94.0, 3);
    assert_eq!(sent(&c), 1);
}

/// Another account's pass releases nothing: each stop waits for the first pass of ITS account.
#[test]
fn only_the_pass_of_the_stops_own_account_releases_it() {
    let mut alt = engine_on(V, SYM, RecordingClient::default());
    alt.route_key = format!("{V}#ALT");
    let mut c = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        vec![(1_000.0, alt)],
        CoreConfig {
            seed_cash: 1_000.0,
            conditionals: vec![snap_arm("r0", Some(&format!("{V}#ALT")), 5.0, 95.0)],
            ..CoreConfig::default()
        },
    );
    set_position(&mut c, 1, 3.0);
    assert_eq!(c.stop_holds.get("r0").map(|h| h.engine), Some(1), "held on ALT's engine");

    c.reconcile_reports(pass(Some(V)));
    assert!(c.stop_holds.contains_key("r0"), "the default account's pass is not ALT's");
    tick(&mut c, 94.0, 1);
    assert!(c.extra_engines[0].1.client.submissions.is_empty(), "still held");

    c.reconcile_reports(pass(Some(&format!("{V}#ALT"))));
    assert!(c.stop_holds.is_empty());
    tick(&mut c, 94.0, 2);
    let sent = &c.extra_engines[0].1.client.submissions;
    assert_eq!((sent.len(), sent[0].qty), (1, 3.0), "ALT's 3 on ALT's client");
}

/// A pass that never comes must not leave the position bare for good: the hold LAPSES a window after the
/// first crossing it held back, and the stop fires against the book as it stands.
#[test]
fn a_hold_lapses_when_no_pass_comes() {
    let mut c = restored(vec![snap_arm("r0", None, 5.0, 95.0)], 2.0);
    c.engine.now_ms = 10_000;
    tick(&mut c, 94.0, 1);
    assert_eq!(sent(&c), 0, "held at the first crossing");

    c.engine.now_ms = 10_000 + RESTORED_TAG_DENY_WINDOW_MS - 1;
    tick(&mut c, 94.0, 2);
    assert_eq!(sent(&c), 0, "still inside the window");

    c.engine.now_ms = 10_000 + RESTORED_TAG_DENY_WINDOW_MS;
    tick(&mut c, 94.0, 3);
    assert_eq!(sent(&c), 1, "the window is over: it fires");
    assert!(c.stop_holds.is_empty());
    assert_eq!(ring(&c, "hold LAPSED"), 1, "{:?}", c.recent);
}

/// …and the window is counted from the first SWEEP after the restart, so a stop that is crossed hours later (no
/// pass ever came) is not held for a window more at the crossing.
#[test]
fn the_sweep_starts_the_lapse_clock_and_releases_the_hold_when_it_runs_out() {
    let mut c = restored(vec![snap_arm("r0", None, 5.0, 95.0)], 2.0);
    c.sweep_core_held_stops(1_000);
    assert_eq!(c.stop_holds["r0"].until, Some(1_000 + RESTORED_TAG_DENY_WINDOW_MS));

    c.sweep_core_held_stops(1_000 + RESTORED_TAG_DENY_WINDOW_MS - 1);
    assert!(c.stop_holds.contains_key("r0"), "inside the window");

    c.sweep_core_held_stops(1_000 + RESTORED_TAG_DENY_WINDOW_MS);
    assert!(c.stop_holds.is_empty(), "lapsed");
    assert_eq!(ring(&c, "hold LAPSED"), 1, "{:?}", c.recent);
    c.engine.now_ms = 5_000_000;
    tick(&mut c, 94.0, 1);
    assert_eq!(sent(&c), 1, "a crossing after the lapse fires at once");
}

/// A stop the operator arms AFTER the restart is not held: they armed it with the book in view.
#[test]
fn a_stop_armed_after_the_restart_is_not_held() {
    let mut c = restored(vec![snap_arm("r0", None, 5.0, 90.0)], 2.0);
    let fresh = arm(&mut c, 2.0, 95.0, 1);
    assert!(!c.stop_holds.contains_key(&fresh) && c.stop_holds.contains_key("r0"));

    tick(&mut c, 94.0, 2); // crosses the new one only
    assert_eq!(sent(&c), 1, "the new stop fired");
    assert!(c.cond_engine.contains_key("r0"), "the restored one (90) still waits");
}

// ── (4) a stop whose position went away is CLEARED ────────────────────────────────────────────────

/// A take-profit, a strategy's exit, a liquidation or a flatten closed the position: the sweep clears the stop,
/// says so once, and the tick lane it kept on goes off. A later position on the far side is not stopped by it.
#[test]
fn a_covered_stop_is_cleared_when_another_order_closes_its_position() {
    let mut c = core(2.0);
    let id = arm(&mut c, 2.0, 95.0, 0);
    assert!(c.cond_engine[&id].covered, "armed over a position: covered");

    c.sweep_core_held_stops(1_000);
    assert_eq!(armed(&c), 1, "the position is there: nothing to clear");

    set_position(&mut c, 0, 0.0); // closed by another order
    c.sweep_core_held_stops(2_000);

    assert_eq!(armed(&c), 0);
    assert!(c.cond_engine.is_empty() && !c.tick_conditionals, "the side record and the lane went");
    assert_eq!(ring(&c, "CLEARED"), 1, "said once: {:?}", c.recent);
    assert!(
        c.recent.iter().any(|l| l.contains(&id) && l.contains("is now flat")),
        "{:?}",
        c.recent
    );

    set_position(&mut c, 0, 3.0);
    tick(&mut c, 94.0, 3);
    assert_eq!(sent(&c), 0, "a later position is not closed by the cleared stop");
    c.sweep_core_held_stops(3_000);
    assert_eq!(ring(&c, "CLEARED"), 1, "and nothing is said again");
}

/// A position that FLIPPED (the SELL stop now sits on a short) is as gone as a flat one.
#[test]
fn a_flipped_position_clears_the_stop_too() {
    let mut c = core(2.0);
    arm(&mut c, 2.0, 95.0, 0);
    set_position(&mut c, 0, -1.0);
    c.sweep_core_held_stops(1_000);
    assert_eq!(armed(&c), 0);
    assert!(
        c.recent.iter().any(|l| l.contains("CLEARED") && l.contains("other side")),
        "{:?}",
        c.recent
    );
}

/// A stop armed on a FLAT book waits for its position (an operator may arm it before the entry fills): the sweep
/// leaves it, marks it covered when the position shows, and only then clears it when the position goes.
#[test]
fn a_stop_armed_on_a_flat_book_waits_for_its_position() {
    let mut c = core(0.0);
    let id = arm(&mut c, 2.0, 95.0, 0);
    assert!(!c.cond_engine[&id].covered);

    c.sweep_core_held_stops(1_000);
    assert_eq!(armed(&c), 1, "flat and never covered: left alone");

    set_position(&mut c, 0, 2.0); // the entry filled
    c.sweep_core_held_stops(2_000);
    assert!(c.cond_engine[&id].covered, "the sweep saw the position");
    assert_eq!(armed(&c), 1);

    set_position(&mut c, 0, 0.0);
    c.sweep_core_held_stops(3_000);
    assert_eq!(armed(&c), 0, "…and now that it went, the stop goes");
}

/// A position only PART closed keeps its stop (the fire caps to what is left), and a STOPPED account's stop is
/// the operator's to look at, not the sweep's to clear.
#[test]
fn a_part_closed_position_and_a_stopped_account_keep_their_stop() {
    let mut c = core(4.0);
    arm(&mut c, 4.0, 95.0, 0);
    set_position(&mut c, 0, 1.0);
    c.sweep_core_held_stops(1_000);
    assert_eq!(armed(&c), 1, "1 of 4 is still open");

    set_position(&mut c, 0, 0.0);
    c.retired_engines.push(0);
    c.sweep_core_held_stops(2_000);
    assert_eq!(armed(&c), 1, "a stopped account's stop is left");
}

/// A plain strategy stop (not reduce-only) is never cleared by this sweep.
#[test]
fn a_strategys_stop_is_left_to_its_strategy() {
    let mut c = core(2.0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: V.into(),
            symbol: SYM.into(),
            side: -1,
            qty: 2.0,
            price: Some(95.0),
            trail: None,
            trigger_by: None,
        }),
        0,
    );
    set_position(&mut c, 0, 0.0);
    c.sweep_core_held_stops(1_000);
    assert_eq!(armed(&c), 1);
    assert!(c.core_held_stop_views().is_empty(), "…and it is not listed as a core-held stop");
}

// ── (3) a stop whose close FAILED is RE-ARMED ─────────────────────────────────────────────────────

/// The close is still working: the sweep waits. It is REFUSED: the stop is armed again at its own trigger for what
/// is left (capped to the position), carrying a count, said once, and the next crossing fires again.
#[test]
fn a_refused_close_re_arms_the_stop_and_the_next_crossing_fires_it_again() {
    let mut c = core(2.0);
    let first = arm(&mut c, 5.0, 95.0, 0);
    tick(&mut c, 94.0, 1);
    assert_eq!((sent(&c), armed(&c)), (1, 0), "fired, and spent");

    c.sweep_core_held_stops(15_000);
    assert_eq!(armed(&c), 0, "the close is still live: no verdict yet");
    assert_eq!(c.fired_stops.len(), 1);

    end_last_close(&mut c, OrderStatus::Rejected, 0.0);
    c.sweep_core_held_stops(30_000);

    assert!(c.fired_stops.is_empty());
    assert_eq!(armed(&c), 1, "armed again");
    let view = &c.core_held_stop_views()[0];
    assert_ne!(view.arm_id, first, "a new arm id");
    assert_eq!((view.qty, view.trigger_price, view.rearms, view.held), (2.0, 95.0, 1, false));
    assert!(c.cond_engine[&view.arm_id].covered && c.tick_conditionals);
    assert_eq!(ring(&c, "RE-ARMED"), 1, "{:?}", c.recent);
    assert!(
        c.recent.iter().any(|l| l.contains(&first) && l.contains("re-arm 1 of")),
        "{:?}",
        c.recent
    );

    tick(&mut c, 94.0, 31);
    assert_eq!(sent(&c), 2, "the re-armed stop fires again");
}

/// A close that FILLED IN PART (the rest cancelled): the stop comes back for what is left, not for what it was
/// armed with.
#[test]
fn a_part_filled_close_re_arms_for_what_is_left() {
    let mut c = core(4.0);
    arm(&mut c, 10.0, 95.0, 0);
    tick(&mut c, 94.0, 1);
    assert_eq!(c.engine.client.submissions[0].qty, 4.0);

    set_position(&mut c, 0, 1.0); // 3 of 4 filled
    end_last_close(&mut c, OrderStatus::Canceled, 3.0);
    c.sweep_core_held_stops(15_000);

    let view = &c.core_held_stop_views()[0];
    assert_eq!((view.qty, view.trigger_price, view.rearms), (1.0, 95.0, 1), "{view:?}");
}

/// A close that took the whole position re-arms nothing; neither does one whose position something else closed.
#[test]
fn a_filled_close_re_arms_nothing() {
    let mut c = core(2.0);
    arm(&mut c, 2.0, 95.0, 0);
    tick(&mut c, 94.0, 1);
    set_position(&mut c, 0, 0.0);
    end_last_close(&mut c, OrderStatus::Filled, 2.0);

    c.sweep_core_held_stops(15_000);

    assert_eq!(armed(&c), 0);
    assert!(c.fired_stops.is_empty(), "judged and forgotten");
    assert_eq!(ring(&c, "RE-ARMED") + ring(&c, "GAVE UP"), 0, "{:?}", c.recent);
}

/// A venue that keeps refusing the close is asked `MAX_STOP_REARMS` more times and then the node says the
/// position is unprotected and stops: no endless loop of refused orders.
#[test]
fn re_arming_gives_up_after_the_cap_and_says_the_position_is_unprotected() {
    let mut c = core(2.0);
    arm(&mut c, 2.0, 95.0, 0);
    let mut at = 0;
    for _ in 0..=MAX_STOP_REARMS {
        tick(&mut c, 94.0, at);
        end_last_close(&mut c, OrderStatus::Rejected, 0.0);
        at += 15_000;
        c.sweep_core_held_stops(at);
    }

    assert_eq!(sent(&c), usize::from(MAX_STOP_REARMS) + 1, "the original fire and each re-arm's");
    assert_eq!(armed(&c), 0, "and no more");
    assert!(c.fired_stops.is_empty());
    assert_eq!(ring(&c, "RE-ARMED"), usize::from(MAX_STOP_REARMS), "{:?}", c.recent);
    assert_eq!(ring(&c, "GAVE UP"), 1, "{:?}", c.recent);
    assert!(
        c.recent.iter().any(|l| l.contains("GAVE UP") && l.contains("UNPROTECTED")),
        "{:?}",
        c.recent
    );
}

/// The operator emptied the book (mass-cancel): a stop that fired just before is not re-armed behind it.
#[test]
fn a_mass_cancel_takes_a_fired_stop_out_of_the_re_arm_watch() {
    let mut c = core(2.0);
    arm(&mut c, 2.0, 95.0, 0);
    tick(&mut c, 94.0, 1);
    assert_eq!(c.fired_stops.len(), 1);

    c.apply_intent(OrderIntent::MassCancel { venue: None, symbol: None, account: None }, 2);
    assert!(c.fired_stops.is_empty(), "forgotten with the book");

    end_last_close(&mut c, OrderStatus::Canceled, 0.0);
    c.sweep_core_held_stops(15_000);
    assert_eq!(armed(&c), 0, "nothing came back");
}

// ── the listing ────────────────────────────────────────────────────────────────────────────────

/// The snapshot carries the core-held stops with their account, terms, hold and count, and never a strategy's arm;
/// a node with none publishes an empty list.
#[test]
fn the_published_snapshot_lists_the_core_held_stops_and_only_those() {
    let mut c = core(2.0);
    assert!(c.core_held_stop_views().is_empty());
    c.publish_guarded();
    assert!(c.snapshot.load().core_held_stops.is_empty());

    let id = arm(&mut c, 5.0, 95.0, 0);
    c.apply_intent(
        OrderIntent::ArmConditional(ConditionalIntent {
            venue: V.into(),
            symbol: SYM.into(),
            side: -1,
            qty: 1.0,
            price: Some(90.0),
            trail: None,
            trigger_by: None,
        }),
        1,
    );
    c.publish_guarded();

    let listed = c.snapshot.load().core_held_stops.clone();
    assert_eq!(
        listed,
        vec![vike_exec::CoreHeldStopView {
            arm_id: id,
            venue: V.into(),
            symbol: SYM.into(),
            route_key: V.into(),
            side: -1,
            qty: 5.0,
            trigger_price: 95.0,
            held: false,
            rearms: 0,
        }]
    );
}

// ── the clock ──────────────────────────────────────────────────────────────────────────────────

/// The sweep is a timer arm of the stuck-order clock: nothing before its first tick, the clear at it.
#[test]
fn the_stuck_order_clock_runs_the_sweep() {
    let mut c = core_of(
        engine_on(V, SYM, RecordingClient::default()),
        Vec::new(),
        CoreConfig { submit_ack_timeout: Some(Duration::from_secs(30)), ..CoreConfig::default() },
    );
    set_position(&mut c, 0, 2.0);
    arm(&mut c, 2.0, 95.0, 0);
    c.arm_boundary_timers(0);
    set_position(&mut c, 0, 0.0);

    c.drive_due_timers(1_000);
    assert_eq!(armed(&c), 1, "the tick (15 s) has not come");

    c.drive_due_timers(20_000);
    assert_eq!(armed(&c), 0, "the clock cleared it");
}
