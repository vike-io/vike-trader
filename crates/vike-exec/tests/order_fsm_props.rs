//! Property suite over the `ManagedOrder` FSM -- the random-walk twin of the r5 fixture replay
//! (`crates/vike-exec/tests/parity/r5_parity.rs`) and of the hand-picked pins in
//! `crates/vike-exec/src/order.rs`'s `state_set_tests`. The subject is that module's `apply`:
//! the ONE mutator of order state.
//!
//! Six laws, folded over arbitrary lifecycle-event sequences: terminal absorption, Err purity
//! (a refused apply never mutates), fill monotonicity + VWAP boundedness, at-most-one
//! live->terminal transition, the Liquidated trap (non-terminal AND not live -- the pinned
//! disagreement), and `resolve_pending_cancel`'s fill-derived restore. Plus a
//! generator-coverage witness: a property over an arm the generator never reaches is vacuously
//! green, so the witness fails BY NAME when a lifecycle variant goes missing from `arb_event`.
//!
//! FAILURE POLICY: a counterexample here is a REAL FSM finding. STOP AND REPORT with the seed
//! proptest prints; commit the generated `proptest-regressions/` sidecar (workspace convention --
//! the proptest rationale comment in the root manifest); never adjust the FSM to satisfy a
//! property without an explicit decision -- the transition table is fixture-pinned (r5
//! `fsm.json`) and must not be widened.
//!
//! Own test binary ON PURPOSE: property suites stay standalone under the grouping rule
//! (`crates/vike-backtest/CLAUDE.md`) -- a proptest regression sidecar disqualifies a file from
//! a grouped binary.

use proptest::prelude::*;
use vike_exec::{ManagedOrder, OrderStatus};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderDenied, OrderExpired,
    OrderFilled, OrderLiquidated, OrderModified, OrderModifyRejected, OrderPartiallyFilled,
    OrderRejected, OrderSubmitted, OrderTriggered,
};
use vike_model::OrderRequest;

/// The one client order id every generated event carries. The suite drives a SINGLE order --
/// routing by id is the engine's business, not the FSM's (`apply` reads the id only to label
/// its error).
const COID: &str = "prop-1";

fn fill(trade_id: u32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        trade_id: vike_model::events::TradeId::prefixed("T", trade_id),
        client_order_id: COID.to_string(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

// One constructor per lifecycle variant, shared by the strategy, the deterministic drivers and
// the coverage witness's exemplar list.

fn submitted() -> Event {
    Event::OrderSubmitted(OrderSubmitted { client_order_id: COID.into(), ts: 0 })
}

fn accepted(void: Option<u32>) -> Event {
    Event::OrderAccepted(OrderAccepted {
        client_order_id: COID.into(),
        venue_order_id: void.map(|v| format!("V{v}").into()),
        ts: 0,
    })
}

fn rejected() -> Event {
    Event::OrderRejected(OrderRejected {
        client_order_id: COID.into(),
        reason: "prop".into(),
        ts: 0,
    })
}

fn denied() -> Event {
    Event::OrderDenied(OrderDenied { client_order_id: COID.into(), reason: "prop".into(), ts: 0 })
}

fn triggered() -> Event {
    Event::OrderTriggered(OrderTriggered { client_order_id: COID.into(), ts: 0 })
}

fn partially_filled(trade_id: u32, qty: f64, px: f64) -> Event {
    Event::OrderPartiallyFilled(OrderPartiallyFilled {
        client_order_id: COID.into(),
        fill: fill(trade_id, qty, px),
        ts: 0,
    })
}

fn filled(trade_id: u32, qty: f64, px: f64) -> Event {
    Event::OrderFilled(OrderFilled {
        client_order_id: COID.into(),
        fill: fill(trade_id, qty, px),
        ts: 0,
    })
}

fn canceled() -> Event {
    Event::OrderCanceled(OrderCanceled {
        client_order_id: COID.into(),
        reason: "prop".into(),
        ts: 0,
    })
}

fn expired() -> Event {
    Event::OrderExpired(OrderExpired { client_order_id: COID.into(), ts: 0 })
}

fn liquidated(liq_price: f64) -> Event {
    Event::OrderLiquidated(OrderLiquidated { client_order_id: COID.into(), liq_price, ts: 0 })
}

fn modified(new_qty: Option<f64>, new_price: Option<f64>, void: Option<u32>) -> Event {
    Event::OrderModified(OrderModified {
        client_order_id: COID.into(),
        venue_order_id: void.map(|v| format!("V{v}").into()),
        new_qty,
        new_price,
        ts: 0,
    })
}

fn cancel_rejected() -> Event {
    Event::OrderCancelRejected(OrderCancelRejected {
        client_order_id: COID.into(),
        reason: "prop".into(),
        ts: 0,
    })
}

fn modify_rejected() -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        client_order_id: COID.into(),
        reason: "prop".into(),
        ts: 0,
    })
}

