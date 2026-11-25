//! The local-side ORDER sweep (`diff` step 3 → `Divergence::OrphanLocalOrder`), end-to-end through
//! `run_pass` — the `fetch -> diff -> resolve` composition the offline `ReconcileClientFake` and the
//! live `ReconcileManager` share. The order twin of `reconcile_local_position_sweep.rs`.
//!
//! A live local order the venue never reported surfaces, under `hybrid` and `quarantine`, as ONE
//! aggregated, dedup-keyed, operator-readable alert: detection with no outcome would consume a
//! pass, count as coverage wherever the kind is enumerated, and tell nobody.
//!
//! It deliberately cancels nothing. Everything `resolve` emits is a LOCAL fold
//! (`crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_reports` publishes into the engine and
//! calls no venue), so a synthesized `OrderCanceled` would terminalize an order that may still be
//! RESTING at the venue and leave no managed order to cancel it with. And the evidence is an
//! ABSENCE, equally consistent with a terminal we missed, an order still in flight, and a report
//! that does not echo our client ids. So the divergence folds and proposes ZERO events under every
//! policy; the only decision is no-op-vs-surface (`vike_execution::recon::resolve`'s module doc is the
//! authority).

use vike_execution::recon::{
    ORPHAN_LOCAL_ORDER_KEY, ReconcileClientFake, ReconcileMode, ReconcilePassOutcome,
    ReconcilePolicy, run_pass,
};
use vike_execution::testing::RecordingClient;
use vike_execution::{Engine, EventBus, EventBusOutbox, EventHandler};
use vike_model::ReconcileDivergenceKind;
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatusReport};

use vike_execution::testing::EngineBuilder;

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> Engine<RecordingClient> {
    EngineBuilder { venue: VENUE.into(), symbol: SYMBOL.into(), ..Default::default() }.build()
}

fn req(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    }
}

/// An engine holding N live (ACCEPTED) local orders, seeded through the REAL submit path and the
/// emitter split (`OrderSubmitted` at submit, `OrderAccepted` from the venue), so `diff`'s
/// `!mo.status.is_terminal()` gate sees real resting orders, not a hand-built registry.
fn engine_resting(order_client_ids: &[&str]) -> Engine<RecordingClient> {
    let mut engine_under_test = engine();
    let mut bus = EventBus::new();
    let mut outbox = EventBusOutbox::default();
    for order_client_id in order_client_ids {
        engine_under_test.submit_order(&req(order_client_id), 1, &mut outbox);
        bus.publish(
            Event::OrderSubmitted(OrderSubmitted {
                order_client_id: (*order_client_id).into(),
                timestamp: 0,
            }),
            &mut engine_under_test,
        );
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                order_client_id: (*order_client_id).into(),
                venue_order_id: Some(format!("v-{order_client_id}").into()),
                timestamp: 0,
            }),
            &mut engine_under_test,
        );
    }
    let live = engine_under_test.local_view();
    assert_eq!(
        live.orders.values().filter(|o| !o.status.is_terminal()).count(),
        order_client_ids.len(),
        "fixture precondition: every order really rests live in the local registry"
    );
    engine_under_test
}

