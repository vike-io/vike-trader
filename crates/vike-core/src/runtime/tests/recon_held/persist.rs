//! **Two passes in a row** (decision 0043 as amended 2026-11-21: `vike_execution::recon::persist` runs before
//! `resolve`). A pass compares venue reads taken over a few seconds with the book at fold time, so an order created
//! or filled in between reads as a divergence the next pass no longer raises (bybit#DEMO, 2026-11-20: 242 holds in a
//! day, every one healed by itself). Such a race is never held, announced or applied; a divergence two passes in a
//! row raise is, under every policy. The last tests are the UP window of `book_own_fills_filled_while_down`: our own
//! order's fill those two passes confirm missing is booked once, the pass that books it judges the position and the
//! cash after the booking (decision 0043 as amended 2026-11-22), and a foreign order's fill is held. The last three are
//! that amendment's follow-ups: a drift netted against a missed fill proposes no leg under the symbol's lot, a fill
//! still pending nets a confirmed drift (so `hybrid` never corrects it and then folds it too), and a fill the first
//! sync's adopted cash already holds is booked without its cash.

use super::*;
use crate::runtime::order_owners_tests::{Events, NOW, binance_engine, cell, config_on, spy_mount};
use crate::scratch::Scratch;

/// Register a LIVE local order `order_client_id` on `SYMBOL` (the way a restored order is) and return the venue's
/// row for it.
fn live_order(
    c: &mut CoreThread<RecordingClient>,
    order_client_id: &str,
) -> vike_model::OrderStatusReport {
    let order = vike_model::OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: format!("v-{order_client_id}").into(),
        order_client_id: Some(order_client_id.to_string()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: vike_model::OrderStatus::Accepted,
        timestamp: 5,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    };
    assert_eq!(c.engine_mut(0).reregister_orders(std::slice::from_ref(&order)), 1);
    order
}

/// A venue fill row on `venue`/`symbol` under trade id `trade`, naming `order_client_id` (`None`: a row that names
/// the order by its venue id only), a buy of 1 at 100 stamped `at`.
fn fill_row(
    venue: &str,
    symbol: &str,
    trade: &str,
    order_client_id: Option<&str>,
    at: i64,
) -> vike_model::FillReport {
    vike_model::FillReport {
        venue: venue.into(),
        symbol: symbol.into(),
        trade_id: vike_model::events::TradeId::new(trade).expect("a non-empty trade id"),
        venue_order_id: "v-fill".into(),
        order_client_id: order_client_id.map(str::to_string),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        timestamp: at,
    }
}

/// THE RACE (input 2 of the 2026-11-21 plan): the venue's order read left before our new order arrived, and its
/// position and fill reads already carry a fill the stream delivers a moment after the fold. One pass raises an
/// `OrphanLocalOrder`, a one-lot `PositionDrift` and a `MissingFill`; the next pass, healed, raises nothing. Nothing
/// is ever held or announced, and nothing is left pending.
#[test]
fn a_race_the_next_pass_heals_is_never_held() {
    let mut c = core();
    c.publish_to(0, bare_fill("t-open", 1, 1.0, 100.0));
    let order = live_order(&mut c, "o-1");
    let fills = || vec![fill_row(VENUE, SYMBOL, "t-race", None, 5)];

    let raced = ReconcileReports { fills: fills(), ..pass(vec![position(SYMBOL, 2.0, 1_000)]) };
    tick(&mut c, 1_000, raced);
    assert!(c.reconcile_alerts.is_empty(), "one pass is no proof: {:?}", held_kinds(&c));
    assert_eq!(reconcile_notes(&c), 0, "nothing announced");
    assert_eq!(held_quantity(&c, SYMBOL), 1.0);

    c.publish_to(0, bare_fill("t-race", 1, 1.0, 100.0)); // the stream delivers it after the fold
    let healed = ReconcileReports {
        orders: vec![order],
        fills: fills(),
        ..pass(vec![position(SYMBOL, 2.0, 1_000 + PASS_MS)])
    };
    tick(&mut c, 1_000 + PASS_MS, healed);
    assert!(c.reconcile_alerts.is_empty(), "{:?}", held_kinds(&c));
    assert_eq!(reconcile_notes(&c), 0);
    assert!(c.reconcile_pass[VENUE].streak.is_empty(), "nothing is left pending");
}

