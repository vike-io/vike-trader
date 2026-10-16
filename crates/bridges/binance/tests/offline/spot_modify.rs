//! The SPOT modify (`BinanceSpotRest::modify_order`): an atomic `POST /api/v3/order/cancelReplace`
//! under the SAME client order id, because binance spot has no in-place price amend. Before this
//! the spot lane took `VenueRest`'s default no-op, so a modify was ACCEPTED by the control door and
//! then silently did nothing at the venue (measured on the CI box, v0.1.60, demo account 3).
//!
//! Pinned here, with a scripted transport and no network:
//! * the wire shape (same coid on both halves, `STOP_ON_FAILURE`, `ONLY_NEW`, a marked throwaway
//!   id for the cancel half) and the success event;
//! * EVERY failure answers with an explicit event, never an empty vec;
//! * the `-2021` case (cancel landed, replacement refused) is settled by asking the venue;
//! * the readers that must not mistake the cancel half for a cancel of the order: the stream
//!   mapper (unit-tested beside it) and `map_binance_history`.

use std::sync::Mutex;

use vike_binance::history::map_binance_history;
use vike_binance::spot::BinanceSpotRest;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType, SymbolProperties};

use crate::tif_gate::NullSigner;

type Touch = (String, String, Vec<(String, String)>);

/// Answers `POST .../cancelReplace` with `replace` and `GET .../order` with `status`, and records
/// every touch.
struct Script {
    replace: Result<serde_json::Value, VenueApiError>,
    status: Result<serde_json::Value, VenueApiError>,
    touched: Mutex<Vec<Touch>>,
}
impl RestTransport for Script {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        self.touched.lock().unwrap().push((
            method.to_string(),
            path.to_string(),
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        ));
        match (method, path) {
            ("POST", "/api/v3/order/cancelReplace") => self.replace.clone(),
            ("GET", "/api/v3/order") => self.status.clone(),
            other => panic!("unexpected wire touch {other:?}"),
        }
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public touch {path}");
    }
}

fn script(
    replace: Result<serde_json::Value, VenueApiError>,
    status: Result<serde_json::Value, VenueApiError>,
) -> Script {
    Script { replace, status, touched: Mutex::new(Vec::new()) }
}

fn err(code: i64, msg: &str) -> VenueApiError {
    VenueApiError { code, msg: msg.to_string() }
}

fn spot(transport: Script, link_id: Option<&str>) -> BinanceSpotRest<NullSigner, Script> {
    BinanceSpotRest {
        signer: NullSigner,
        transport,
        base_url: "https://unused.invalid".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: SymbolProperties { tick_size: 0.01, step_size: 0.0001, ..Default::default() },
        base_asset: "BTC".to_string(),
        link_id: link_id.map(str::to_string),
    }
}

/// The measured the CI box order: LIMIT BUY BTCUSDT 0.0001 @ 65700.
fn resting() -> OrderRequest {
    OrderRequest {
        client_order_id: "vt0abc".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.0001,
        order_type: OrderType::Limit,
        price: Some(65_700.0),
        ..Default::default()
    }
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn ok_replace(order_id: i64) -> serde_json::Value {
    serde_json::json!({
        "cancelResult": "SUCCESS",
        "newOrderResult": "SUCCESS",
        "cancelResponse": {"symbol": "BTCUSDT", "orderId": 1, "status": "CANCELED"},
        "newOrderResponse": {"symbol": "BTCUSDT", "orderId": order_id, "clientOrderId": "vt0abc"}
    })
}

#[test]
fn a_price_modify_is_one_cancel_replace_under_the_same_coid() {
    let s = spot(script(Ok(ok_replace(77)), Ok(serde_json::json!({}))), None);
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));

    let touched = s.transport.touched.lock().unwrap().clone();
    assert_eq!(touched.len(), 1, "a clean success is ONE request: {touched:?}");
    let (method, path, p) = &touched[0];
    assert_eq!((method.as_str(), path.as_str()), ("POST", "/api/v3/order/cancelReplace"));
    assert_eq!(param(p, "symbol"), Some("BTCUSDT"));
    assert_eq!(param(p, "side"), Some("BUY"));
    assert_eq!(param(p, "type"), Some("LIMIT"));
    assert_eq!(param(p, "timeInForce"), Some("GTC"));
    assert_eq!(param(p, "price").and_then(|v| v.parse::<f64>().ok()), Some(65_600.0));
    assert_eq!(param(p, "quantity").and_then(|v| v.parse::<f64>().ok()), Some(0.0001));
    assert_eq!(param(p, "cancelReplaceMode"), Some("STOP_ON_FAILURE"));
    assert_eq!(param(p, "cancelRestrictions"), Some("ONLY_NEW"));
    assert_eq!(param(p, "cancelOrigClientOrderId"), Some("vt0abc"));
    assert_eq!(
        param(p, "newClientOrderId"),
        Some("vt0abc"),
        "the replacement keeps the coid the registry, cancel path and reconcile key on"
    );
    let throwaway = param(p, "cancelNewClientOrderId").unwrap();
    assert_ne!(throwaway, "vt0abc");
    assert!(
        vike_binance::family::order_map::is_replace_cancel_id(throwaway),
        "the cancel half is marked so its CANCELED report can be told from a real cancel"
    );

    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "vt0abc");
            assert_eq!(m.venue_order_id.as_deref(), Some("77"), "the venue id is the NEW order's");
            assert_eq!(m.new_price, Some(65_600.0));
            assert_eq!(m.new_qty, None);
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
}

