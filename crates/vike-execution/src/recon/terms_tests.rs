//! `diff_order_terms` and its resolution: what is compared, on the grid, and what a held row proposes.

use std::assert_matches;
use std::collections::HashSet;

use indexmap::IndexMap;
use vike_model::events::{Event, PositionSide};
use vike_model::{
    Grid, OrderComboLeg, OrderRecord, OrderRequest, OrderStatus, OrderStatusReport, OrderType,
};

use super::super::resolve::resolve;
use super::super::types::{
    ReconcileDivergence, ReconcileLocalCash, ReconcileLocalView, ReconcileMode, ReconcilePolicy,
};
use super::diff_order_terms;
use vike_model::{ReconcileDivergenceKind, ReconcilePolicyName};

const TICK: f64 = 0.0001;
const LOT: f64 = 0.01;

/// How one call of `diff_order_terms` is set up: the grid, the venue's size word, a settling amend.
#[derive(Clone, Copy)]
struct Setup {
    tick: Option<f64>,
    lot: Option<f64>,
    quantity_is_total: bool,
    settling: bool,
}

const PLAIN: Setup =
    Setup { tick: Some(TICK), lot: Some(LOT), quantity_is_total: true, settling: false };

/// A live local order of `kind` resting at `price` / `trigger`.
fn local(kind: OrderType, price: Option<f64>, trigger: Option<f64>) -> OrderRecord {
    let mut mo = OrderRecord::new(OrderRequest {
        order_client_id: "c1".into(),
        venue: "ig".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1.0,
        order_type: kind,
        price,
        trigger_price: trigger,
        ..Default::default()
    });
    mo.status = OrderStatus::Accepted;
    mo
}

/// The venue's open-order row of `c1`, stating `price` / `trigger`.
fn row(kind: OrderType, price: Option<f64>, trigger: Option<f64>) -> OrderStatusReport {
    OrderStatusReport {
        venue: "ig".into(),
        symbol: "EURUSD".into(),
        venue_order_id: "D1".into(),
        order_client_id: Some("c1".into()),
        side: 1,
        order_type: kind,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: OrderStatus::Accepted,
        timestamp: 7,
        price,
        trigger_price: trigger,
        trail: None,
        trail_percent: None,
    }
}

/// `diff_order_terms` over one local order and one venue row, under [`PLAIN`].
fn terms(mo: OrderRecord, r: OrderStatusReport) -> Vec<ReconcileDivergence> {
    terms_with(mo, r, PLAIN)
}

fn terms_with(mo: OrderRecord, r: OrderStatusReport, s: Setup) -> Vec<ReconcileDivergence> {
    let grid = Grid { tick_size: s.tick, lot_size: s.lot, min_notional: None, min_qty: None };
    let orders = IndexMap::from([("c1".to_string(), mo)]);
    let seen = HashSet::new();
    let positions: IndexMap<(String, PositionSide), f64> = IndexMap::new();
    let view = ReconcileLocalView {
        venue: "ig",
        orders: &orders,
        seen_trade_ids: &seen,
        positions: &positions,
        quantity_tolerance: 1e-9,
        cash: ReconcileLocalCash::default(),
    };
    diff_order_terms(&[r], &view, &|_| grid, s.quantity_is_total, &|_| s.settling)
}

/// [`row`] with the venue's TOTAL size `qty`, `filled` of it executed.
fn sized_row(qty: f64, filled: f64) -> OrderStatusReport {
    OrderStatusReport { qty, filled_qty: filled, ..row(OrderType::Limit, Some(0.948), None) }
}

/// IG5, measured: the book kept 0.948 while IG rested the order at 0.9196. Base: no divergence at all.
#[test]
fn a_lost_amend_is_a_terms_drift_naming_both_prices() {
    let d = terms(
        local(OrderType::Limit, Some(0.948), None),
        row(OrderType::Limit, Some(0.9196), None),
    );
    assert_eq!(
        d,
        vec![ReconcileDivergence::OrderTermsDrift {
            venue: "ig".into(),
            symbol: "EURUSD".into(),
            order_client_id: "c1".into(),
            price: Some((0.948, 0.9196)),
            trigger_price: None,
            qty: None,
            timestamp: 7,
        }]
    );
    assert_eq!(d[0].kind(), ReconcileDivergenceKind::OrderTermsDrift);
    assert!(d[0].describe().contains("price local=0.948 venue=0.9196"), "{}", d[0].describe());
    assert_eq!(d[0].identity_detail(), "ig EURUSD order_client_id=c1", "no number in the identity");
}