/// A divergence two passes in a row raise is held on the second, and announced then.
#[test]
fn a_divergence_two_passes_in_a_row_raise_is_held_on_the_second() {
    let mut c = core();
    c.publish_to(0, bare_fill("t-open", 1, 1.0, 100.0));
    let reports = |at: i64| ReconcileReports {
        fills: vec![fill_row(VENUE, SYMBOL, "t-missed", None, 5)],
        ..pass(vec![position(SYMBOL, 2.0, at)])
    };

    tick(&mut c, 1_000, reports(1_000));
    assert!(c.reconcile_alerts.is_empty(), "pending: {:?}", held_kinds(&c));
    tick(&mut c, 1_000 + PASS_MS, reports(1_000 + PASS_MS));
    assert_eq!(
        held_kinds(&c),
        vec![ReconcileDivergenceKind::MissingFill, ReconcileDivergenceKind::PositionDrift]
    );
    assert_eq!(reconcile_notes(&c), 2, "each announced once, on the second pass");
    assert_eq!(held_quantity(&c, SYMBOL), 1.0, "quarantine folds nothing");
}

/// The orphan orders `resolve` folds into ONE row (`ORPHAN_LOCAL_ORDER_KEY`) are judged per order: two passes that
/// each catch ANOTHER order in flight hold nothing; the order absent on two passes in a row is held, and alone.
#[test]
fn orphan_orders_are_judged_per_order_inside_the_one_row() {
    let mut c = core();
    let first = live_order(&mut c, "o-1");
    let second = live_order(&mut c, "o-2");
    let listing = |orders: Vec<vike_model::OrderStatusReport>| ReconcileReports {
        orders,
        ..pass(Vec::new())
    };

    tick(&mut c, 1_000, listing(vec![second.clone()])); // o-1 in flight
    tick(&mut c, 2_000, listing(vec![first.clone()])); // o-2 in flight
    assert!(c.reconcile_alerts.is_empty(), "another order each pass: {:?}", held_kinds(&c));

    tick(&mut c, 3_000, listing(vec![first])); // o-2 again
    let row = c
        .reconcile_alerts
        .values()
        .find(|h| h.kind == ReconcileDivergenceKind::OrphanLocalOrder)
        .expect("o-2 is held");
    assert!(row.detail.starts_with("1 live LOCAL order(s)"), "{}", row.detail);
    assert!(row.detail.contains("o-2") && !row.detail.contains("o-1"), "{}", row.detail);
}

/// Under `hybrid` a `PositionDrift` rewrites the book with no operator in front of it: only on its second pass.
#[test]
fn hybrid_applies_a_position_drift_only_on_its_second_pass() {
    let mut c = core();
    c.publish_to(0, bare_fill("t-open", 1, 1.0, 100.0));
    let hybrid = |at: i64| ReconcileReports {
        policy: vike_execution::recon::ReconcilePolicy::of(vike_model::ReconcilePolicyName::Hybrid),
        ..pass(vec![position(SYMBOL, 2.0, at)])
    };

    tick(&mut c, 1_000, hybrid(1_000));
    assert_eq!(held_quantity(&c, SYMBOL), 1.0, "the first pass applies nothing");
    tick(&mut c, 1_000 + PASS_MS, hybrid(1_000 + PASS_MS));
    assert_eq!(held_quantity(&c, SYMBOL), 2.0, "the second applies the venue's size");
    assert!(c.reconcile_alerts.is_empty(), "{:?}", held_kinds(&c));
}

