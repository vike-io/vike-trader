//! Combo submit path — the OFFLINE half of combo orders PR-4.
//!
//! NO network, NO credentials, NO orders: `DeribitTransport` is constructed pointing at an
//! unroutable host and never `connect()`-ed, so every JSON-RPC call fails immediately at the
//! socket. That is exactly the "dead venue path" the venue-adapter contract cares about, and it
//! lets this file prove the one guarantee that must hold no matter what the venue does:
//!
//! **a combo order NEVER silently vanishes** — `submit_order` emits `OrderSubmitted` synchronously
//! (Rust's half of the emitter split) and then exactly ONE terminal `OrderRejected` when the combo
//! cannot be registered, so the `OrderRecord` FSM always sees a complete lifecycle.
//!
//! The wire-shape and sign correctness live in `src/execution/combo.rs`'s pure fixture tests; this file is
//! the wiring proof. What is NOT proven anywhere offline: that Deribit accepts these exact frames
//! on a real combo book (see the crate's live-smoke note in the PR).

use serde_json::json;

use vike_bridge_core::rest::VenueRest;
use vike_deribit::execution::rest::VenueRestDeribit;
use vike_deribit::transport::client::DeribitTransport;
use vike_model::events::Event;
use vike_model::{OrderCombo, OrderComboLeg, OrderRequest, SymbolProperties, build_combo};

/// An unconnected transport: every `call` returns `connect() not called` without touching a socket.
fn dead_rest() -> VenueRestDeribit {
    // a combo leg is a USER name (an option's is the venue's own), read off the kept list (decision 0290)
    vike_deribit::names::hold_deribit_kept_rows();
    let tx = DeribitTransport::new("wss://test.invalid", "id", "secret", None);
    let properties =
        SymbolProperties { tick_size: 0.0005, step_size: 0.1, min_qty: 0.1, ..Default::default() };
    VenueRestDeribit::new(tx, "BTC-27MAR26-100000-C", properties, "BTC")
        .with_clock(crate::before_the_captures)
}

fn call_spread_request(order_client_id: &str, net_limit: Option<f64>) -> OrderRequest {
    let spec = OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 2.0,
        legs: vec![
            // two BTC futures by their USER names (decision 0290; a coin-settled option has none): `BTC-26JUN26`
            // and `BTC-27MAR27` on the wire
            OrderComboLeg { symbol: "BTCUSD_260626".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "BTCUSD_270327".into(), ratio: -1, limit: None },
        ],
        net_limit,
        time_in_force: Default::default(),
        account: None,
        order_client_id: None,
    };
    build_combo(&spec, order_client_id).expect("the spec is valid")
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

/// A combo priced by its legs (wave 6: `OrderComboLeg::limit`) reaches this bridge as a `limit` with NO net
/// price, and is refused by name before `create_combo` (`kind_refusal`): the venue takes no leg limit,
/// and the bridge never sends one as a combo market order.
#[test]
fn a_combo_priced_by_its_legs_is_refused_before_create_combo() {
    let mut req = call_spread_request("combo-legs", Some(0.015));
    req.price = None;
    req.combo_legs[0].limit = Some(0.03);
    req.combo_legs[1].limit = Some(0.01);
    let events = dead_rest().submit_order(&req);
    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    let Event::OrderRejected(r) = &events[1] else { unreachable!() };
    assert!(r.reason.contains("names no net price"), "{:?}", r.reason);
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
            assert_eq!(r.order_client_id, "combo-1");
            assert!(
                r.reason.contains("create_combo"),
                "the rejection must name the failing step, got {:?}",
                r.reason
            );
            assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
        }
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