/// Mirrors the conformance harness's limit-order request shape
/// (`crates/vike-bridge-core/tests/bridge_conformance.rs`'s `limit_order`).
fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(50_000.0),
        ..Default::default()
    }
}

/// Short display name for failure messages. The crate-private `event_name` in
/// `crates/vike-exec/src/order.rs` is not exported; this local copy names only what `arb_event`
/// can produce.
fn name_of(ev: &Event) -> &'static str {
    match ev {
        Event::OrderSubmitted(_) => "OrderSubmitted",
        Event::OrderAccepted(_) => "OrderAccepted",
        Event::OrderRejected(_) => "OrderRejected",
        Event::OrderDenied(_) => "OrderDenied",
        Event::OrderTriggered(_) => "OrderTriggered",
        Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
        Event::OrderFilled(_) => "OrderFilled",
        Event::OrderCanceled(_) => "OrderCanceled",
        Event::OrderExpired(_) => "OrderExpired",
        Event::OrderLiquidated(_) => "OrderLiquidated",
        Event::OrderModified(_) => "OrderModified",
        Event::OrderCancelRejected(_) => "OrderCancelRejected",
        Event::OrderModifyRejected(_) => "OrderModifyRejected",
        _ => "non-lifecycle",
    }
}

/// The wrapped fill of a fill event, if any. The two wrap variants hold DIFFERENT payload
/// structs (`OrderPartiallyFilled` vs `OrderFilled`), so an or-pattern cannot bind across them.
fn wrapped_fill(ev: &Event) -> Option<&FillEvent> {
    match ev {
        Event::OrderPartiallyFilled(w) => Some(&w.fill),
        Event::OrderFilled(w) => Some(&w.fill),
        _ => None,
    }
}

/// Every `OrderStatus` variant, for the exhaustive `resolve_pending_cancel` no-op sweep --
/// mirrors the `ALL` roster in `crates/vike-exec/src/order.rs`'s `state_set_tests`.
const ALL_STATUSES: &[OrderStatus] = &[
    OrderStatus::Initialized,
    OrderStatus::Submitted,
    OrderStatus::Accepted,
    OrderStatus::Triggered,
    OrderStatus::PartiallyFilled,
    OrderStatus::Filled,
    OrderStatus::Canceled,
    OrderStatus::Rejected,
    OrderStatus::Denied,
    OrderStatus::Expired,
    OrderStatus::PendingCancel,
    OrderStatus::Liquidated,
    OrderStatus::Emulated,
    OrderStatus::Released,
];