/// A held `PositionDrift` whose gap MOVES is never confirmed on its old reading: the pass that sees the new gap once
/// (pending) leaves the old row stale, so it is pruned and a confirm of its id folds nothing; the new gap's second pass
/// holds it, and that confirm books the venue's new figure.
#[test]
fn a_held_drift_whose_gap_moved_is_not_confirmed_on_its_old_reading() {
    let mut c = core();
    c.publish_to(0, bare_fill("t-open", 1, 1.0, 100.0));
    let at = |p: i64| 1_000 + p * PASS_MS;
    tick(&mut c, at(0), pass(vec![position(SYMBOL, 2.0, at(0))]));
    tick(&mut c, at(1), pass(vec![position(SYMBOL, 2.0, at(1))]));
    assert_eq!(
        held_kinds(&c),
        vec![ReconcileDivergenceKind::PositionDrift],
        "the gap of 1 is held"
    );
    let old = alert_ids(&c)[0];

    tick(&mut c, at(2), pass(vec![position(SYMBOL, 1.5, at(2))])); // the gap is 0.5 now: seen once
    assert!(c.reconcile_alerts.is_empty(), "the old reading is stale: {:?}", held_kinds(&c));
    c.confirm_reconcile(old);
    assert_eq!(held_quantity(&c, SYMBOL), 1.0, "the venue's old 2.0 is never written");

    tick(&mut c, at(3), pass(vec![position(SYMBOL, 1.5, at(3))]));
    let (&id, row) = c.reconcile_alerts.first().expect("the gap of 0.5 is held on its second pass");
    assert_eq!(row.kind, ReconcileDivergenceKind::PositionDrift);
    c.confirm_reconcile(id);
    assert_eq!(held_quantity(&c, SYMBOL), 1.5, "the confirmed reading is the venue's new figure");
}

/// When this process started `binance`'s link: a minute before the core clock.
const LINK_START: i64 = NOW - 60_000;

/// A `binance` core whose mount has an order WORKING (placed in this process, so in the engine registry and owned by
/// the mount), its venue link started at [`LINK_START`]. Returns the order's client id.
fn binance_mount_with_a_working_order(dir: &Scratch) -> (String, CoreThread<RecordingClient>) {
    let (next, events) = (cell(), Events::default());
    let mut mount = spy_mount(&next, &events);
    mount.venue = "binance".into();
    let mut c =
        core_of(binance_engine("binance", 1_000.0), Vec::new(), config_on(dir, vec![mount]));
    *next.lock().unwrap() = Some(1);
    c.drive_strategy_feed_status("binance", "BTCUSDT", FeedStatus::Live);
    let order_client_id =
        c.engine.client.submissions.last().expect("the mount quoted").order_client_id.clone();
    c.engine.venue_link_started_ms = Some(LINK_START);
    (order_client_id, c)
}

/// One `binance` pass under `quarantine` reporting `fills`, delivered as `handle` delivers it.
fn binance_pass(c: &mut CoreThread<RecordingClient>, fills: Vec<vike_model::FillReport>) {
    c.reconcile_reports(ReconcileReports { venue: "binance".into(), fills, ..pass(Vec::new()) });
    c.dispatch_applied_fills();
}

/// Size of the `binance` engine's open position.
fn binance_position(c: &CoreThread<RecordingClient>) -> f64 {
    c.engine.account.positions.values().map(|p| p.size).sum()
}