/// A venue open-order row that DOES echo an order client id — the shape that makes an order NOT an orphan.
fn order_report(order_client_id: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: format!("v-{order_client_id}").into(),
        order_client_id: Some(order_client_id.into()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 0.0,
        average_price: 0.0,
        status: vike_model::OrderStatus::Accepted,
        timestamp: 9,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

/// One reconcile pass over `engine`'s local view against `client` under `policy`: the pass both sweep
/// suites (`reconcile_local_position_sweep` too) judge.
pub(crate) fn sweep_pass(
    engine: &Engine<RecordingClient>,
    client: &ReconcileClientFake,
    policy: &ReconcilePolicy,
) -> ReconcilePassOutcome {
    let owned = engine.local_view();
    run_pass(client, 0, &owned.as_view(), None, policy, None, false).unwrap()
}

/// The venue's report lists a DIFFERENT order (so the fetch demonstrably works) and none for the
/// resting local order client id: under `hybrid` that surfaces exactly one held, dedup-keyed,
/// operator-readable alert and folds nothing, so the order is never abandoned behind a synthetic
/// cancel.
#[test]
fn venue_report_omitting_a_local_order_raises_a_held_alert_under_hybrid() {
    let mut engine = engine_resting(&["c-mine"]);
    let client =
        ReconcileClientFake { orders: vec![order_report("c-other")], ..Default::default() };

    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());

    assert!(
        pass.events.is_empty(),
        "never synthesizes a cancel (or anything else): {:?}",
        pass.events
    );
    let orphan: Vec<_> = pass
        .alerts
        .iter()
        .filter(|a| a.kind == ReconcileDivergenceKind::OrphanLocalOrder)
        .collect();
    assert_eq!(orphan.len(), 1, "{:?}", pass.alerts);
    assert_eq!(orphan[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(
        orphan[0].proposed_events.is_empty(),
        "investigate-only: nothing for a confirm to fold"
    );
    assert!(orphan[0].recover_orders.is_empty(), "a confirm is a pure acknowledgement");
    assert!(orphan[0].detail.contains("c-mine"), "{}", orphan[0].detail);

    // Folding the pass's (empty) event list leaves the order live and cancellable.
    for e in &pass.events {
        engine.on_event(e, &mut EventBusOutbox::default());
    }
    let after = engine.local_view();
    assert_eq!(
        after.orders.values().filter(|o| !o.status.is_terminal()).count(),
        1,
        "an unreported local order must not be terminalized by the pass"
    );
}

/// NO FALSE POSITIVE: a venue row echoing the local order client id keeps the order off the sweep. The sweep
/// keys on this pass's report order client id set, which is why a venue that does not echo client ids (or
/// echoes them broker-prefixed) orphans a whole book at once;
/// `crates/bridges/binance/src/family/recon.rs`'s `parse_order_row` strips that prefix for this
/// reason.
#[test]
fn a_venue_row_echoing_the_local_order_client_id_is_not_an_orphan() {
    let engine = engine_resting(&["c-mine"]);
    let client = ReconcileClientFake { orders: vec![order_report("c-mine")], ..Default::default() };
    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());
    assert_eq!(pass, ReconcilePassOutcome::default(), "a matched order raises nothing at all");
}

/// THE BOUND, end to end (why it matters: `crates/vike-execution/tests/recon/reconcile_policy_pin.rs`'s
/// `a_whole_book_of_orphans_costs_exactly_one_alert_row`): a whole resting book orphaning at once
/// costs ONE row, with an exact count and a truncated sample.
#[test]
fn a_whole_resting_book_orphaning_at_once_costs_one_alert() {
    let order_client_ids: Vec<String> = (0..20).map(|i| format!("mm-{i:02}")).collect();
    let engine = engine_resting(&order_client_ids.iter().map(String::as_str).collect::<Vec<_>>());
    // An order report that lists a live order but echoes NO client id — the order-client-id-less venue shape.
    let mut blind = order_report("ignored");
    blind.order_client_id = None;
    let client = ReconcileClientFake { orders: vec![blind], ..Default::default() };

    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::hybrid());
    let orphan: Vec<_> = pass
        .alerts
        .iter()
        .filter(|a| a.kind == ReconcileDivergenceKind::OrphanLocalOrder)
        .collect();
    assert_eq!(orphan.len(), 1, "one row for the whole book: {:?}", pass.alerts);
    assert!(
        orphan[0].detail.starts_with("20 live LOCAL order(s)"),
        "the COUNT is exact even though the sample truncates: {}",
        orphan[0].detail
    );
    assert!(orphan[0].detail.contains("more)"), "sample truncated: {}", orphan[0].detail);
}

/// RECURRING PASSES: no events fold, so while the condition persists every pass re-raises the
/// IDENTICAL alert and the runtime refreshes the one held row per (venue, kind) via `dedup_key`.
/// Equal passes make that refresh a true no-op (no ring note, no row churn).
#[test]
fn a_recurring_pass_reproduces_the_identical_dedup_keyed_alert() {
    let engine = engine_resting(&["c-mine"]);
    let client =
        ReconcileClientFake { orders: vec![order_report("c-other")], ..Default::default() };
    let policy = ReconcilePolicy::hybrid();
    let owned = engine.local_view();
    let pass1 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    let pass2 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert_eq!(pass1, pass2, "a recurring pass is identical — the held row refreshes in place");
}

/// The `synthesize` policy's NAMED residual (shared with `OrphanLocalPosition`): with everything on
/// auto-apply the kind folds its (empty) event list, so nothing folds AND nothing alerts; an
/// operator who chose "fold everything, no operator in front of it" opted out of alerts.
#[test]
fn synthesize_policy_is_silent_about_an_unreported_local_order() {
    let engine = engine_resting(&["c-mine"]);
    let client =
        ReconcileClientFake { orders: vec![order_report("c-other")], ..Default::default() };
    let pass = sweep_pass(&engine, &client, &ReconcilePolicy::default());
    assert_eq!(
        pass,
        ReconcilePassOutcome::default(),
        "synthesize: a documented no-op, not an alert"
    );
}

/// `quarantine` holds it exactly like `hybrid`: TWO orphans, ONE dedup-keyed, event-free row (not
/// an un-keyed row per orphan per pass).
///
/// ⚠ The pass also carries an `UnknownOrder` alert, correctly: the fixture's venue row (`c-other`)
/// has no local match. Only orphan rows aggregate, so the assertion filters by kind.
#[test]
fn quarantine_policy_holds_the_same_orphan_alert() {
    let engine = engine_resting(&["c-mine", "c-mine-2"]);
    let client =
        ReconcileClientFake { orders: vec![order_report("c-other")], ..Default::default() };
    let policy = ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() };
    let pass = sweep_pass(&engine, &client, &policy);
    assert!(pass.events.is_empty());
    let orphan: Vec<_> = pass
        .alerts
        .iter()
        .filter(|a| a.kind == ReconcileDivergenceKind::OrphanLocalOrder)
        .collect();
    assert_eq!(orphan.len(), 1, "TWO orphans, ONE row: {:?}", pass.alerts);
    assert_eq!(orphan[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(orphan[0].detail.starts_with("2 live LOCAL order(s)"));
    assert!(
        pass.alerts.iter().any(|a| a.kind == ReconcileDivergenceKind::UnknownOrder),
        "the venue's own unmatched row stays on its own path: {:?}",
        pass.alerts
    );
}

/// A TERMINAL local order is never swept, under any policy (`diff`'s gate is
/// `!mo.status.is_terminal()`); otherwise the sweep would raise the whole day's order history
/// every pass.
#[test]
fn a_terminal_local_order_is_never_swept() {
    let mut engine = engine_resting(&["c-done"]);
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderCanceled(vike_model::events::OrderCanceled {
            order_client_id: "c-done".into(),
            reason: "user".to_string().into(),
            timestamp: 1,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        &mut engine,
    );
    let owned = engine.local_view();
    let pass = run_pass(
        &ReconcileClientFake::default(),
        0,
        &owned.as_view(),
        None,
        &ReconcilePolicy::hybrid(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(pass, ReconcilePassOutcome::default(), "a terminal order is not a live local order");
}
