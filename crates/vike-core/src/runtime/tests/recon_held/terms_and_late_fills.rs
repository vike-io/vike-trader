//! **Two findings of the 2026-11-09 venue fixes, in the shared pass.**
//!
//! - A matched order's TERMS: the pass compared a live order's status and filled qty only, so a lost
//!   amend (IG5: the book kept 0.948, IG rested the order at 0.9196) was never seen. It is an
//!   `OrderTermsDrift` now, held under `quarantine`, and its confirm adopts the venue's price through
//!   the event an amend's answer folds (`OrderModified`).
//! - A LATE fill: a pass that read the venue's fill before the exec stream delivered it raised a
//!   `MissingFill` the book then folded, and the row stood until a confirm or a restart (cTrader,
//!   2026-11-09). The next pass clears it, by the PRESENCE of that very fill in the book
//!   (`CoreThread::clear_folded_missing_fills`), never by absence.

use super::*;
use vike_model::events::{Event, FillEvent, LiquiditySide, OrderModified, PositionSide, TradeId};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, OrderStatusReport, OrderType};

const TICK: f64 = 0.0001;
const LOT: f64 = 0.001;

/// [`core`] with a price tick and a lot, which the terms leg compares on. Its venue (bybit) states an
/// order's TOTAL size on its rows (`vike_venues::venue_amend::report_quantity_is_total`).
fn core_with_tick() -> CoreThread<RecordingClient> {
    let mut c = core();
    c.engine_mut(0).gate.limits.tick_size = Some(TICK);
    c.engine_mut(0).gate.limits.lot_size = Some(LOT);
    c
}

/// Rest a live local order `order_client_id` (venue id `void`) in engine 0's registry.
fn rest(c: &mut CoreThread<RecordingClient>, order_client_id: &str, void: &str, price: f64) {
    let mut mo = OrderRecord::new(OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(price),
        ..Default::default()
    });
    mo.status = OrderStatus::Accepted;
    mo.venue_order_id = Some(void.into());
    c.engine_mut(0).registry.insert(order_client_id.into(), mo);
}

/// The venue's open-order row of `order_client_id`, resting at `price`.
fn venue_row(order_client_id: &str, price: f64) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: "D1".into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        order_type: OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: OrderStatus::Accepted,
        timestamp: 1,
        price: Some(price),
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

/// A pass under the policy an operator NAMES (`quarantine`, the CI box's), carrying `orders` and `fills`.
fn named_pass(
    orders: Vec<OrderStatusReport>,
    fills: Vec<vike_model::FillReport>,
) -> ReconcileReports {
    ReconcileReports {
        orders,
        fills,
        policy: vike_execution::recon::ReconcilePolicy::of(
            vike_model::ReconcilePolicyName::Quarantine,
        ),
        ..pass(Vec::new())
    }
}

fn limit_of(c: &CoreThread<RecordingClient>, order_client_id: &str) -> Option<f64> {
    c.engine(0).registry.get(order_client_id).and_then(|o| o.request.price)
}

// ---------------------------------------------------------------------------------------------
// A. The terms of a matched order.
// ---------------------------------------------------------------------------------------------

/// IG5's shape: held, nothing moved until the confirm, and the confirm rests the book's order at the
/// venue's price. Base: the pass raises nothing, so the premise fails.
#[test]
fn a_lost_amend_is_held_and_its_confirm_adopts_the_venues_price() {
    let mut c = core_with_tick();
    rest(&mut c, "c1", "D1", 0.948);
    tick_held(&mut c, 1_000, named_pass(vec![venue_row("c1", 0.9196)], Vec::new()));

    let id = held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift).expect("the drift is held");
    assert_eq!(limit_of(&c, "c1"), Some(0.948), "quarantine folds nothing by itself");
    let held = &c.reconcile_alerts[&id];
    assert!(held.detail.contains("price local=0.948 venue=0.9196"), "{}", held.detail);

    c.confirm_reconcile(id);
    assert_eq!(limit_of(&c, "c1"), Some(0.9196), "the confirm adopts the venue's terms");
    assert_eq!(
        c.engine(0).registry["c1"].status,
        OrderStatus::Accepted,
        "and the order stays live"
    );

    // The venue and the book agree now: the next pass raises nothing.
    tick(&mut c, 1_000 + PASS_MS, named_pass(vec![venue_row("c1", 0.9196)], Vec::new()));
    assert_eq!(held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift), None);
}