/// The grid, never a raw float: the same price once on the tick, or within half a tick, is no drift.
#[test]
fn prices_are_compared_on_the_tick() {
    let limit = |l: f64, v: f64| {
        terms(local(OrderType::Limit, Some(l), None), row(OrderType::Limit, Some(v), None))
    };
    assert!(limit(0.9196, 0.9196).is_empty());
    assert!(limit(0.9196, 0.919_600_000_000_1).is_empty(), "a float re-encoding");
    assert!(limit(0.9196, 0.919_64).is_empty(), "within half a tick");
    assert_eq!(limit(0.9196, 0.9198).len(), 1, "two ticks apart");
    // No tick known: nothing is compared.
    let none = terms_with(
        local(OrderType::Limit, Some(0.948), None),
        row(OrderType::Limit, Some(0.9196), None),
        Setup { tick: None, ..PLAIN },
    );
    assert!(none.is_empty());
}

/// A lost SIZE amend is the same failure: the book holds 1, the venue's TOTAL is 2. On the lot, and its
/// adoption is the same `OrderModified`, a TOTAL.
#[test]
fn a_lost_size_amend_is_a_terms_drift_on_the_lot_and_its_event_sets_the_total() {
    let lost = terms(local(OrderType::Limit, Some(0.948), None), sized_row(2.0, 0.0));
    assert_matches!(
        lost.as_slice(),
        [ReconcileDivergence::OrderTermsDrift {
            price: None,
            trigger_price: None,
            qty: Some((1.0, 2.0)),
            ..
        }]
    );
    assert!(lost[0].describe().contains("qty local=1 venue=2"), "{}", lost[0].describe());
    let r = resolve(lost, &ReconcilePolicy::default(), None, None);
    let mut mo = local(OrderType::Limit, Some(0.948), None);
    mo.apply(&r.events[0]).expect("an Accepted order takes it");
    assert_eq!(mo.request.qty, 2.0);
    assert_eq!(mo.request.price, Some(0.948), "the agreeing price stays");

    let sized = |q: f64| terms(local(OrderType::Limit, Some(0.948), None), sized_row(q, 0.0));
    assert!(sized(1.004).is_empty(), "within half a lot");
    assert!(sized(1.000_000_000_1).is_empty(), "a float re-encoding");
    assert_eq!(sized(1.02).len(), 1, "two lots apart");
    // A trailing kind's size IS an amend's to move (its prices are not compared).
    let trailing = terms(local(OrderType::Trailing, None, Some(1.10)), sized_row(2.0, 0.0));
    assert_matches!(
        trailing.as_slice(),
        [ReconcileDivergence::OrderTermsDrift { qty: Some(_), .. }]
    );
}

/// The size is compared only where the venue's row states the order's TOTAL, there is a lot, the row
/// states a size, no fill is on its way, and the order is not a combo.
#[test]
fn a_size_is_not_compared_on_a_remaining_row_mid_fill_or_on_a_combo() {
    let order = || local(OrderType::Limit, Some(0.948), None);
    let skip = |mo: OrderRecord, r: OrderStatusReport, s: Setup, why: &str| {
        assert!(terms_with(mo, r, s).is_empty(), "{why}");
    };
    skip(
        order(),
        sized_row(2.0, 0.0),
        Setup { quantity_is_total: false, ..PLAIN },
        "a remaining row",
    );
    skip(order(), sized_row(2.0, 0.0), Setup { lot: None, ..PLAIN }, "no lot");
    skip(order(), sized_row(0.0, 0.0), PLAIN, "a row that states no size");
    skip(order(), sized_row(2.0, 0.5), PLAIN, "the venue executed what the book has not folded");
    let mut combo = order();
    combo.request.combo_legs = vec![OrderComboLeg { symbol: "LEG".into(), ratio: 1, limit: None }];
    skip(combo, sized_row(2.0, 0.0), PLAIN, "a combo's size is in combo units");
    // Both sides executed the same part: compared.
    let mut part = order();
    part.filled_qty = 0.5;
    part.status = OrderStatus::PartiallyFilled;
    assert_eq!(terms(part, sized_row(2.0, 0.5)).len(), 1);
}