#[test]
fn a_quantity_modify_sends_the_new_total_and_keeps_the_price() {
    let s = spot(script(Ok(ok_replace(78)), Ok(serde_json::json!({}))), None);
    let events = VenueRest::modify_order(&s, &resting(), Some(0.0002), None);
    let touched = s.transport.touched.lock().unwrap().clone();
    let p = &touched[0].2;
    assert_eq!(param(p, "quantity").and_then(|v| v.parse::<f64>().ok()), Some(0.0002));
    assert_eq!(param(p, "price").and_then(|v| v.parse::<f64>().ok()), Some(65_700.0));
    assert!(matches!(&events[0], Event::OrderModified(m) if m.new_qty == Some(0.0002)));
}

#[test]
fn the_broker_prefix_is_applied_to_both_halves_ids() {
    let s = spot(script(Ok(ok_replace(79)), Ok(serde_json::json!({}))), Some("LNK"));
    let _ = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    let touched = s.transport.touched.lock().unwrap().clone();
    let p = &touched[0].2;
    assert_eq!(param(p, "cancelOrigClientOrderId"), Some("x-LNK-vt0abc"));
    assert_eq!(param(p, "newClientOrderId"), Some("x-LNK-vt0abc"));
}

#[test]
fn a_non_limit_order_is_refused_loudly_without_touching_the_wire() {
    let s = spot(script(Ok(ok_replace(1)), Ok(serde_json::json!({}))), None);
    let mut o = resting();
    o.order_type = OrderType::Market;
    let events = VenueRest::modify_order(&s, &o, None, Some(65_600.0));
    assert!(s.transport.touched.lock().unwrap().is_empty());
    assert!(
        matches!(&events[..], [Event::OrderModifyRejected(r)] if r.client_order_id == "vt0abc")
    );
}

#[test]
fn a_nonsense_price_or_quantity_is_refused_before_the_wire() {
    for (q, p) in [(Some(0.0), None), (Some(f64::NAN), None), (None, Some(0.0)), (None, Some(-1.0))]
    {
        let s = spot(script(Ok(ok_replace(1)), Ok(serde_json::json!({}))), None);
        let events = VenueRest::modify_order(&s, &resting(), q, p);
        assert!(s.transport.touched.lock().unwrap().is_empty(), "{q:?} {p:?}");
        assert!(matches!(&events[..], [Event::OrderModifyRejected(_)]), "{q:?} {p:?}");
    }
}

