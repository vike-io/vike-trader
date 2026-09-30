//! R6 slice-3 golden parity: the Binance perp adapter against the FROZEN `fixtures/r6/perp.json`
//! bytes — ORDER_TRADE_UPDATE/ACCOUNT_UPDATE mapper events, fapi instruments, order params
//! (positionSide/reduceOnly deltas), and reconcile_positions incl. the pinned SHORT-position and
//! hedge-legs gates.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use std::path::PathBuf;
use vike_binance::perp::{BinancePerpRest, map_perp_open_order, parse_binance_perp_instruments};
use vike_binance::perp_mapper::map_binance_perp;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::{PreparedRequest, Signer};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_model::f64_to_hex_bits;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6/perp.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn pairs(v: &serde_json::Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
        .collect()
}

#[test]
fn perp_mapper_parity() {
    let fx = fixture();
    for (i, case) in fx["mapper"].as_array().unwrap().iter().enumerate() {
        let got: Vec<serde_json::Value> = map_binance_perp(&case["frame"], "binance", "BTCUSDT")
            .iter()
            .map(|e| serde_json::to_value(e).unwrap())
            .collect();
        let want = case["events"].as_array().unwrap();
        assert_eq!(got.len(), want.len(), "mapper[{i}] event count: {got:?}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g, w, "mapper[{i}]");
        }
    }
}

#[test]
fn perp_order_trade_update_surfaces_the_commission_asset() {
    // USDS-M ORDER_TRADE_UPDATE carries `N` = commission asset inside `o`.
    let frame = serde_json::json!({
        "e": "ORDER_TRADE_UPDATE",
        "o": {
            "x": "TRADE", "X": "FILLED", "c": "c1", "S": "BUY", "s": "BTCUSDT",
            "l": "1.0", "L": "100.0", "n": "0.5", "N": "USDT", "t": 7, "m": false, "ps": "BOTH"
        }
    });
    let fill = map_binance_perp(&frame, "binance", "BTCUSDT")
        .into_iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.5);
    assert_eq!(fill.commission_asset, "USDT");
}

#[test]
fn perp_instruments_parity() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let want = fx["instruments"].as_object().unwrap();
    assert_eq!(instruments.len(), want.len());
    for (sym, inst) in &instruments {
        let w = &want[sym];
        for (name, val) in [
            ("tick_size", inst.properties.tick_size),
            ("step_size", inst.properties.step_size),
            ("min_qty", inst.properties.min_qty),
            ("max_qty", inst.properties.max_qty),
            ("min_notional", inst.properties.min_notional),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{sym}.{name}");
        }
        assert_eq!(inst.base_asset, w["base_asset"].as_str().unwrap(), "{sym}.base_asset");
    }
}

/// Reconcile open-order mapping: a fapi `/openOrders` row → a reconcile-seeded ACCEPTED
/// `ManagedOrder` (feeds `apply_snapshot`'s stale-order reap). A limit row keeps its price; a market
/// row's "0" price maps to None.
#[test]
fn perp_open_order_mapping() {
    let limit = serde_json::json!({
        "clientOrderId": "vperpL", "orderId": 8886, "symbol": "BTCUSDT",
        "side": "SELL", "type": "LIMIT", "origQty": "0.005", "executedQty": "0.0",
        "price": "65000.5", "positionSide": "BOTH"
    });
    let mo = map_perp_open_order(&limit);
    assert_eq!(mo.client_order_id(), "vperpL");
    assert_eq!(mo.request.venue, "binance");
    assert_eq!(mo.request.symbol, "BTCUSDT");
    assert_eq!(mo.request.side, -1, "SELL → -1");
    assert_eq!(mo.request.qty, 0.005);
    assert_eq!(mo.request.order_type, "limit");
    assert_eq!(mo.request.price, Some(65000.5));
    assert_eq!(mo.status, vike_exec::OrderStatus::Accepted);
    assert_eq!(mo.venue_order_id.as_deref(), Some("8886"));

    let market = serde_json::json!({
        "clientOrderId": "vperpM", "orderId": 8887, "symbol": "BTCUSDT",
        "side": "BUY", "type": "MARKET", "origQty": "0.002", "executedQty": "0.0", "price": "0"
    });
    let mo = map_perp_open_order(&market);
    assert_eq!(mo.request.side, 1, "BUY → +1");
    assert_eq!(mo.request.order_type, "market");
    assert_eq!(mo.request.price, None, "market row '0' price → None");
}

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

