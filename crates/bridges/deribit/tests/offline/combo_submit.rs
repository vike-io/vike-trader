//! Combo submit path — the OFFLINE half of combo orders PR-4.
//!
//! NO network, NO credentials, NO orders: `DeribitOrderTransport` is constructed pointing at an
//! unroutable host and never `connect()`-ed, so every JSON-RPC call fails immediately at the
//! socket. That is exactly the "dead venue path" the venue-adapter contract cares about, and it
//! lets this file prove the one guarantee that must hold no matter what the venue does:
//!
//! **a combo order NEVER silently vanishes** — `submit_order` emits `OrderSubmitted` synchronously
//! (Rust's half of the emitter split) and then exactly ONE terminal `OrderRejected` when the combo
//! cannot be registered, so the `ManagedOrder` FSM always sees a complete lifecycle.
//!
//! The wire-shape and sign correctness live in `src/combo.rs`'s pure fixture tests; this file is
//! the wiring proof. What is NOT proven anywhere offline: that Deribit accepts these exact frames
//! on a real combo book (see the crate's live-smoke note in the PR).

use serde_json::json;

use vike_bridge_core::rest::VenueRest;
use vike_deribit::client::DeribitRest;
use vike_deribit::transport::DeribitOrderTransport;
use vike_model::events::Event;
use vike_model::{build_combo, ComboLeg, ComboSpec, OrderRequest, SymbolProperties};

/// An unconnected transport: every `call` returns `connect() not called` without touching a socket.
fn dead_rest() -> DeribitRest {
    let tx = DeribitOrderTransport::new("wss://test.invalid", "id", "secret", None);
    let properties =
        SymbolProperties { tick_size: 0.0005, step_size: 0.1, min_qty: 0.1, ..Default::default() };
    DeribitRest::new(tx, "BTC-27MAR26-100000-C", properties, "BTC")
}

fn call_spread_request(coid: &str, net_limit: Option<f64>) -> OrderRequest {
    let spec = ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
            ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
        ],
        net_limit,
        time_in_force: Default::default(),
    };
    build_combo(&spec, coid).expect("the spec is valid")
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "Submitted",
            Event::OrderAccepted(_) => "Accepted",
            Event::OrderRejected(_) => "Rejected",
            _ => "other",
        })
        .collect()
}

/// THE contract test: a combo whose `create_combo` call cannot reach the venue still produces a
/// complete `[Submitted, Rejected]` lifecycle — never a lone `Submitted` that strands the FSM.
#[test]
fn dead_combo_path_synthesizes_a_terminal_rejection() {
    let rest = dead_rest();
    let events = rest.submit_order(&call_spread_request("combo-1", Some(0.015)));

    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    match &events[1] {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "combo-1");
            assert!(
                r.reason.contains("create_combo"),
                "the rejection must name the failing step, got {:?}",
                r.reason
            );
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

/// A CREDIT combo (negative net limit) takes the same dead path and is rejected identically — the
/// negative price must not be special-cased, clamped or panicked on anywhere upstream of the wire.
#[test]
fn credit_combo_dead_path_also_terminates() {
    let rest = dead_rest();
    let req = call_spread_request("combo-credit", Some(-0.02));
    assert_eq!(req.price, Some(-0.02), "build_combo must carry the SIGNED net limit through");
    assert_eq!(kinds(&rest.submit_order(&req)), vec!["Submitted", "Rejected"]);
}

/// A combo MARKET order (`net_limit: None`) likewise terminates.
#[test]
fn market_combo_dead_path_also_terminates() {
    let rest = dead_rest();
    assert_eq!(
        kinds(&rest.submit_order(&call_spread_request("combo-mkt", None))),
        vec!["Submitted", "Rejected"]
    );
}

/// Regression: the SINGLE-LEG path is untouched by the combo branch. An ordinary request (empty
/// `combo_legs`) still emits `[Submitted, Rejected]` on a dead transport, and its rejection comes
/// from the ORDER call, not from `create_combo` — i.e. it never entered the combo branch.
#[test]
fn single_leg_path_is_unchanged() {
    let rest = dead_rest();
    let req: OrderRequest = serde_json::from_value(json!({
        "client_order_id": "plain-1", "venue": "deribit", "symbol": "BTC-27MAR26-100000-C",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 0.05
    }))
    .unwrap();
    assert!(req.combo_legs.is_empty(), "this fixture must NOT be a combo");

    let events = rest.submit_order(&req);
    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    match &events[1] {
        Event::OrderRejected(r) => assert!(
            !r.reason.contains("create_combo"),
            "a non-combo must never touch the combo branch, got {:?}",
            r.reason
        ),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

/// The `VenueCaps` honesty tie (venue_caps.rs: "the bridges are the source of the TRUTH those
/// values are tested against"): deribit declares `supports_combo` — the flip that lets
/// `vike-core`'s combo lowering hand a `ComboSpec` to this adapter instead of synthesizing a
/// terminal reject — and THIS file is the no-network proof the declaration is honest:
/// `submit_order` really routes a non-empty `combo_legs` through the combo branch (the
/// rejections above name `create_combo`, so the branch was entered) rather than putting an
/// empty-symbol order on the wire.
#[test]
fn caps_declare_the_combo_support_this_file_proves() {
    // through the non-const registry (the exact lookup `vike-core`'s lowering performs), then
    // tied back to the crate's own re-exported row
    let caps = vike_model::caps_for("deribit");
    assert!(caps.supports_combo, "deribit's row must declare the wired combo path");
    assert_eq!(caps, vike_deribit::CAPS, "the registry must serve the crate's re-exported row");
}

/// `build_combo` leaves `symbol` EMPTY and the adapter substitutes the resolved combo id — pin that
/// handoff so a future change to either side cannot silently diverge.
#[test]
fn combo_request_arrives_with_an_empty_symbol() {
    let req = call_spread_request("combo-sym", Some(0.01));
    assert!(req.symbol.is_empty(), "the venue adapter resolves the combo instrument name");
    assert_eq!(req.combo_legs.len(), 2);
    assert_eq!(req.qty, 2.0, "qty is combo UNITS");
}