/// **A combo whose legs settle in different coins is refused by name, before `create_combo`** (decision 0258: one
/// engine keeps one settle coin, the venue one wallet per coin): a BTC and an ETH leg on a BTC account, and two ETH
/// legs on it, each a terminal reject naming the leg and both currencies, with nothing registered. A combo of the
/// account's own coin still goes to `create_combo` (the dead path above).
#[test]
fn a_combo_with_a_leg_of_another_coin_is_refused_before_create_combo() {
    let rest = dead_rest();
    for legs in [["BTCUSD_260626", "ETHUSD_260626"], ["ETHUSD_260626", "ETHUSD_270327"]] {
        let spec = OrderCombo {
            venue: "deribit".into(),
            side: 1,
            qty: 2.0,
            legs: vec![
                OrderComboLeg { symbol: legs[0].into(), ratio: 1, limit: None },
                OrderComboLeg { symbol: legs[1].into(), ratio: -1, limit: None },
            ],
            net_limit: Some(0.01),
            time_in_force: Default::default(),
            account: None,
            order_client_id: None,
        };
        let req = build_combo(&spec, "combo-coins").expect("the spec is valid");
        let events = rest.submit_order(&req);
        assert_eq!(kinds(&events), vec!["Submitted", "Rejected"], "{legs:?}");
        let Event::OrderRejected(r) = &events[1] else { panic!("{events:?}") };
        let why = r.reason.to_string();
        assert!(
            why.contains("settles in ETH") && why.contains("settles in BTC"),
            "{legs:?}: {why}"
        );
        assert!(why.contains("ETH-"), "names the leg as the venue spells it: {why}");
        assert!(
            why.contains("every leg of a combo settles in it; refused, nothing sent"),
            "refused before anything is registered: {why}"
        );
        assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
    }
}

/// **A combo leg no kept row names is refused before `create_combo`** (decision 0290): nothing was sent.
#[test]
fn a_combo_leg_no_kept_row_names_is_refused_as_not_sent() {
    let mut req = call_spread_request("combo-unnamed", Some(0.015));
    req.combo_legs[1].symbol = "NOSUCHUSD_270327".into();
    let events = dead_rest().submit_order(&req);
    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    let Event::OrderRejected(r) = &events[1] else { unreachable!() };
    // The name's own refusal (`vike_deribit::names::venue_name_of`), before `create_combo` registers anything.
    assert!(
        r.reason.starts_with("deribit: ") && r.reason.ends_with("; nothing was sent"),
        "{:?}",
        r.reason
    );
    assert_eq!(r.refusal_id.as_deref(), Some(vike_model::OrderRefusalId::NotSent.word()));
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
        "order_client_id": "plain-1", "venue": "deribit", "symbol": "BTC-27MAR26-100000-C",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 0.05
    }))
    .unwrap();
    assert!(req.combo_legs.is_empty(), "this fixture must NOT be a combo");

    let events = rest.submit_order(&req);
    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    match &events[1] {
        Event::OrderRejected(r) => assert_eq!(
            r.reason, "DeribitTransport.connect() not called",
            "a non-combo must never touch the combo branch: the ORDER call's own failure"
        ),
        other => panic!("expected OrderRejected, got {other:?}"),
    }
}

/// The `VenueCapabilities` honesty tie (venue_caps.rs: "the bridges are the source of the TRUTH those
/// values are tested against"): deribit declares `supports_combo` — the flip that lets
/// `vike-core`'s combo lowering hand a `OrderCombo` to this adapter instead of synthesizing a
/// terminal reject — and THIS file is the no-network proof the declaration is honest:
/// `submit_order` really routes a non-empty `combo_legs` through the combo branch (the
/// rejections above name `create_combo`, so the branch was entered) rather than putting an
/// empty-symbol order on the wire.
#[test]
fn capabilities_declare_the_combo_support_this_file_proves() {
    // through the non-const registry (the exact lookup `vike-core`'s lowering performs), then
    // tied back to the crate's own re-exported row
    let capabilities = vike_venues::capabilities_for("deribit");
    assert!(capabilities.supports_combo, "deribit's row must declare the wired combo path");
    assert_eq!(
        capabilities,
        vike_deribit::CAPABILITIES,
        "the registry must serve the crate's re-exported row"
    );
}

/// The call spread's combo book as `create_combo` and `get_combo_details` answer it: trading, and just born.
const ACTIVE: &str = r#"{"id":"FAKE-COMBO","state":"active","legs":[{"instrument_name":"BTC-26JUN26","amount":1},{"instrument_name":"BTC-27MAR27","amount":-1}]}"#;
const INACTIVE: &str = r#"{"id":"FAKE-COMBO","state":"inactive","legs":[{"instrument_name":"BTC-26JUN26","amount":1},{"instrument_name":"BTC-27MAR27","amount":-1}]}"#;