struct Stub {
    position_rows: serde_json::Value,
    balance_resp: serde_json::Value,
    balance_fails: bool,
}

impl RestTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/positionRisk") {
            Ok(self.position_rows.clone())
        } else if path.ends_with("/balance") {
            if self.balance_fails {
                Err(VenueApiError { code: 0, msg: "balance down".into() })
            } else {
                Ok(self.balance_resp.clone())
            }
        } else if path.ends_with("/openOrders") {
            // Reconcile now also fetches resting orders (feeds apply_snapshot's stale-order reap);
            // this matrix pins the POSITION legs, so an empty resting set keeps the snapshot shape.
            Ok(serde_json::json!([]))
        } else {
            panic!("unexpected path {path}")
        }
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

#[test]
fn perp_params_and_reconcile_parity() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let make = |rows: serde_json::Value, fails: bool| BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: Stub {
            position_rows: rows,
            balance_resp: fx["balance_resp"].clone(),
            balance_fails: fails,
        },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };

    // build_order_params — ordered pairs vs the frozen `order_params` block (limit sell +
    // reduce-only market)
    let client = make(serde_json::json!([]), false);
    let limit_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6perpL", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.0034999999, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6perpM", "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let got: Vec<(String, String)> = client
        .build_order_params(&limit_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, String)> = client
        .build_order_params(&market_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, pairs(&fx["order_params"]["market"]), "market params");

    // reconcile matrix — net long / NET SHORT (the pinned gate) / hedge / flat / bal-fail
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let client = make(case["position_rows"].clone(), case["balance_fails"].as_bool().unwrap());
        let snap = client.reconcile_positions().unwrap();
        let want = &case["snapshot"];
        let check_pairs = |got: &[(String, f64)], want: &serde_json::Value, what: &str| {
            let want = want.as_array().unwrap();
            assert_eq!(got.len(), want.len(), "{name}: {what} count");
            for ((sym, v), w) in got.iter().zip(want) {
                assert_eq!(sym, w[0].as_str().unwrap(), "{name}: {what} symbol");
                assert_eq!(f64_to_hex_bits(*v), w[1].as_str().unwrap(), "{name}: {what}");
            }
        };
        check_pairs(&snap.positions, &want["positions"], "positions");
        check_pairs(&snap.position_avg_px, &want["position_avg_px"], "avg_px");
        check_pairs(&snap.position_mark_px, &want["position_mark_px"], "mark_px");
        let want_sides: Vec<(String, String)> = want["position_sides"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(snap.position_sides, want_sides, "{name}: sides");
        assert_eq!(
            f64_to_hex_bits(snap.balance),
            want["balance"].as_str().unwrap(),
            "{name}: balance"
        );
        if name == "net_short" {
            assert!(snap.positions[0].1 < 0.0, "the SHORT gate must be signed-negative");
        }
    }
}

/// Stub for the native batch + modify endpoints: records the params and returns fapi-shaped
/// responses — batchOrders POST = an array (one order-ok, one error); PUT /order = the modified
/// order; DELETE batchOrders = an ok array.
struct OrderStub {
    last_params: std::cell::RefCell<Vec<(String, String)>>,
}

impl RestTransport for OrderStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        *self.last_params.borrow_mut() =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        if path.ends_with("/batchOrders") && method.eq_ignore_ascii_case("POST") {
            Ok(serde_json::json!([
                {"orderId": 111, "clientOrderId": "bat0", "status": "NEW"},
                {"code": -2019, "msg": "Margin is insufficient."}
            ]))
        } else if path.ends_with("/batchOrders") && method.eq_ignore_ascii_case("DELETE") {
            Ok(serde_json::json!([{"orderId": 111}, {"orderId": 112}]))
        } else if path.ends_with("/order") && method.eq_ignore_ascii_case("PUT") {
            Ok(serde_json::json!({"orderId": 999, "clientOrderId": "vtModify", "status": "NEW"}))
        } else {
            panic!("unexpected {method} {path}")
        }
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn perp_client() -> BinancePerpRest<NullSigner, OrderStub> {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: OrderStub { last_params: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

fn limit(coid: &str, side: i32, qty: f64, price: f64) -> vike_model::OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": coid, "venue": "binance", "symbol": "BTCUSDT",
        "side": side, "qty": qty, "order_type": "limit", "price": price
    }))
    .unwrap()
}

