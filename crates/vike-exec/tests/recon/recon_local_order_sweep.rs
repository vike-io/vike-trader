//! The local-side ORDER sweep (`diff` step 3 → `Divergence::OrphanLocalOrder`), end-to-end through
//! `run_pass` — the `fetch -> diff -> resolve` composition the offline `FakeReconClient` and the
//! live `ReconManager` share. The order twin of `recon_local_position_sweep.rs`.
//!
//! THE DEFECT THIS CLOSES: the sweep has always DETECTED the divergence and `resolve` then dropped
//! it on the floor under every policy. `events_for` has no arm for the kind, so it folded an empty
//! list; `ReconPolicy::hybrid` classified it auto-apply, so that empty list was "applied" and no
//! alert was raised either; only `quarantine` surfaced it, un-keyed (a fresh row per orphan per
//! pass) with the literal kind name as its whole detail. Detection with no outcome is worse than no
//! detection: it consumes a pass, counts as coverage wherever the kind is enumerated, and tells
//! nobody. Under `hybrid` — the policy an unset `VIKE_RECONCILE_POLICY` gives an operator — a live
//! local order the venue never reported produced NOTHING AT ALL.
//!
//! What this deliberately does NOT do is cancel anything. Everything `resolve` emits is a LOCAL
//! fold (`crates/vike-core/src/runtime/mod.rs`'s `reconcile_reports` publishes into the engine and
//! calls no venue), so a synthesized `OrderCanceled` would terminalize an order that may still be
//! RESTING at the venue and leave no managed order to cancel it with. And the evidence is an
//! ABSENCE: a coid missing from an order report is equally consistent with a terminal we missed, an
//! order still in flight to the venue, and a report that does not echo our client ids at all. So
//! the divergence folds ZERO events under every policy, proposes ZERO events for a confirm, and the
//! only decision left is no-op-vs-surface — see `vike_exec::recon::resolve`'s module doc, the
//! authority.

use vike_exec::recon::{
    DivergenceKind, FakeReconClient, ORPHAN_LOCAL_ORDER_KEY, Recon, ReconMode, ReconPolicy,
    run_pass,
};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventBus, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatusReport};

const VENUE: &str = "sim";
const SYMBOL: &str = "BTCUSDT";

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    )
}

fn req(coid: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// An engine holding N genuinely-live (ACCEPTED) local orders, seeded through the REAL submit path
/// plus the venue-adapter emitter split (`OrderSubmitted` at submit, `OrderAccepted` from the
/// venue) — so `diff`'s `!mo.status.is_terminal()` gate sees real resting orders, not a hand-built
/// registry.
fn engine_resting(coids: &[&str]) -> ExecutionEngine<RecordingClient> {
    let mut eng = engine();
    let mut bus = EventBus::new();
    let mut outbox = Outbox::default();
    for coid in coids {
        eng.submit_order(&req(coid), 1, &mut outbox);
        bus.publish(
            Event::OrderSubmitted(OrderSubmitted { client_order_id: (*coid).into(), ts: 0 }),
            &mut eng,
        );
        bus.publish(
            Event::OrderAccepted(OrderAccepted {
                client_order_id: (*coid).into(),
                venue_order_id: Some(format!("v-{coid}").into()),
                ts: 0,
            }),
            &mut eng,
        );
    }
    let live = eng.local_view();
    assert_eq!(
        live.orders.values().filter(|o| !o.status.is_terminal()).count(),
        coids.len(),
        "fixture precondition: every order really rests live in the local registry"
    );
    eng
}

/// A venue open-order row that DOES echo a coid — the shape that makes an order NOT an orphan.
fn order_report(coid: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        venue_order_id: format!("v-{coid}").into(),
        client_order_id: Some(coid.into()),
        side: 1,
        order_type: "LIMIT".into(),
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: "NEW".into(),
        ts: 9,
    }
}