/// Uniform over the 13 lifecycle variants `ManagedOrder::apply` handles -- the 10
/// transition-table rows plus `OrderModified` and the two advisories (`OrderCancelRejected`,
/// `OrderModifyRejected`). `prop_oneof!` compiles at most 10 arms (`TupleUnion`'s arity cap),
/// so the 13 split into a 7-arm and a 6-arm half, outer-weighted 7:6 to keep every variant at
/// probability 1/13.
fn arb_event() -> impl Strategy<Value = Event> {
    let fill_params = (any::<u32>(), 0.0f64..2.0, 1.0f64..100.0);
    prop_oneof![
        7 => prop_oneof![
            Just(submitted()),
            proptest::option::of(any::<u32>()).prop_map(accepted),
            Just(rejected()),
            Just(denied()),
            Just(triggered()),
            fill_params.clone().prop_map(|(t, q, p)| partially_filled(t, q, p)),
            fill_params.prop_map(|(t, q, p)| filled(t, q, p)),
        ],
        6 => prop_oneof![
            Just(canceled()),
            Just(expired()),
            (1.0f64..100.0).prop_map(liquidated),
            (
                proptest::option::of(0.1f64..5.0),
                proptest::option::of(1.0f64..100.0),
                proptest::option::of(any::<u32>()),
            )
                .prop_map(|(q, p, v)| modified(q, p, v)),
            Just(cancel_rejected()),
            Just(modify_rejected()),
        ],
    ]
}