#[test]
fn native_batch_submit_and_cancel() {
    let client = perp_client();
    let ev =
        client.submit_batch(&[limit("bat0", 1, 0.002, 60000.0), limit("bat1", 1, 0.002, 60000.5)]);
    let kinds: Vec<String> = ev
        .iter()
        .map(|e| serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, vec!["OrderSubmitted", "OrderSubmitted", "OrderAccepted", "OrderRejected"]);
    match (&ev[2], &ev[3]) {
        (
            vike_model::events::Event::OrderAccepted(a),
            vike_model::events::Event::OrderRejected(r),
        ) => {
            assert_eq!(a.client_order_id, "bat0");
            assert_eq!(a.venue_order_id.as_deref(), Some("111"));
            assert_eq!(r.client_order_id, "bat1");
            assert!(r.reason.contains("Margin"), "reason from the error element: {}", r.reason);
        }
        other => panic!("unexpected tail {other:?}"),
    }
    // the batchOrders param is a JSON array of 2 order objects
    let params = client.transport.last_params.borrow();
    let batch = params.iter().find(|(k, _)| k == "batchOrders").map(|(_, v)| v).unwrap();
    let arr: serde_json::Value = serde_json::from_str(batch).unwrap();
    assert_eq!(arr.as_array().map(|a| a.len()), Some(2), "batchOrders array");
    drop(params);

    let client = perp_client();
    assert!(client.cancel_batch(&["bat0".into(), "bat1".into()]).is_ok());
}

#[test]
fn native_modify_sends_side_and_both_qty_price() {
    let client = perp_client();
    // resting limit BUY qty 0.005 @ 60000; modify PRICE ONLY → qty must fall back to the resting
    // value (fapi modify requires BOTH), and `side` must be sent
    let order = limit("vtModify", 1, 0.005, 60000.0);
    let ev = client.modify_order(&order, None, Some(61000.0));
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.client_order_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("999"));
            assert_eq!(a.new_price, Some(61000.0));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    let params = client.transport.last_params.borrow();
    let get = |k: &str| params.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.clone());
    assert_eq!(get("side").as_deref(), Some("BUY"), "fapi modify requires side");
    assert_eq!(get("origClientOrderId").as_deref(), Some("vtModify"));
    assert!(get("quantity").is_some(), "quantity sent (fallback to resting qty)");
    assert!(get("price").is_some(), "price sent");
}

#[test]
fn stop_order_maps_to_stop_market() {
    let client = perp_client();
    // a bracket stop-loss leg: reduce-only "stop" with a trigger_price
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "sl1", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.005, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap();
    let params = client.build_order_params(&stop); // pure — no transport call
    let get = |k: &str| params.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.clone());
    assert_eq!(get("type").as_deref(), Some("STOP_MARKET"), "native conditional stop");
    assert!(get("stopPrice").is_some(), "stopPrice = trigger");
    assert_eq!(get("reduceOnly").as_deref(), Some("true"));
    assert!(get("price").is_none(), "STOP_MARKET has no limit price");
}