#[test]
fn a_refusal_before_the_cancel_is_an_explicit_modify_rejected_and_asks_nothing_more() {
    // e.g. -1013 filter failure on the request itself: the order is untouched.
    let s = spot(
        script(Err(err(-1013, "Filter failure: PRICE_FILTER")), Ok(serde_json::json!({}))),
        None,
    );
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    assert_eq!(s.transport.touched.lock().unwrap().len(), 1, "no status query for a refusal");
    match &events[..] {
        [Event::OrderModifyRejected(r)] => {
            assert_eq!(r.client_order_id, "vt0abc");
            assert!(r.reason.contains("PRICE_FILTER"), "the venue reason travels: {}", r.reason);
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
}

#[test]
fn a_failed_cancel_half_leaves_the_order_and_says_so() {
    // -2022: the cancel failed (here: the order has an execution, ONLY_NEW), nothing was placed.
    let live = serde_json::json!({
        "status": "PARTIALLY_FILLED", "orderId": 5, "price": "65700.00000000", "origQty": "0.00010000"
    });
    let s = spot(script(Err(err(-2022, "Order cancel-replace failed.")), Ok(live)), None);
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    assert!(matches!(&events[..], [Event::OrderModifyRejected(_)]), "{events:?}");
}

#[test]
fn a_refused_replacement_after_a_landed_cancel_reports_the_order_gone() {
    // -2021: the cancel landed, the replacement was refused. The stream's CANCELED for this id is
    // dropped on purpose, so THIS event is the only thing that terminates the order locally.
    let gone = serde_json::json!({"status": "CANCELED", "orderId": 5, "price": "65700.00000000"});
    let s = spot(script(Err(err(-2021, "Order cancel-replace partially failed.")), Ok(gone)), None);
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    match &events[..] {
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, "vt0abc"),
        other => panic!("expected one OrderCanceled, got {other:?}"),
    }
}

#[test]
fn an_ambiguous_timeout_that_did_go_through_is_reported_as_modified() {
    let applied = serde_json::json!({
        "status": "NEW", "orderId": 91, "price": "65600.00000000", "origQty": "0.00010000"
    });
    let s = spot(script(Err(err(E_TIMEOUT_AMBIGUOUS, "timeout")), Ok(applied)), None);
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    match &events[..] {
        [Event::OrderModified(m)] => {
            assert_eq!(m.venue_order_id.as_deref(), Some("91"));
            assert_eq!(m.new_price, Some(65_600.0));
        }
        other => panic!("expected one OrderModified, got {other:?}"),
    }
}

#[test]
fn an_ambiguous_timeout_that_did_not_apply_is_a_modify_rejected() {
    let untouched = serde_json::json!({
        "status": "NEW", "orderId": 5, "price": "65700.00000000", "origQty": "0.00010000"
    });
    let s = spot(script(Err(err(E_TIMEOUT_AMBIGUOUS, "timeout")), Ok(untouched)), None);
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    assert!(matches!(&events[..], [Event::OrderModifyRejected(_)]), "{events:?}");
}

#[test]
fn an_unreadable_outcome_is_still_an_explicit_event() {
    let s = spot(
        script(Err(err(-2021, "partially failed")), Err(err(-1003, "too many requests"))),
        None,
    );
    let events = VenueRest::modify_order(&s, &resting(), None, Some(65_600.0));
    match &events[..] {
        [Event::OrderModifyRejected(r)] => {
            assert!(r.reason.contains("resync pending"), "names the doubt: {}", r.reason);
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
}

/// The history replay lists the replaced order's CANCELED row beside its live successor under the
/// SAME coid. Replaying that row would terminate a live, modified order.
#[test]
fn history_ignores_the_canceled_row_of_a_replaced_order_but_not_a_real_cancel() {
    let orders = serde_json::json!([
        {"orderId": 5, "clientOrderId": "vt0abc", "status": "CANCELED", "updateTime": 100},
        {"orderId": 9, "clientOrderId": "vt0abc", "status": "NEW", "updateTime": 100},
        {"orderId": 6, "clientOrderId": "vt0def", "status": "CANCELED", "updateTime": 101},
    ]);
    let events = map_binance_history(&orders, &serde_json::json!([]), "binance", "BTCUSDT");
    let canceled: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(c) => Some(c.client_order_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(canceled, vec!["vt0def"], "only the genuinely cancelled order replays as cancelled");

    // ...and when the SUCCESSOR was itself cancelled, that newest incarnation does replay.
    let orders = serde_json::json!([
        {"orderId": 5, "clientOrderId": "vt0abc", "status": "CANCELED", "updateTime": 100},
        {"orderId": 9, "clientOrderId": "vt0abc", "status": "CANCELED", "updateTime": 200},
    ]);
    let events = map_binance_history(&orders, &serde_json::json!([]), "binance", "BTCUSDT");
    assert_eq!(events.iter().filter(|e| matches!(e, Event::OrderCanceled(_))).count(), 1);
}