/// GENERATOR-COVERAGE WITNESS. A proptest strategy that never reaches an arm proves nothing --
/// the arm's property is vacuously green (a regex strategy without `(?s)` never made a newline;
/// same failure class). 2048 deterministic samples must produce every one of the 13 lifecycle
/// discriminants; a variant added to `Event` and to `apply` but not to `arb_event` fails here
/// by name.
#[test]
fn the_generator_reaches_every_lifecycle_variant() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    use std::collections::HashSet;
    use std::mem::{discriminant, Discriminant};

    let exemplars: [(&str, Event); 13] = [
        ("OrderSubmitted", submitted()),
        ("OrderAccepted", accepted(Some(1))),
        ("OrderRejected", rejected()),
        ("OrderDenied", denied()),
        ("OrderTriggered", triggered()),
        ("OrderPartiallyFilled", partially_filled(1, 0.5, 10.0)),
        ("OrderFilled", filled(2, 0.5, 10.0)),
        ("OrderCanceled", canceled()),
        ("OrderExpired", expired()),
        ("OrderLiquidated", liquidated(10.0)),
        ("OrderModified", modified(Some(2.0), Some(11.0), None)),
        ("OrderCancelRejected", cancel_rejected()),
        ("OrderModifyRejected", modify_rejected()),
    ];
    let strategy = arb_event();
    let mut runner = TestRunner::deterministic();
    let mut seen: HashSet<Discriminant<Event>> = HashSet::new();
    for _ in 0..2048 {
        let ev = strategy
            .new_tree(&mut runner)
            .expect("arb_event must always build a value tree")
            .current();
        seen.insert(discriminant(&ev));
    }
    for (name, exemplar) in &exemplars {
        assert!(
            seen.contains(&discriminant(exemplar)),
            "arb_event produced no {name} in 2048 deterministic samples -- every property over \
             that arm is vacuous; extend the generator (or its weights) until this passes"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    // LAW 1 -- a terminal status is ABSORBING. After the first Ok transition into a terminal
    // status (`OrderStatus::is_terminal`: Filled/Canceled/Rejected/Denied/Expired), every
    // further lifecycle event must Err and leave the aggregate byte-identical (PartialEq over
    // the whole struct -- status, terms, venue id, fill accumulation).
    #[test]
    fn a_terminal_status_is_absorbing(
        events in proptest::collection::vec(arb_event(), 0..40),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        let mut terminal = false;
        for ev in &events {
            if terminal {
                let before = mo.clone();
                prop_assert!(
                    mo.apply(ev).is_err(),
                    "apply({}) returned Ok from terminal {}",
                    name_of(ev),
                    before.status.as_str()
                );
                prop_assert_eq!(
                    &mo,
                    &before,
                    "a refused apply({}) mutated the aggregate",
                    name_of(ev)
                );
            } else {
                let _ = mo.apply(ev);
                terminal = mo.status.is_terminal();
            }
        }
    }

    // LAW 2 -- an Err from `apply` NEVER mutates: every refusal path (modify while not
    // modifiable, advisory outside its allowed-from set, illegal table edge, non-lifecycle
    // event) must return before touching status, terms, venue id, or fill accumulation.
    #[test]
    fn an_err_apply_never_mutates(
        events in proptest::collection::vec(arb_event(), 0..40),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        for ev in &events {
            let before = mo.clone();
            if mo.apply(ev).is_err() {
                prop_assert_eq!(
                    &mo,
                    &before,
                    "Err apply({}) from {} mutated the aggregate",
                    name_of(ev),
                    before.status.as_str()
                );
            }
        }
    }

    // LAW 4 -- at most ONE Ok transition from a live (non-terminal) status into a terminal one.
    // The FSM's half of the conformance harness's exactly-one-terminal contract
    // (`crates/vike-bridge-core/tests/bridge_conformance.rs`): whether a terminal is REACHED is
    // the engine/venue's business; the FSM alone guarantees it cannot happen twice.
    #[test]
    fn at_most_one_live_to_terminal_transition(
        events in proptest::collection::vec(arb_event(), 0..40),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        let mut live_to_terminal = 0usize;
        for ev in &events {
            let was_terminal = mo.status.is_terminal();
            if mo.apply(ev).is_ok() && !was_terminal && mo.status.is_terminal() {
                live_to_terminal += 1;
            }
        }
        prop_assert!(
            live_to_terminal <= 1,
            "{} live->terminal Ok transitions in one fold (must be <= 1)",
            live_to_terminal
        );
    }

    // LAW 3 -- `filled_qty` is monotone non-decreasing across Ok applies, and after every Ok
    // apply `avg_fill_px` lies within [min, max] of the POSITIVE-qty fill prices folded so far
    // (a zero-qty fill moves nothing; before the first positive fill the VWAP sits at its
    // initial 0.0). The 1e-9-scaled slack is documented ULP headroom for the running-VWAP f64
    // arithmetic (the `scalar_props` idiom in vike-model), NOT a tunable tolerance -- never
    // widen it.
    #[test]
    fn filled_qty_is_monotone_and_vwap_stays_bounded(
        events in proptest::collection::vec(arb_event(), 0..40),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        let mut px_min = f64::INFINITY;
        let mut px_max = f64::NEG_INFINITY;
        let mut positive_fill_folded = false;
        for ev in &events {
            let qty_before = mo.filled_qty;
            if mo.apply(ev).is_ok() {
                prop_assert!(
                    mo.filled_qty >= qty_before,
                    "filled_qty decreased: {} -> {} on {}",
                    qty_before,
                    mo.filled_qty,
                    name_of(ev)
                );
                if let Some(f) = wrapped_fill(ev) {
                    if f.last_qty > 0.0 {
                        px_min = px_min.min(f.last_px);
                        px_max = px_max.max(f.last_px);
                        positive_fill_folded = true;
                    }
                }
                if positive_fill_folded {
                    let slack = 1e-9 * (px_max.abs() + 1.0);
                    prop_assert!(
                        mo.avg_fill_px >= px_min - slack && mo.avg_fill_px <= px_max + slack,
                        "VWAP {} escaped [{}, {}] (ULP slack {})",
                        mo.avg_fill_px,
                        px_min,
                        px_max,
                        slack
                    );
                } else {
                    prop_assert_eq!(
                        mo.avg_fill_px,
                        0.0,
                        "VWAP moved off 0.0 with no positive-qty fill folded"
                    );
                }
            }
        }
    }

    // LAW 5 -- `Liquidated` is a TRAP but NOT a terminal: `is_terminal()` is false (perp
    // force-close is deliberately excluded) while `is_live()` is also false (never worth a
    // cancel/modify) -- the disagreement `state_set_tests` pins -- and every further lifecycle
    // event Errs without mutating. Driven deterministically (submit -> accept -> 0..4 partial
    // fills -> liquidate) so EVERY case exercises the trap, instead of only the rare random
    // walk that happens to reach it.
    #[test]
    fn liquidated_is_a_trap_but_not_terminal(
        fills in proptest::collection::vec((any::<u32>(), 0.0f64..2.0, 1.0f64..100.0), 0..4),
        liq_px in 1.0f64..100.0,
        events in proptest::collection::vec(arb_event(), 0..40),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        mo.apply(&submitted()).expect("INITIALIZED -> SUBMITTED is a table edge");
        mo.apply(&accepted(Some(7))).expect("SUBMITTED -> ACCEPTED is a table edge");
        for (t, q, p) in &fills {
            mo.apply(&partially_filled(*t, *q, *p))
                .expect("partial fill from ACCEPTED/PARTIALLY_FILLED is a table edge");
        }
        mo.apply(&liquidated(liq_px))
            .expect("liquidation from a CAN_RECEIVE_LIQUIDATION status is a table edge");
        prop_assert_eq!(mo.status, OrderStatus::Liquidated);
        prop_assert!(!mo.status.is_terminal(), "Liquidated must stay non-terminal (pinned)");
        prop_assert!(!mo.status.is_live(), "Liquidated must not be live (pinned)");
        for ev in &events {
            let before = mo.clone();
            prop_assert!(
                mo.apply(ev).is_err(),
                "apply({}) escaped the Liquidated trap",
                name_of(ev)
            );
            prop_assert_eq!(
                &mo,
                &before,
                "a refused apply({}) mutated the aggregate",
                name_of(ev)
            );
        }
    }

    // LAW 6 -- `resolve_pending_cancel` is a pure function of (status, filled_qty): from
    // PENDING_CANCEL it returns true and restores the fill-derived live status
    // (PARTIALLY_FILLED iff any qty folded, else ACCEPTED) touching nothing else; from every
    // other status it returns false and mutates nothing. The direct `status` write below is the
    // venue-status-seeded shape (reregister/recon), which is exactly how a live order comes to
    // sit at PENDING_CANCEL -- the fields are pub.
    #[test]
    fn resolve_pending_cancel_restores_the_fill_derived_status(
        fills in proptest::collection::vec((any::<u32>(), 0.0f64..2.0, 1.0f64..100.0), 0..5),
    ) {
        let mut mo = ManagedOrder::new(order(COID));
        mo.apply(&submitted()).expect("INITIALIZED -> SUBMITTED is a table edge");
        mo.apply(&accepted(None)).expect("SUBMITTED -> ACCEPTED is a table edge");
        for (t, q, p) in &fills {
            mo.apply(&partially_filled(*t, *q, *p))
                .expect("partial fill from ACCEPTED/PARTIALLY_FILLED is a table edge");
        }
        mo.status = OrderStatus::PendingCancel;
        let before = mo.clone();
        prop_assert!(mo.resolve_pending_cancel(), "PENDING_CANCEL must restore");
        let expected = if before.filled_qty > 0.0 {
            OrderStatus::PartiallyFilled
        } else {
            OrderStatus::Accepted
        };
        prop_assert_eq!(
            mo.status,
            expected,
            "restored status must derive from filled_qty {}",
            before.filled_qty
        );
        prop_assert_eq!(mo.filled_qty, before.filled_qty, "fill accumulation untouched");
        prop_assert_eq!(mo.avg_fill_px, before.avg_fill_px, "VWAP untouched");
        prop_assert_eq!(&mo.request, &before.request, "resting terms untouched");
        // From every OTHER status: false, and byte-identical.
        for status in ALL_STATUSES {
            if *status == OrderStatus::PendingCancel {
                continue;
            }
            let mut other = before.clone();
            other.status = *status;
            let frozen = other.clone();
            prop_assert!(!other.resolve_pending_cancel(), "no restore from {}", status.as_str());
            prop_assert_eq!(&other, &frozen, "the {} no-op must not mutate", status.as_str());
        }
    }
}