/// A lost SIZE amend: the book holds 1, the venue rests 2. Held, and its confirm sets the book's
/// order to the venue's total.
#[test]
fn a_lost_size_amend_is_held_and_its_confirm_adopts_the_venues_size() {
    let mut c = core_with_tick();
    rest(&mut c, "c1", "D1", 0.9196);
    let row = OrderStatusReport { qty: 2.0, ..venue_row("c1", 0.9196) };
    tick_held(&mut c, 1_000, named_pass(vec![row], Vec::new()));

    let id = held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift).expect("the drift is held");
    assert!(
        c.reconcile_alerts[&id].detail.contains("qty local=1 venue=2"),
        "{}",
        c.reconcile_alerts[&id].detail
    );
    assert_eq!(c.engine(0).registry["c1"].request.qty, 1.0, "quarantine folds nothing by itself");

    c.confirm_reconcile(id);
    assert_eq!(c.engine(0).registry["c1"].request.qty, 2.0, "the confirm adopts the venue's total");
    assert_eq!(limit_of(&c, "c1"), Some(0.9196), "and leaves the agreeing price");
}

/// Equal on the tick is no drift, and a hand-built policy (every pass journaled before the
/// comparison) compares no terms at all.
#[test]
fn the_same_price_on_the_tick_and_a_policy_without_the_key_raise_nothing() {
    let mut c = core_with_tick();
    rest(&mut c, "c1", "D1", 0.9196);
    tick(&mut c, 1_000, named_pass(vec![venue_row("c1", 0.919_64)], Vec::new()));
    assert_eq!(
        held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift),
        None,
        "within half a tick"
    );

    let old = ReconcileReports { orders: vec![venue_row("c1", 0.948)], ..pass(Vec::new()) };
    assert_eq!(old.policy.order_terms, None, "PREMISE: a hand-built policy");
    tick(&mut c, 1_000 + PASS_MS, old);
    assert_eq!(held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift), None);
}

/// An amend answered within the last pass races the venue's row: the book moved to 0.948 a moment
/// ago and the row still says 0.9196. No drift then; a pass after the amend settles raises it.
#[test]
fn an_amend_answered_within_the_last_pass_is_not_a_drift() {
    let mut c = core_with_tick();
    rest(&mut c, "c1", "D1", 0.9196);
    c.engine.now_ms = 10_000;
    c.publish_to(
        0,
        Event::OrderModified(OrderModified {
            order_client_id: "c1".into(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(0.948),
            new_trigger_price: None,
            timestamp: 10_000,
        }),
    );
    assert_eq!(limit_of(&c, "c1"), Some(0.948), "PREMISE: the amend's answer folded");

    tick(&mut c, 10_000 + PASS_MS / 2, named_pass(vec![venue_row("c1", 0.9196)], Vec::new()));
    assert_eq!(
        held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift),
        None,
        "a race, not a drift"
    );

    let settled = 10_000 + vike_execution::execution_engine::AMEND_SETTLE_MS + 1;
    tick_held(&mut c, settled, named_pass(vec![venue_row("c1", 0.9196)], Vec::new()));
    assert!(
        held_id_of(&c, ReconcileDivergenceKind::OrderTermsDrift).is_some(),
        "it outlived the race"
    );
}

// ---------------------------------------------------------------------------------------------
// B. A late fill.
// ---------------------------------------------------------------------------------------------

/// The venue's fill row: trade `tid` of order `void`, echoing `order_client_id`.
fn fill_row(tid: &str, void: &str, order_client_id: Option<&str>) -> vike_model::FillReport {
    vike_model::FillReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        trade_id: TradeId::new(tid).expect("a non-empty trade id"),
        venue_order_id: void.into(),
        order_client_id: order_client_id.map(str::to_string),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 5,
    }
}