/// Stub whose PUT /order (modify) always fails — to exercise the modify-reject path (audit T2).
struct FailingModifyStub;
impl RestTransport for FailingModifyStub {
    fn signed(
        &self,
        _base: &str,
        _path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        Err(VenueApiError { code: -2013, msg: "Order does not exist.".into() })
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

#[test]
fn native_modify_failure_emits_modify_rejected() {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    let client = BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: FailingModifyStub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };
    let order = limit("vtModify", 1, 0.005, 60000.0);
    let ev = client.modify_order(&order, None, Some(61000.0));
    assert_eq!(ev.len(), 1, "a failed modify must not vanish");
    match &ev[0] {
        vike_model::events::Event::OrderModifyRejected(r) => {
            assert_eq!(r.client_order_id, "vtModify");
            assert!(r.reason.contains("does not exist"), "carries venue reason: {}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

/// Audit T1: a submit whose POST returns the ambiguous-timeout sentinel must NOT emit a false
/// terminal — it re-queries order status (GET) and resolves live-vs-dead. `get` selects the GET
/// outcome: Some(Ok(has_order)) or an error code.
struct TimeoutThenStatusStub {
    get_has_order: Option<bool>, // Some(true)=venue has it, Some(false)=absent, None=GET also times out
    get_absent_code: i64,        // code used when get_has_order == Some(false)
}
impl RestTransport for TimeoutThenStatusStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/order") && method.eq_ignore_ascii_case("POST") {
            return Err(VenueApiError {
                code: E_TIMEOUT_AMBIGUOUS,
                msg: "network error: timeout".into(),
            });
        }
        if path.ends_with("/order") && method.eq_ignore_ascii_case("GET") {
            return match self.get_has_order {
                Some(true) => Ok(serde_json::json!({"orderId": 777, "status": "NEW"})),
                Some(false) => {
                    Err(VenueApiError { code: self.get_absent_code, msg: "absent".into() })
                }
                None => Err(VenueApiError {
                    code: E_TIMEOUT_AMBIGUOUS,
                    msg: "network error: timeout".into(),
                }),
            };
        }
        panic!("unexpected {method} {path}")
    }
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("unexpected public {path}")
    }
}

fn perp_with(stub: TimeoutThenStatusStub) -> BinancePerpRest<NullSigner, TimeoutThenStatusStub> {
    let fx = fixture();
    let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
    BinancePerpRest {
        link_id: None,
        signer: NullSigner,
        transport: stub,
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

fn last_event_kind(ev: &[vike_model::events::Event]) -> &'static str {
    use vike_model::events::Event;
    match ev.last().unwrap() {
        Event::OrderAccepted(_) => "Accepted",
        Event::OrderRejected(_) => "Rejected",
        _ => "other",
    }
}

#[test]
fn timeout_submit_with_live_order_resolves_to_accepted() {
    let client = perp_with(TimeoutThenStatusStub { get_has_order: Some(true), get_absent_code: 0 });
    let ev = client.submit_order(&limit("t1", 1, 0.005, 60000.0));
    assert_eq!(
        last_event_kind(&ev),
        "Accepted",
        "timed-out-but-live order must be managed, not a phantom"
    );
}

#[test]
fn timeout_submit_venue_absent_resolves_to_rejected() {
    // -2013 = order does not exist → the venue confirms it never landed → TRUE terminal reject.
    let client =
        perp_with(TimeoutThenStatusStub { get_has_order: Some(false), get_absent_code: -2013 });
    let ev = client.submit_order(&limit("t2", 1, 0.005, 60000.0));
    assert_eq!(last_event_kind(&ev), "Rejected");
}

#[test]
fn timeout_submit_double_timeout_is_optimistic_never_false_terminal() {
    let client = perp_with(TimeoutThenStatusStub { get_has_order: None, get_absent_code: 0 });
    let ev = client.submit_order(&limit("t3", 1, 0.005, 60000.0));
    assert_eq!(
        last_event_kind(&ev),
        "Accepted",
        "double-timeout stays optimistic — never a false terminal"
    );
}

/// Audit T1 regression: a DEFINITE failure (connect-refused code 0, or a venue business error)
/// must STILL be a terminal reject and must NOT trigger a status re-query — only the ambiguous
/// timeout sentinel diverts. The stub panics on any GET to prove no requery happens.
#[test]
fn definite_failure_still_terminal_rejects_without_requery() {
    struct RejectStub(i64);
    impl RestTransport for RejectStub {
        fn signed(
            &self,
            _b: &str,
            path: &str,
            method: &str,
            _p: &[(&str, String)],
            _s: &dyn Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            if path.ends_with("/order") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query the venue (saw {method} {path})");
        }
        fn public(
            &self,
            _b: &str,
            path: &str,
            _p: &[(&str, String)],
        ) -> Result<serde_json::Value, VenueApiError> {
            panic!("unexpected public {path}")
        }
    }
    for code in [0_i64, -2010] {
        let fx = fixture();
        let instruments = parse_binance_perp_instruments(&fx["exchange_info"]);
        let client = BinancePerpRest {
            link_id: None,
            signer: NullSigner,
            transport: RejectStub(code),
            base_url: "https://stub".into(),
            symbol: "BTCUSDT".into(),
            properties: instruments["BTCUSDT"].properties,
            leverage: 2.0,
        };
        let ev = client.submit_order(&limit("d1", 1, 0.005, 60000.0));
        assert_eq!(
            last_event_kind(&ev),
            "Rejected",
            "definite failure (code {code}) must stay terminal"
        );
    }
}