/// THE FIX. The venue's order report lists a DIFFERENT order (so the fetch demonstrably works),
/// none for the coid local is resting. Under `hybrid` — the production default — that now surfaces
/// exactly one held, dedup-keyed, operator-readable alert, where before it surfaced nothing at all.
/// And it still folds nothing, so the local order is never abandoned behind a synthetic cancel.
#[test]
fn venue_report_omitting_a_local_order_raises_a_held_alert_under_hybrid() {
    let mut eng = engine_resting(&["c-mine"]);
    let client = FakeReconClient { orders: vec![order_report("c-other")], ..Default::default() };

    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();

    assert!(
        pass.events.is_empty(),
        "never synthesizes a cancel (or anything else): {:?}",
        pass.events
    );
    let orphan: Vec<_> =
        pass.alerts.iter().filter(|a| a.kind == DivergenceKind::OrphanLocalOrder).collect();
    assert_eq!(orphan.len(), 1, "{:?}", pass.alerts);
    assert_eq!(orphan[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(
        orphan[0].proposed_events.is_empty(),
        "investigate-only: nothing for a confirm to fold"
    );
    assert!(orphan[0].recover_orders.is_empty(), "a confirm is a pure acknowledgement");
    assert!(orphan[0].detail.contains("c-mine"), "{}", orphan[0].detail);

    // Folding the pass's (empty) event list leaves the order exactly where it was — still live,
    // still cancellable, because nothing terminalized it behind our back.
    for e in &pass.events {
        eng.on_event(e, &mut Outbox::default());
    }
    let after = eng.local_view();
    assert_eq!(
        after.orders.values().filter(|o| !o.status.is_terminal()).count(),
        1,
        "an unreported local order must not be terminalized by the pass"
    );
}

/// NO FALSE POSITIVE. A venue row that DOES echo the local coid keeps the order off the sweep
/// entirely — the sweep keys on the coid set of this pass's order report, which is exactly why a
/// venue that does not echo client ids (or echoes them broker-prefixed) orphans an entire book at
/// once. `crates/bridges/binance/src/family/recon.rs`'s `parse_order_row` strips that prefix for
/// this reason.
#[test]
fn a_venue_row_echoing_the_local_coid_is_not_an_orphan() {
    let eng = engine_resting(&["c-mine"]);
    let client = FakeReconClient { orders: vec![order_report("c-mine")], ..Default::default() };
    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();
    assert_eq!(pass, Recon::default(), "a matched order raises nothing at all");
}

/// THE BOUND that makes surfacing this kind affordable. The held-alert store never self-clears
/// (`crates/vike-core/src/runtime/mod.rs`'s `confirm_recon` is its only remover) and every held row
/// is cloned into every published snapshot (`crates/vike-core/src/runtime/publish.rs`'s
/// `recon_block`), while this divergence heals routinely and its natural key — the coid — is
/// unbounded. So a whole resting book orphaning at once must still cost ONE row, with an exact
/// count and a truncated sample.
#[test]
fn a_whole_resting_book_orphaning_at_once_costs_one_alert() {
    let coids: Vec<String> = (0..20).map(|i| format!("mm-{i:02}")).collect();
    let eng = engine_resting(&coids.iter().map(String::as_str).collect::<Vec<_>>());
    // An order report that lists a live order but echoes NO client id — the coid-less venue shape.
    let mut blind = order_report("ignored");
    blind.client_order_id = None;
    let client = FakeReconClient { orders: vec![blind], ..Default::default() };

    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::hybrid(), None, false).unwrap();
    let orphan: Vec<_> =
        pass.alerts.iter().filter(|a| a.kind == DivergenceKind::OrphanLocalOrder).collect();
    assert_eq!(orphan.len(), 1, "one row for the whole book: {:?}", pass.alerts);
    assert!(
        orphan[0].detail.starts_with("20 live LOCAL order(s)"),
        "the COUNT is exact even though the sample truncates: {}",
        orphan[0].detail
    );
    assert!(orphan[0].detail.contains("more)"), "sample truncated: {}", orphan[0].detail);
}

/// RECURRING PASSES. Nothing about this divergence self-heals from the engine's side (no events
/// fold), so while the condition persists every pass re-raises the IDENTICAL alert — which is what
/// the `dedup_key` is for: the runtime refreshes the one held row per (venue, kind) instead of
/// appending. Pinning the two passes as equal is what makes that refresh a true no-op, with no ring
/// note and no row churn.
#[test]
fn a_recurring_pass_reproduces_the_identical_dedup_keyed_alert() {
    let eng = engine_resting(&["c-mine"]);
    let client = FakeReconClient { orders: vec![order_report("c-other")], ..Default::default() };
    let policy = ReconPolicy::hybrid();
    let owned = eng.local_view();
    let pass1 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    let pass2 = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert_eq!(pass1, pass2, "a recurring pass is identical — the held row refreshes in place");
}

/// The `synthesize` policy's NAMED residual, pinned so it cannot drift silently: with everything on
/// auto-apply the kind folds its (empty) event list, so the pass is a true no-op — nothing folds
/// AND nothing alerts. The same residual `OrphanLocalPosition` carries: an operator who chose "fold
/// everything, no operator in front of it" opted out of alerts.
#[test]
fn synthesize_policy_is_silent_about_an_unreported_local_order() {
    let eng = engine_resting(&["c-mine"]);
    let client = FakeReconClient { orders: vec![order_report("c-other")], ..Default::default() };
    let owned = eng.local_view();
    let pass =
        run_pass(&client, 0, &owned.as_view(), None, &ReconPolicy::default(), None, false).unwrap();
    assert_eq!(pass, Recon::default(), "synthesize: a documented no-op, not an alert");
}

/// `quarantine` holds it exactly like `hybrid` — the same single, dedup-keyed, event-free alert.
/// Before this it raised one UN-KEYED row per orphan per pass, whose entire detail was the literal
/// string `OrphanLocalOrder`: unbounded row growth, and nothing an operator could act on. TWO
/// orphans, ONE row.
///
/// ⚠ The pass carries a SECOND alert of a different kind, and that is correct: the venue row this
/// fixture lists (`c-other`) has no local match, so it is an ordinary `UnknownOrder`, which
/// `quarantine` — and `hybrid` — also hold. Only the orphan rows are aggregated, so the assertion
/// filters by kind rather than counting the whole pass.
#[test]
fn quarantine_policy_holds_the_same_orphan_alert() {
    let eng = engine_resting(&["c-mine", "c-mine-2"]);
    let client = FakeReconClient { orders: vec![order_report("c-other")], ..Default::default() };
    let policy = ReconPolicy { default: ReconMode::Quarantine, ..Default::default() };
    let owned = eng.local_view();
    let pass = run_pass(&client, 0, &owned.as_view(), None, &policy, None, false).unwrap();
    assert!(pass.events.is_empty());
    let orphan: Vec<_> =
        pass.alerts.iter().filter(|a| a.kind == DivergenceKind::OrphanLocalOrder).collect();
    assert_eq!(orphan.len(), 1, "TWO orphans, ONE row: {:?}", pass.alerts);
    assert_eq!(orphan[0].dedup_key.as_deref(), Some(ORPHAN_LOCAL_ORDER_KEY));
    assert!(orphan[0].detail.starts_with("2 live LOCAL order(s)"));
    assert!(
        pass.alerts.iter().any(|a| a.kind == DivergenceKind::UnknownOrder),
        "the venue's own unmatched row stays on its own path: {:?}",
        pass.alerts
    );
}

/// A TERMINAL local order is never swept, under any policy — `diff`'s gate is
/// `!mo.status.is_terminal()`. This is the property that keeps the alert from firing on the
/// ordinary end of every order's life; without it the sweep would raise the whole day's order
/// history every pass.
#[test]
fn a_terminal_local_order_is_never_swept() {
    let mut eng = engine_resting(&["c-done"]);
    let mut bus = EventBus::new();
    bus.publish(
        Event::OrderCanceled(vike_model::events::OrderCanceled {
            client_order_id: "c-done".into(),
            reason: "user".to_string().into(),
            ts: 1,
        }),
        &mut eng,
    );
    let owned = eng.local_view();
    let pass = run_pass(
        &FakeReconClient::default(),
        0,
        &owned.as_view(),
        None,
        &ReconPolicy::hybrid(),
        None,
        false,
    )
    .unwrap();
    assert_eq!(pass, Recon::default(), "a terminal order is not a live local order");
}