/// Only a term the venue STATES and the local order rests on, only where `OrderModified` lands it.
#[test]
fn only_a_stated_term_the_order_rests_on_is_compared() {
    // The venue states no price: not compared.
    assert!(
        terms(local(OrderType::Limit, Some(0.948), None), row(OrderType::Limit, None, None))
            .is_empty()
    );
    // A stop rests on its trigger: its trigger drifts, a price the row also states is not read.
    let stop = terms(
        local(OrderType::Stop, None, Some(1.10)),
        row(OrderType::Stop, Some(1.20), Some(1.12)),
    );
    assert_matches!(
        stop.as_slice(),
        [ReconcileDivergence::OrderTermsDrift {
            price: None,
            trigger_price: Some((1.10, 1.12)),
            ..
        }]
    );
    // A stop-limit rests on both.
    let both = terms(
        local(OrderType::StopLimit, Some(1.09), Some(1.10)),
        row(OrderType::StopLimit, Some(1.08), Some(1.11)),
    );
    assert_matches!(
        both.as_slice(),
        [ReconcileDivergence::OrderTermsDrift {
            price: Some((1.09, 1.08)),
            trigger_price: Some((1.10, 1.11)),
            ..
        }]
    );
}

/// The three races and kinds the brief excludes: a trailing kind's activation, an amend in flight or
/// just answered, and an order the FSM would not let an `OrderModified` touch.
#[test]
fn a_trailing_kind_a_settling_amend_and_an_unmodifiable_order_are_not_compared() {
    assert!(
        terms(
            local(OrderType::Trailing, None, Some(1.10)),
            row(OrderType::Trailing, None, Some(1.30)),
        )
        .is_empty(),
        "a trailing kind's trigger is its activation"
    );
    let settling = terms_with(
        local(OrderType::Limit, Some(0.948), None),
        sized_row(2.0, 0.0),
        Setup { settling: true, ..PLAIN },
    );
    assert!(settling.is_empty(), "an amend in flight races the row");
    let mut submitted = local(OrderType::Limit, Some(0.948), None);
    submitted.status = OrderStatus::Submitted;
    assert!(terms(submitted, row(OrderType::Limit, Some(0.9196), None)).is_empty());
    let mut done = row(OrderType::Limit, Some(0.9196), None);
    done.status = OrderStatus::Canceled;
    assert!(terms(local(OrderType::Limit, Some(0.948), None), done).is_empty());
}

/// Held under `quarantine` with ONE `OrderModified` to the venue's terms proposed (what a confirm
/// folds); folded at once under `synthesize`; held under `hybrid`.
#[test]
fn a_terms_drift_proposes_one_order_modified_and_follows_the_policy() {
    let d = terms(
        local(OrderType::StopLimit, Some(1.09), Some(1.10)),
        row(OrderType::StopLimit, Some(1.08), Some(1.10)),
    );
    let quarantine = ReconcilePolicy::of(ReconcilePolicyName::Quarantine);
    let held = resolve(d.clone(), &quarantine, None, None);
    assert!(held.events.is_empty());
    assert_eq!(held.alerts.len(), 1);
    assert_eq!(held.alerts[0].kind, ReconcileDivergenceKind::OrderTermsDrift);
    match held.alerts[0].proposed_events.as_slice() {
        [Event::OrderModified(m)] => {
            assert_eq!(m.order_client_id, "c1");
            assert_eq!(m.new_price, Some(1.08));
            assert_eq!(m.new_trigger_price, None, "an agreeing term is left where it is");
            assert_eq!(m.new_qty, None);
            assert_eq!(m.venue_order_id, None, "the venue id is not rewritten");
        }
        other => panic!("expected one OrderModified, got {other:?}"),
    }
    let synthesize = ReconcilePolicy::of(ReconcilePolicyName::Synthesize);
    let folded = resolve(d.clone(), &synthesize, None, None);
    assert_eq!(folded.events, held.alerts[0].proposed_events);
    assert!(folded.alerts.is_empty());
    assert_eq!(
        ReconcilePolicy::hybrid().mode_for(ReconcileDivergenceKind::OrderTermsDrift),
        ReconcileMode::Quarantine
    );
    assert_eq!(resolve(d, &ReconcilePolicy::hybrid(), None, None).alerts.len(), 1);
}

/// The adoption event lands where it was compared: a stop's trigger moves, its price stays.
#[test]
fn the_proposed_event_moves_a_stops_trigger() {
    let d = terms(local(OrderType::Stop, None, Some(1.10)), row(OrderType::Stop, None, Some(1.12)));
    let r = resolve(d, &ReconcilePolicy::default(), None, None);
    let mut mo = local(OrderType::Stop, None, Some(1.10));
    mo.apply(&r.events[0]).expect("an Accepted order takes it");
    assert_eq!(mo.request.trigger_price, Some(1.12));
    assert_eq!(mo.request.price, None);
}