/// The call spread submitted against a fake that answers `answers`: the events, the methods the venue
/// got with when each arrived, and the `info` lines on `vike_deribit::execution::combo`.
fn submit_against(
    answers: &'static [(&'static str, &'static str)],
) -> (Vec<Event>, Vec<(String, std::time::Instant)>, Vec<String>) {
    use crate::fake_deribit_websocket::{FakeDeribit, Session, rest_against};
    let arrived = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let at = std::sync::Arc::clone(&arrived);
    let hook = std::sync::Arc::new(move |method: &str, _: &serde_json::Value| {
        at.lock().unwrap().push((method.to_string(), std::time::Instant::now()));
    });
    vike_deribit::names::hold_deribit_kept_rows();
    let fake = FakeDeribit::spawn_with_hook(vec![Session::Scripted(answers)], hook);
    let rest = rest_against(&fake);
    let (events, lines) = vike_log::capture::captured(|| {
        rest.submit_order(&call_spread_request("combo-fresh", Some(0.015)))
    });
    let infos = lines
        .iter()
        .filter(|line| {
            line.target == "vike_deribit::execution::combo" && line.level == tracing::Level::INFO
        })
        .map(|line| format!("{} {}", line.message, line.field("combo_id").unwrap_or_default()))
        .collect();
    let arrived = arrived.lock().unwrap().clone();
    (events, arrived, infos)
}

/// **A fresh combo book's wait, pinned before its ladder moved to the shared ladder (`vike_utility::retry`)**: a combo the
/// create answers `inactive` is read again five times, each read 300 ms after the answer before it (the first
/// too), and still inactive it is a terminal reject naming the combo with no order sent and no line of the
/// wait's own; read active, it says so ONCE at `info` and the order goes; an already-active combo is never
/// read again.
#[test]
fn a_fresh_combo_is_read_again_five_times_300_ms_apart_before_it_is_rejected() {
    const CREATE: &str = "private/create_combo";
    const DETAILS: &str = "public/get_combo_details";
    let pause = std::time::Duration::from_millis(300);
    let reads = |arrived: &[(String, std::time::Instant)]| -> Vec<std::time::Instant> {
        arrived.iter().filter(|(m, _)| m == CREATE || m == DETAILS).map(|(_, at)| *at).collect()
    };
    let sent = |arrived: &[(String, std::time::Instant)]| {
        arrived.iter().filter(|(m, _)| m == "private/buy").count()
    };

    let (events, arrived, infos) = submit_against(&[(CREATE, INACTIVE), (DETAILS, INACTIVE)]);
    assert_eq!(kinds(&events), vec!["Submitted", "Rejected"]);
    let Event::OrderRejected(r) = &events[1] else { unreachable!() };
    assert_eq!(r.reason.to_string(), "combo FAKE-COMBO is inactive (no order book)");
    let at = reads(&arrived);
    assert_eq!(at.len(), 6, "the create, then five reads: {arrived:?}");
    assert!(at.windows(2).all(|w| w[1] - w[0] >= pause), "300 ms before each read: {at:?}");
    assert_eq!((sent(&arrived), infos.len()), (0, 0), "{infos:?}");

    let (events, arrived, infos) = submit_against(&[(CREATE, INACTIVE), (DETAILS, ACTIVE)]);
    assert_eq!(kinds(&events), vec!["Submitted", "Accepted"]);
    let at = reads(&arrived);
    assert_eq!(at.len(), 2, "the create and ONE read: {arrived:?}");
    assert!(at[1] - at[0] >= pause, "{at:?}");
    assert_eq!(sent(&arrived), 1);
    assert_eq!(infos.first().map(String::as_str), Some("fresh combo book activated FAKE-COMBO"));
    assert!(!infos.iter().skip(1).any(|line| line.starts_with("fresh combo")), "{infos:?}");

    let (events, arrived, infos) = submit_against(&[(CREATE, ACTIVE)]);
    assert_eq!(kinds(&events), vec!["Submitted", "Accepted"]);
    assert_eq!((reads(&arrived).len(), sent(&arrived)), (1, 1), "never read again: {arrived:?}");
    assert!(!infos.iter().any(|line| line.starts_with("fresh combo")), "{infos:?}");
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