/// The exec stream's delivery of an execution: trade `tid` of `order_client_id`, `qty` at `px`.
fn stream_fill(tid: &str, order_client_id: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("a non-empty trade id"),
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: vike_marketdata::Symbol::new("USDT"),
        liquidity_side: LiquiditySide::Taker,
        timestamp: 6,
        mark_price: None,
        position_side: PositionSide::Both,
    })
}

/// cTrader's race, our own order: the pass read the fill first, then the stream delivered it. Base:
/// the row stands (a `MissingFill` is never covered by a pass).
#[test]
fn a_missed_fill_the_book_then_folds_is_cleared_by_the_next_pass() {
    let mut c = core();
    rest(&mut c, "c1", "v9", 100.0);
    let fills = vec![fill_row("t1", "v9", Some("c1"))];
    tick_held(&mut c, 1_000, named_pass(Vec::new(), fills.clone()));
    let missed =
        held_id_of(&c, ReconcileDivergenceKind::MissingFill).expect("PREMISE: raised first");

    c.publish_to(0, stream_fill("t1", "c1", 1.0, 100.0));
    tick(&mut c, 1_000 + PASS_MS, named_pass(Vec::new(), fills));
    assert!(!c.reconcile_alerts.contains_key(&missed), "the book holds that very fill");
    assert!(c.recent.iter().any(|l| l.contains("RECON cleared") && l.contains("MissingFill")));
}

/// The same for an order-client-id-less fill (an order placed elsewhere): its `EXT-` id names the venue order.
#[test]
fn an_order_client_id_less_missed_fill_the_book_then_folds_is_cleared_too() {
    let mut c = core();
    let fills = vec![fill_row("t1", "v9", None)];
    tick_held(&mut c, 1_000, named_pass(Vec::new(), fills.clone()));
    let missed =
        held_id_of(&c, ReconcileDivergenceKind::MissingFill).expect("PREMISE: raised first");

    c.publish_to(
        0,
        stream_fill(
            "t1",
            &vike_execution::recon::external_order_client_id(VENUE, "v9"),
            1.0,
            100.0,
        ),
    );
    tick(&mut c, 1_000 + PASS_MS, named_pass(Vec::new(), fills));
    assert!(!c.reconcile_alerts.contains_key(&missed));
}

/// Presence of THAT fill, never of its id: a colliding trade id that folded another order's fill
/// (another size and price), or the order the book knows under another venue id, keeps the row,
/// the only trace of the fill the ledger refused.
#[test]
fn a_colliding_trade_id_or_another_venue_order_keeps_the_row() {
    let mut c = core();
    rest(&mut c, "c1", "v9", 100.0);
    let fills = vec![fill_row("t1", "v9", Some("c1"))];
    tick_held(&mut c, 1_000, named_pass(Vec::new(), fills.clone()));
    let missed =
        held_id_of(&c, ReconcileDivergenceKind::MissingFill).expect("PREMISE: raised first");

    c.publish_to(0, stream_fill("t1", "c2", 2.0, 101.0));
    tick(&mut c, 1_000 + PASS_MS, named_pass(Vec::new(), fills.clone()));
    assert!(c.reconcile_alerts.contains_key(&missed), "another execution under the same id");

    let mut other = core();
    rest(&mut other, "c1", "v8", 100.0);
    tick_held(&mut other, 1_000, named_pass(Vec::new(), fills.clone()));
    let missed =
        held_id_of(&other, ReconcileDivergenceKind::MissingFill).expect("PREMISE: raised first");
    other.publish_to(0, stream_fill("t1", "c1", 1.0, 100.0));
    tick(&mut other, 1_000 + PASS_MS, named_pass(Vec::new(), fills));
    assert!(
        other.reconcile_alerts.contains_key(&missed),
        "the book knows c1 as another venue order"
    );
}