/// THE 2026-11-20 FILL: bybit's stream reset 82 ms after our fill and the reconnect's history read had not indexed it.
/// Our own order's fill, missed while the node ran, is booked by the pass that confirms it (the second), under
/// `quarantine`; its mount hears it; a later stream delivery of the same trade id is refused by the fill ledger, and
/// a third pass raises nothing.
#[test]
fn our_fill_missed_while_up_is_booked_once_on_the_second_pass() {
    let dir = Scratch::reserved("vco-recon-missed-while-up");
    let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
    let row = || fill_row("binance", "BTCUSDT", "t-missed", Some(&order_client_id), NOW - 1_000);

    binance_pass(&mut c, vec![row()]);
    assert_eq!(binance_position(&c), 0.0, "one pass is no proof");

    binance_pass(&mut c, vec![row()]);
    assert_eq!(binance_position(&c), 1.0, "booked on the second pass");
    assert!(
        !c.reconcile_alerts.values().any(|h| h.kind == ReconcileDivergenceKind::MissingFill),
        "nothing held for it: {:?}",
        held_kinds(&c)
    );
    assert_eq!(c.mount_attribution[0].size, 1.0, "its mount heard the fill");
    assert!(c.recent.iter().any(|l| l.contains("RECON booked 1 fill(s)")), "{:?}", c.recent);

    let stream = vike_model::events::FillEvent {
        trade_id: "t-missed".into(),
        order_client_id: order_client_id.clone(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new("USDT"),
        liquidity_side: vike_model::events::LiquiditySide::Taker,
        timestamp: NOW - 1_000,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    };
    c.publish_to(0, vike_model::events::Event::Fill(stream));
    c.dispatch_applied_fills();
    assert_eq!(binance_position(&c), 1.0, "the ledger refused the stream's copy");

    binance_pass(&mut c, vec![row()]);
    assert_eq!(binance_position(&c), 1.0);
    let booked = c.recent.iter().filter(|l| l.contains("RECON booked")).count();
    assert_eq!(booked, 1, "booked once");
}

/// The book opens `qty` long on `binance`/`BTCUSDT` at 100 from an external fill (no order client id).
fn binance_open(c: &mut CoreThread<RecordingClient>, qty: f64) {
    let vike_model::events::Event::Fill(open) = bare_fill("t-open", 1, qty, 100.0) else {
        unreachable!("bare_fill is a fill")
    };
    let open =
        vike_model::events::FillEvent { venue: "binance".into(), symbol: "BTCUSDT".into(), ..open };
    c.publish_to(0, vike_model::events::Event::Fill(open));
}

/// One `binance` pass under `quarantine` reporting `fills`, the position `qty` and, when `Some`, the cash `cash`.
fn binance_pass_at(
    c: &mut CoreThread<RecordingClient>,
    fills: Vec<vike_model::FillReport>,
    qty: f64,
    cash: Option<f64>,
) {
    let row = vike_model::PositionStatusReport {
        venue: "binance".into(),
        ..position("BTCUSDT", qty, NOW)
    };
    c.reconcile_reports(ReconcileReports {
        venue: "binance".into(),
        fills,
        balance: cash,
        reconcile_balance: cash.is_some(),
        ..pass(vec![row])
    });
    c.dispatch_applied_fills();
}

/// A kind a booked fill can explain: a position kind or `BalanceDrift`.
fn book_kind(k: ReconcileDivergenceKind) -> bool {
    matches!(
        k,
        ReconcileDivergenceKind::PositionDrift
            | ReconcileDivergenceKind::PositionOnlyExternal
            | ReconcileDivergenceKind::OrphanLocalPosition
            | ReconcileDivergenceKind::BalanceDrift
    )
}

/// The held kinds a booked fill can explain ([`book_kind`]).
fn held_book_kinds(c: &CoreThread<RecordingClient>) -> Vec<ReconcileDivergenceKind> {
    held_kinds(c).into_iter().filter(|k| book_kind(*k)).collect()
}

/// THE SAME DEFECT IN THE UP WINDOW (bybit#DEMO, 2026-11-22 02:34:39, seen at a restart): the pass that books our fill
/// also raises, for the second time, the position and cash gaps that fill makes, because it diffs the book before the
/// booking. The book holds 0.5 and 1_000; the venue 1.5 and 995 (our buy of 1 at 100, fee 5, missed while up). The
/// second pass books it and judges the book after: no `PositionDrift`, no `BalanceDrift`, the fee counted once, and no
/// gap of the book pending after a third pass.
#[test]
fn our_fill_booked_while_up_leaves_no_drift_of_its_size() {
    let dir = Scratch::reserved("vco-recon-missed-while-up-drift");
    let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
    binance_open(&mut c, 0.5);
    binance_pass_at(&mut c, Vec::new(), 0.5, Some(1_000.0)); // the cash anchor
    let row = || vike_model::FillReport {
        commission: 5.0,
        ..fill_row("binance", "BTCUSDT", "t-missed", Some(&order_client_id), NOW - 1_000)
    };

    binance_pass_at(&mut c, vec![row()], 1.5, Some(995.0));
    assert_eq!(binance_position(&c), 0.5, "one pass is no proof");
    assert!(held_book_kinds(&c).is_empty(), "pending: {:?}", held_kinds(&c));

    binance_pass_at(&mut c, vec![row()], 1.5, Some(995.0));
    assert!(c.recent.iter().any(|l| l.contains("RECON booked 1 fill(s)")), "{:?}", c.recent);
    assert_eq!(binance_position(&c), 1.5, "booked on the second pass");
    assert_eq!(c.engine.account.balance, 995.0, "its fee, once");
    assert!(held_book_kinds(&c).is_empty(), "no gap of the booked fill: {:?}", held_kinds(&c));

    binance_pass_at(&mut c, vec![row()], 1.5, Some(995.0));
    assert!(held_book_kinds(&c).is_empty(), "{:?}", held_kinds(&c));
    assert_eq!(c.engine.account.balance, 995.0);
    let streak = &c.reconcile_pass["binance"].streak;
    assert!(!streak.pending(book_kind), "no gap of the book is pending: {streak:?}");
}

/// A REAL drift beside the booking is still held on the second pass: the venue holds 1.75, the book 0.5 and our missed
/// buy of 1. The drift is held, and its proposed legs are the residual 0.25 alone (`resolve` nets the booked fill), so a
/// confirm lands the book on the venue's figure.
#[test]
fn a_drift_of_another_size_beside_a_booking_is_still_held() {
    let dir = Scratch::reserved("vco-recon-missed-while-up-real-drift");
    let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
    binance_open(&mut c, 0.5);
    let row = || fill_row("binance", "BTCUSDT", "t-missed", Some(&order_client_id), NOW - 1_000);

    binance_pass_at(&mut c, vec![row()], 1.75, None);
    assert!(held_book_kinds(&c).is_empty(), "pending: {:?}", held_kinds(&c));
    binance_pass_at(&mut c, vec![row()], 1.75, None);

    assert_eq!(binance_position(&c), 1.5, "the fill is booked");
    assert_eq!(held_book_kinds(&c), vec![ReconcileDivergenceKind::PositionDrift]);
    let drift = c
        .reconcile_alerts
        .values()
        .find(|h| h.kind == ReconcileDivergenceKind::PositionDrift)
        .expect("the drift is held");
    let legs: f64 = drift
        .proposed_events
        .iter()
        .filter_map(|e| match e {
            vike_model::events::Event::Fill(f) => Some(f64::from(f.side) * f.last_qty),
            _ => None,
        })
        .sum();
    assert!((legs - 0.25).abs() < 1e-12, "the residual alone: {legs}");
}

/// A FOREIGN order's fill (an order client id no mount minted) and a row naming our order by its venue id only are
/// HELD on the second pass, never booked; so is our own fill stamped before this process started the account's link.
#[test]
fn a_foreign_fill_is_held_not_booked() {
    let dir = Scratch::reserved("vco-recon-missed-foreign");
    let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
    let rows = || {
        vec![
            fill_row("binance", "BTCUSDT", "t-stranger", Some("stranger"), NOW - 1_000),
            fill_row("binance", "BTCUSDT", "t-venue-id", None, NOW - 1_000),
            fill_row("binance", "BTCUSDT", "t-early", Some(&order_client_id), LINK_START - 1),
        ]
    };

    binance_pass(&mut c, rows());
    binance_pass(&mut c, rows());

    assert_eq!(binance_position(&c), 0.0, "nothing booked");
    let missing =
        c.reconcile_alerts.values().filter(|h| h.kind == ReconcileDivergenceKind::MissingFill);
    assert_eq!(missing.count(), 3, "all three held for the operator: {:?}", held_kinds(&c));
}

/// The signed fill legs the held `PositionDrift` proposes.
fn drift_legs(c: &CoreThread<RecordingClient>) -> Vec<f64> {
    let drift = c
        .reconcile_alerts
        .values()
        .find(|h| h.kind == ReconcileDivergenceKind::PositionDrift)
        .expect("the drift is held");
    drift
        .proposed_events
        .iter()
        .filter_map(|e| match e {
            vike_model::events::Event::Fill(f) => Some(f64::from(f.side) * f.last_qty),
            _ => None,
        })
        .collect()
}

/// THE DUST LEG (decision 0043 as amended 2026-11-22): a drift netted against a missed fill of the same pass proposed
/// the float residue of the netting as a leg (bybit#DEMO's `0.249 − 0.001 − 0.2480000000000002`, about −1.9e-16). The
/// book holds `0.1 + 0.2` (`0.30000000000000004`), the venue 0.7 and a missed fill of 0.4 that names no order (held):
/// under the symbol's lot of 0.001 the residue is no leg, so the held drift proposes none. A residual of whole lots
/// (the venue at 0.75) keeps its leg of 0.05.
#[test]
fn a_drift_a_missed_fill_explains_proposes_no_dust_leg() {
    let held_legs = |venue_qty: f64| {
        let mut c = core();
        c.engine.gate.limits.lot_size = Some(0.001);
        c.publish_to(0, bare_fill("t-1", 1, 0.1, 100.0));
        c.publish_to(0, bare_fill("t-2", 1, 0.2, 100.0));
        let local = held_quantity(&c, SYMBOL);
        let missed = vike_model::FillReport {
            last_qty: 0.4,
            ..fill_row(VENUE, SYMBOL, "t-missed", None, 5)
        };
        let reports = ReconcileReports {
            fills: vec![missed],
            ..pass(vec![position(SYMBOL, venue_qty, 1_000)])
        };
        tick_held(&mut c, 1_000, reports);
        assert_eq!(
            held_kinds(&c),
            vec![ReconcileDivergenceKind::MissingFill, ReconcileDivergenceKind::PositionDrift]
        );
        (local, drift_legs(&c))
    };

    let (local, dust) = held_legs(0.7);
    assert_ne!(0.7 - 0.4 - local, 0.0, "PREMISE: the netting leaves a float residue");
    assert!(dust.is_empty(), "no dust leg: {dust:?}");
    let (_, real) = held_legs(0.75);
    assert_eq!(real.len(), 1, "{real:?}");
    assert!((real[0] - 0.05).abs() < 1e-12, "a residual of whole lots keeps its leg: {real:?}");
}

/// One `binance` pass under `hybrid` reporting `fills` and the position `qty`.
fn binance_hybrid_pass(
    c: &mut CoreThread<RecordingClient>,
    fills: Vec<vike_model::FillReport>,
    qty: f64,
) {
    let row = vike_model::PositionStatusReport {
        venue: "binance".into(),
        ..position("BTCUSDT", qty, NOW)
    };
    c.reconcile_reports(ReconcileReports {
        venue: "binance".into(),
        fills,
        policy: vike_execution::recon::ReconcilePolicy::of(vike_model::ReconcilePolicyName::Hybrid),
        ..pass(vec![row])
    });
    c.dispatch_applied_fills();
}

/// THE `hybrid` DOUBLE COUNT (decision 0043 as amended 2026-11-22): the venue's position report shows our fill one
/// pass before its history lists it, so the drift is confirmed (passes 1 and 2) while the fill is pending (pass 2
/// only). The drift's correction used to fold on pass 2 and the fill on pass 3: 1.0 twice, until later passes undid it.
/// Now the pending fill nets the drift: nothing is corrected on pass 2 and the fill folds once, on pass 3. A drift
/// beyond the fill (the venue at 1.75) still has its residual of 0.25 corrected on pass 2.
#[test]
fn a_drift_a_pending_fill_explains_is_not_corrected_under_hybrid() {
    let run = |name: &str, venue_qty: f64| {
        let dir = Scratch::reserved(name);
        let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
        binance_open(&mut c, 0.5);
        let row =
            || fill_row("binance", "BTCUSDT", "t-missed", Some(&order_client_id), NOW - 1_000);
        binance_hybrid_pass(&mut c, Vec::new(), venue_qty); // the history does not list it yet
        let mut sizes = vec![binance_position(&c)];
        for _ in 0..3 {
            binance_hybrid_pass(&mut c, vec![row()], venue_qty);
            sizes.push(binance_position(&c));
        }
        sizes
    };

    assert_eq!(
        run("vco-recon-hybrid-pending-fill", 1.5),
        vec![0.5, 0.5, 1.5, 1.5],
        "no correction beside the pending fill, and the fill once"
    );
    assert_eq!(
        run("vco-recon-hybrid-pending-fill-residual", 1.75),
        vec![0.5, 0.75, 1.75, 1.75],
        "the residual beyond the fill is still corrected"
    );
}

/// THE DOUBLE FEE OF THE FIRST SYNC (decision 0043 as amended 2026-11-22): a restart's first pass adopts the venue's
/// cash whole (2_000), read after a history that lists our fill the book missed (a buy of 1 at 100, fee 5, closing a
/// short of 1 at 110), so that figure already paid the fee and holds the PnL; the next pass books the fill. It used to
/// charge the fee again (1_995) and add the PnL again to what the cash reconcile expects, a `BalanceDrift` of both. Now
/// the booking moves the position and the PnL and not the cash, and nothing is held or pending. A real cash gap beside
/// it (the venue at 1_900 from the second pass) is still held.
#[test]
fn a_fill_the_first_sync_cash_already_holds_is_booked_without_its_cash() {
    let run = |name: &str, later_cash: f64| {
        let dir = Scratch::reserved(name);
        let (order_client_id, mut c) = binance_mount_with_a_working_order(&dir);
        let vike_model::events::Event::Fill(open) = bare_fill("t-open", -1, 1.0, 110.0) else {
            unreachable!("bare_fill is a fill")
        };
        let open = vike_model::events::FillEvent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            ..open
        };
        c.publish_to(0, vike_model::events::Event::Fill(open));
        let row = || vike_model::FillReport {
            commission: 5.0,
            ..fill_row("binance", "BTCUSDT", "t-missed", Some(&order_client_id), NOW - 1_000)
        };

        binance_pass_at(&mut c, vec![row()], 0.0, Some(2_000.0)); // the first sync
        assert_eq!(c.engine.account.balance, 2_000.0, "adopted whole");
        binance_pass_at(&mut c, vec![row()], 0.0, Some(later_cash));
        assert!(c.recent.iter().any(|l| l.contains("RECON booked 1 fill(s)")), "{:?}", c.recent);
        assert_eq!(binance_position(&c), 0.0, "the fill closed the short");
        assert!(c.engine.account.realized_pnl > 0.0, "and booked its PnL");
        assert_eq!(c.engine.account.balance, 2_000.0, "the adopted cash already paid its fee");
        binance_pass_at(&mut c, vec![row()], 0.0, Some(later_cash));
        (dir, c)
    };

    let (_dir, c) = run("vco-recon-first-sync-fee", 2_000.0);
    assert_eq!(c.engine.account.balance, 2_000.0);
    assert!(held_book_kinds(&c).is_empty(), "{:?}", held_kinds(&c));
    let streak = &c.reconcile_pass["binance"].streak;
    assert!(!streak.pending(book_kind), "no gap of the book is pending: {streak:?}");

    let (_dir, c) = run("vco-recon-first-sync-real-gap", 1_900.0);
    assert_eq!(held_book_kinds(&c), vec![ReconcileDivergenceKind::BalanceDrift], "a real gap");
    assert_eq!(c.engine.account.balance, 2_000.0, "the local figure: the fee and the PnL once");
}
