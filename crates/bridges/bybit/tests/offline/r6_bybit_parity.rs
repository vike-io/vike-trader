//! R6 slice-4 golden parity: the Bybit V5 adapter against the FROZEN `fixtures/r6/bybit.json`
//! bytes — REST signer (GET query + POST exact-body signing), WS auth frames, spot/perp mappers
//! (incl. BustTrade/AdlTrade liq + the Funding-row drop that routes around the execFee sign trap),
//! funding-settlement decoder, instruments, typed order params, and the reconcile matrix incl.
//! NET SHORT + hedge.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use std::path::PathBuf;
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::event_mapper::{map_bybit_perp, map_bybit_private, map_execution};
use vike_bybit::funding::decode_bybit_funding_settlements;
use vike_bybit::perp::{map_bybit_open_order, parse_bybit_perp_instruments, BybitPerpRest};
use vike_bybit::transport::BybitTransport;
use vike_bybit::ws_auth::{build_auth_frame, build_subscribe_frame, bybit_ws_sign};
use vike_model::f64_to_hex_bits;

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6/bybit.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn creds(fx: &serde_json::Value) -> Credentials {
    Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string(),
        passphrase: None,
    }
}

fn typed_pairs(v: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].clone()))
        .collect()
}

#[test]
fn rest_signer_query_and_body_byte_identical() {
    let fx = fixture();
    let now = fx["now_ms"].as_i64().unwrap();
    for (i, case) in fx["rest_signer"].as_array().unwrap().iter().enumerate() {
        let signer = BybitV5Signer::new(&creds(&fx), move || now);
        signer.set_offset_ms(-77); // matches the exporter
        let owned = typed_pairs(&case["params"]);
        let params: Vec<(&str, serde_json::Value)> =
            owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let prepared = signer.prepare(&params, case["method"].as_str().unwrap());
        assert_eq!(prepared.query, case["query"].as_str().unwrap(), "case {i} query");
        match (&prepared.body, case["body"].as_str()) {
            (Some(b), Some(want)) => {
                assert_eq!(std::str::from_utf8(b).unwrap(), want, "case {i} body")
            }
            (None, None) => {}
            (a, b) => panic!("case {i} body presence mismatch {a:?} vs {b:?}"),
        }
        let got: std::collections::HashMap<_, _> = prepared.headers.iter().cloned().collect();
        for (k, v) in case["headers"].as_object().unwrap() {
            assert_eq!(got.get(k).map(String::as_str), v.as_str(), "case {i} header {k}");
        }
    }
}

#[test]
fn ws_auth_parity() {
    let fx = fixture();
    let ws = &fx["ws_auth"];
    for case in ws["sign"].as_array().unwrap() {
        assert_eq!(
            bybit_ws_sign(fx["api_secret"].as_str().unwrap(), case["expires"].as_i64().unwrap()),
            case["signature"].as_str().unwrap()
        );
    }
    let auth = build_auth_frame(
        fx["api_key"].as_str().unwrap(),
        fx["api_secret"].as_str().unwrap(),
        fx["now_ms"].as_i64().unwrap(),
    );
    assert_eq!(auth, ws["auth_frame"], "auth frame");
    assert_eq!(
        build_subscribe_frame(&["execution", "order"]),
        ws["subscribe_frame"],
        "subscribe frame"
    );
}

fn assert_events(got: Vec<vike_model::events::Event>, want: &serde_json::Value, what: &str) {
    let got: Vec<serde_json::Value> =
        got.iter().map(|e| serde_json::to_value(e).unwrap()).collect();
    let want = want.as_array().unwrap();
    assert_eq!(got.len(), want.len(), "{what}: count {got:?}");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "{what}");
    }
}

#[test]
fn mappers_parity() {
    let fx = fixture();
    for (i, case) in fx["spot_mapper"].as_array().unwrap().iter().enumerate() {
        assert_events(
            map_bybit_private(&case["frame"], "bybit", "BTCUSDT"),
            &case["events"],
            &format!("spot_mapper[{i}]"),
        );
    }
    for (i, case) in fx["perp_mapper"].as_array().unwrap().iter().enumerate() {
        assert_events(
            map_bybit_perp(&case["frame"], "bybit", "BTCUSDT"),
            &case["events"],
            &format!("perp_mapper[{i}]"),
        );
    }
}

#[test]
fn execution_surfaces_the_fee_currency() {
    // Bybit execution rows carry `feeCurrency` alongside `execFee`.
    let item = serde_json::json!({
        "execType": "Trade", "orderId": "o1", "orderLinkId": "c1", "execId": "e1",
        "execPrice": "100.0", "execQty": "1.0", "side": "Buy", "execFee": "0.1",
        "feeCurrency": "USDC", "isMaker": false, "execTime": "5"
    });
    let fill = map_execution(&item, "bybit", "BTCUSDT")
        .into_iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.1);
    assert_eq!(fill.commission_asset, "USDC");
}

#[test]
fn funding_decoder_parity() {
    let fx = fixture();
    let rows: Vec<serde_json::Value> = fx["funding_rows"].as_array().unwrap().clone();
    let got: Vec<serde_json::Value> = decode_bybit_funding_settlements(&rows, "bybit", "BTCUSDT")
        .iter()
        .map(|e| {
            // the exporter tags FundingEvent dicts; wrap ours identically
            let mut v = serde_json::to_value(e).unwrap();
            v["type"] = serde_json::json!("FundingEvent");
            v
        })
        .collect();
    let want = fx["funding_events"].as_array().unwrap();
    assert_eq!(got.len(), want.len(), "funding event count");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "funding event");
    }
}

struct Stub {
    position_rows: serde_json::Value,
    wallet: serde_json::Value,
    wallet_fails: bool,
}

impl BybitTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/position/list") {
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK",
                                  "result": {"list": self.position_rows}}))
        } else if path.ends_with("/wallet-balance") {
            if self.wallet_fails {
                Err(VenueApiError { code: 0, msg: "wallet down".into() })
            } else {
                Ok(self.wallet.clone())
            }
        } else if path.ends_with("/set-leverage") {
            Ok(serde_json::json!({"retCode": 110043, "retMsg": "leverage not modified"}))
        } else if path.ends_with("/order/realtime") {
            // Reconcile now also fetches resting orders (feeds apply_snapshot's stale-order reap);
            // this matrix pins the POSITION legs, so an empty resting set keeps the snapshot shape.
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK", "result": {"list": []}}))
        } else {
            panic!("unexpected path {path}")
        }
    }
}

/// Reconcile open-order mapping: a `/v5/order/realtime` row → a reconcile-seeded ACCEPTED
/// `ManagedOrder` (feeds `apply_snapshot`'s stale-order reap). `orderLinkId` is our client id; a
/// limit row keeps its price; a market row's "0" price maps to None.
#[test]
fn perp_open_order_mapping() {
    let limit = serde_json::json!({
        "orderLinkId": "vperpL", "orderId": "1662ab", "symbol": "BTCUSDT",
        "side": "Sell", "orderType": "Limit", "qty": "0.005", "price": "65000.5"
    });
    let mo = map_bybit_open_order(&limit);
    assert_eq!(mo.client_order_id(), "vperpL");
    assert_eq!(mo.request.venue, "bybit");
    assert_eq!(mo.request.symbol, "BTCUSDT");
    assert_eq!(mo.request.side, -1, "Sell → -1");
    assert_eq!(mo.request.qty, 0.005);
    assert_eq!(mo.request.order_type, "limit");
    assert_eq!(mo.request.price, Some(65000.5));
    assert_eq!(mo.status, vike_exec::OrderStatus::Accepted);
    assert_eq!(mo.venue_order_id.as_deref(), Some("1662ab"));

    let market = serde_json::json!({
        "orderLinkId": "vperpM", "orderId": "1662ac", "symbol": "BTCUSDT",
        "side": "Buy", "orderType": "Market", "qty": "0.002", "price": "0"
    });
    let mo = map_bybit_open_order(&market);
    assert_eq!(mo.request.side, 1, "Buy → +1");
    assert_eq!(mo.request.order_type, "market");
    assert_eq!(mo.request.price, None, "market row '0' price → None");
}

#[test]
fn instruments_params_and_reconcile_parity() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
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
        assert_eq!(inst.base_asset, w["base_asset"].as_str().unwrap());
    }

    let make = |rows: serde_json::Value, fails: bool| BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: Stub {
            position_rows: rows,
            wallet: fx["wallet_resp"].clone(),
            wallet_fails: fails,
        },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };

    // set-leverage: 110043 must be swallowed (benign "already at target")
    make(serde_json::json!([]), false).set_leverage().expect("110043 swallowed");

    // typed order params (bools/ints preserved)
    let limit_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6byL", "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.0025999, "order_type": "limit", "price": 61999.96
    }))
    .unwrap();
    let market_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6byM", "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let client = make(serde_json::json!([]), false);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&limit_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&market_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["market"]), "market params");

    // tif step-2 (FLIPPED bybit row): the golden `limit` fixture uses the DEFAULT TIF (Gtc) —
    // its bytes above are already the byte-identity pin (timeInForce=GTC, same slot as the old
    // hardcode). A limit asking Ioc now yields the SAME golden bytes with ONLY the timeInForce
    // value swapped to "IOC" — the requested TIF reaches the wire, nothing else moves.
    let mut ioc_req = limit_req.clone();
    ioc_req.time_in_force = vike_model::TimeInForce::Ioc;
    let got: Vec<(String, serde_json::Value)> =
        client.build_order_params(&ioc_req).into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let want_ioc: Vec<(String, serde_json::Value)> = typed_pairs(&fx["order_params"]["limit"])
        .into_iter()
        .map(|(k, v)| if k == "timeInForce" { (k, serde_json::json!("IOC")) } else { (k, v) })
        .collect();
    assert_eq!(got, want_ioc, "requested IOC must be on the wire, all other bytes unchanged");
    assert_eq!(
        vike_bridge_core::tif::venue_tif("bybit", vike_model::TimeInForce::Ioc),
        vike_bridge_core::tif::TifOutcome::Mapped("IOC")
    );

    // reconcile matrix — net long / NET SHORT / hedge / flat / wallet-fail
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let client = make(case["position_rows"].clone(), case["wallet_fails"].as_bool().unwrap());
        let snap = client.reconcile_positions().unwrap();
        let want = &case["snapshot"];
        let check = |got: &[(String, f64)], want: &serde_json::Value, what: &str| {
            let want = want.as_array().unwrap();
            assert_eq!(got.len(), want.len(), "{name}: {what} count");
            for ((sym, v), w) in got.iter().zip(want) {
                assert_eq!(sym, w[0].as_str().unwrap());
                assert_eq!(f64_to_hex_bits(*v), w[1].as_str().unwrap(), "{name}: {what}");
            }
        };
        check(&snap.positions, &want["positions"], "positions");
        check(&snap.position_avg_px, &want["position_avg_px"], "avg_px");
        check(&snap.position_mark_px, &want["position_mark_px"], "mark_px");
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
            assert!(snap.positions[0].1 < 0.0, "SHORT gate must be signed-negative");
        }
    }
}

/// Stub for the native-amend path: acks (or rejects via a non-zero retCode) `/v5/order/amend`.
struct ModifyStub {
    fail: bool,
}

impl BybitTransport for ModifyStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/order/amend"), "unexpected path {path}");
        if self.fail {
            Ok(serde_json::json!({"retCode": 110001, "retMsg": "order not exists or too late"}))
        } else {
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK",
                "result": {"orderId": "bb-999", "orderLinkId": "vtModify"}}))
        }
    }
}

#[test]
fn native_modify_maps_to_order_modified() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    let make = |fail: bool| BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: ModifyStub { fail },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };

    // success: the venue acks → one OrderModified carrying the venue orderId + new terms
    let ev = make(false).modify_order("vtModify", Some(0.01), Some(50000.0));
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.client_order_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("bb-999"));
            assert_eq!(a.new_qty, Some(0.01));
            assert_eq!(a.new_price, Some(50000.0));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }

    // failure (retCode != 0) → the resting order keeps its terms, NO event emitted
    assert!(
        make(true).modify_order("vtModify", Some(0.01), None).is_empty(),
        "a rejected modify surfaces nothing"
    );
}

/// Stub for the native batch endpoints: records the body, returns a partial-batch create response
/// (order 0 ok, order 1 rejected via retExtInfo) and an idempotent cancel-batch.
struct BatchStub {
    last_body: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl BybitTransport for BatchStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        *self.last_body.borrow_mut() =
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        if path.ends_with("/order/create-batch") {
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK",
                "result": {"list": [
                    {"orderId": "bb-1", "orderLinkId": "bat0"},
                    {"orderId": "", "orderLinkId": "bat1"}]},
                "retExtInfo": {"list": [{"code": 0, "msg": "OK"}, {"code": 10001, "msg": "qty invalid"}]}}))
        } else if path.ends_with("/order/cancel-batch") {
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK",
                "result": {"list": [{"orderLinkId": "a"}, {"orderLinkId": "b"}]},
                "retExtInfo": {"list": [{"code": 0}, {"code": 0}]}}))
        } else {
            panic!("unexpected path {path}")
        }
    }
}

#[test]
fn native_batch_submit_and_cancel() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    let make = || BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: BatchStub { last_body: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };
    let req = |coid: &str| -> vike_model::OrderRequest {
        serde_json::from_value(serde_json::json!({
            "client_order_id": coid, "venue": "bybit", "symbol": "BTCUSDT",
            "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
        }))
        .unwrap()
    };

    // batch-submit: 2x OrderSubmitted, then per-order Accepted/Rejected from retExtInfo.list[i].code
    let client = make();
    let ev = client.submit_batch(&[req("bat0"), req("bat1")]);
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
            assert_eq!(a.venue_order_id.as_deref(), Some("bb-1"));
            assert_eq!(r.client_order_id, "bat1");
            assert_eq!(r.reason, "qty invalid");
        }
        other => panic!("unexpected tail {other:?}"),
    }
    // body is {category, request:[…]} — the request array holds 2 orders, each WITHOUT category
    let body = client.transport.last_body.borrow();
    let request = body.iter().find(|(k, _)| k == "request").map(|(_, v)| v).unwrap();
    assert_eq!(request.as_array().map(|a| a.len()), Some(2), "request array");
    assert!(request[0].get("category").is_none(), "per-order object omits category");
    assert!(request[0].get("orderLinkId").is_some(), "per-order object has orderLinkId");
    drop(body);

    // batch-cancel: idempotent ok, body is {category, request:[…]}
    let client = make();
    assert!(client.cancel_batch(&["a".into(), "b".into()]).is_ok());
    let body = client.transport.last_body.borrow();
    let request = body.iter().find(|(k, _)| k == "request").map(|(_, v)| v).unwrap();
    assert_eq!(request.as_array().map(|a| a.len()), Some(2), "cancel request array");
}

#[test]
fn stop_order_maps_to_conditional_trigger() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    let client = BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: BatchStub { last_body: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };
    // bracket stop-loss on a LONG (exit side SELL) → fires as price FALLS → triggerDirection 2
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "sl1", "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap();
    let params = client.build_order_params(&stop); // pure — no transport call
    let get = |k: &str| params.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.clone());
    assert_eq!(get("orderType"), Some(serde_json::json!("Market")), "conditional fires as Market");
    assert!(get("triggerPrice").is_some(), "triggerPrice = trigger");
    assert_eq!(get("triggerDirection"), Some(serde_json::json!(2)), "SELL stop → falls");
    assert_eq!(get("reduceOnly"), Some(serde_json::json!(true)));
    // byte-identity pin: NO requested trigger_by keeps the historical LastPrice hardcode
    assert_eq!(get("triggerBy"), Some(serde_json::json!("LastPrice")), "None = LastPrice, as ever");
}

/// A requested `trigger_by` replaces the historical LastPrice hardcode with the venue string of
/// the ONE trigger-source authority row (`vike_bridge_core::trigger::venue_trigger_by`) — V5
/// expresses all three sources, so bybit never denies one.
#[test]
fn stop_order_honors_requested_trigger_by() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    let client = BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: BatchStub { last_body: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    };
    for (tb, wire) in [
        (vike_model::TriggerBy::Last, "LastPrice"),
        (vike_model::TriggerBy::Mark, "MarkPrice"),
        (vike_model::TriggerBy::Index, "IndexPrice"),
    ] {
        let mut stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
            "client_order_id": "sl1", "venue": "bybit", "symbol": "BTCUSDT",
            "side": -1, "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0,
            "reduce_only": true
        }))
        .unwrap();
        stop.trigger_by = Some(tb);
        let params = client.build_order_params(&stop); // pure — no transport call
        let get = |k: &str| params.iter().find(|(pk, _)| *pk == k).map(|(_, v)| v.clone());
        assert_eq!(get("triggerBy"), Some(serde_json::json!(wire)), "{tb:?}");
        // and the field never leaks onto non-stop params
        let mut limit = stop.clone();
        limit.order_type = "limit".into();
        limit.price = Some(50000.0);
        limit.trigger_price = None;
        let lp = client.build_order_params(&limit);
        assert!(lp.iter().all(|(k, _)| *k != "triggerBy"), "{tb:?}: no triggerBy on limit");
    }
}

/// Audit T1 (Bybit): a submit whose POST returns the ambiguous-timeout sentinel must re-query order
/// status and resolve live-vs-dead instead of a false terminal. Re-query is TWO-STAGE: `/v5/order/
/// realtime` (open + recently-closed) FIRST, then `/v5/order/history` (closed/filled) as a FALLBACK
/// when realtime is empty — a filled-and-closed order that dropped off realtime's short retention
/// window would otherwise phantom-reject a REAL live position. Bybit signals "absent" with an EMPTY
/// result.list (no error code), so only BOTH endpoints empty concludes absent.
struct BybitTimeoutStub {
    realtime: Requery,
    history: Requery,
}

/// One re-query endpoint's scripted response.
enum Requery {
    /// Ok envelope whose `result.list` is these rows (an empty array = venue-confirmed absent).
    Rows(serde_json::Value),
    /// The re-query itself times out (Err `E_TIMEOUT_AMBIGUOUS`) → inconclusive.
    Timeout,
    /// Must NOT be reached — panics if the adapter queries this endpoint (proves it is skipped).
    Never,
}

impl Requery {
    fn respond(&self, what: &str) -> Result<serde_json::Value, VenueApiError> {
        match self {
            Requery::Rows(list) => {
                Ok(serde_json::json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
            }
            Requery::Timeout => Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "network error: timeout".into(),
            }),
            Requery::Never => panic!("{what} must NOT be re-queried"),
        }
    }
}

impl BybitTransport for BybitTimeoutStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &BybitV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/order/create") && method.eq_ignore_ascii_case("POST") {
            return Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "network error: timeout".into(),
            });
        }
        if path.ends_with("/order/realtime") && method.eq_ignore_ascii_case("GET") {
            return self.realtime.respond("realtime");
        }
        if path.ends_with("/order/history") && method.eq_ignore_ascii_case("GET") {
            return self.history.respond("history");
        }
        panic!("unexpected {method} {path}")
    }
}

fn bybit_t1_client(realtime: Requery, history: Requery) -> BybitPerpRest<BybitTimeoutStub> {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    BybitPerpRest {
        signer: BybitV5Signer::new(&creds(&fx), || 0),
        transport: BybitTimeoutStub { realtime, history },
        base_url: "https://stub".into(),
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

fn bybit_t1_req() -> vike_model::OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": "t1", "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

fn bybit_last_kind(ev: &[vike_model::events::Event]) -> &'static str {
    use vike_model::events::Event;
    match ev.last().unwrap() {
        Event::OrderAccepted(_) => "Accepted",
        Event::OrderRejected(_) => "Rejected",
        _ => "other",
    }
}

#[test]
fn bybit_timeout_submit_with_live_order_resolves_to_accepted() {
    // realtime HAS the order → Accepted; history is NEVER consulted (Requery::Never proves the
    // fallback is skipped once realtime is authoritative — no extra REST call on the common path).
    let client = bybit_t1_client(
        Requery::Rows(serde_json::json!([{"orderId": "bb-777", "orderLinkId": "t1"}])),
        Requery::Never,
    );
    assert_eq!(bybit_last_kind(&client.submit_order(&bybit_t1_req())), "Accepted");
}

#[test]
fn bybit_timeout_submit_realtime_empty_history_filled_resolves_to_accepted() {
    // The dangerous class this fix closes: realtime empty (the order FILLED-AND-CLOSED just outside
    // its retention window) but history HAS it → managed OrderAccepted carrying the venue id
    // recovered from history (its fill follows on the user-data / A3 lane), NOT a phantom reject of
    // a REAL live position.
    let client = bybit_t1_client(
        Requery::Rows(serde_json::json!([])),
        Requery::Rows(serde_json::json!([
            {"orderId": "bb-888", "orderLinkId": "t1", "orderStatus": "Filled"}
        ])),
    );
    let ev = client.submit_order(&bybit_t1_req());
    assert_eq!(bybit_last_kind(&ev), "Accepted", "history-confirmed order must NOT be rejected");
    match ev.last().unwrap() {
        vike_model::events::Event::OrderAccepted(a) => {
            assert_eq!(
                a.venue_order_id.as_deref(),
                Some("bb-888"),
                "venue id recovered from history"
            )
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
}

#[test]
fn bybit_timeout_submit_venue_absent_resolves_to_rejected() {
    // BOTH realtime AND history empty = Bybit confirms the order never landed → true terminal reject.
    let client =
        bybit_t1_client(Requery::Rows(serde_json::json!([])), Requery::Rows(serde_json::json!([])));
    assert_eq!(bybit_last_kind(&client.submit_order(&bybit_t1_req())), "Rejected");
}

#[test]
fn bybit_timeout_submit_double_timeout_is_optimistic() {
    // the realtime re-query itself times out → inconclusive → optimistic Accepted; the `?` short-
    // circuits before history, so Requery::Never proves the fallback does NOT fire on an Err.
    let client = bybit_t1_client(Requery::Timeout, Requery::Never);
    assert_eq!(bybit_last_kind(&client.submit_order(&bybit_t1_req())), "Accepted");
}

/// Audit T1 (Bybit) regression: a definite failure (business retCode, or code 0 connect-refused)
/// must stay a terminal reject and must NOT re-query — the stub panics on any GET to prove it.
#[test]
fn bybit_definite_failure_never_requeries() {
    struct RejectStub(i64);
    impl BybitTransport for RejectStub {
        fn signed(
            &self,
            _b: &str,
            path: &str,
            method: &str,
            _p: &[(&str, serde_json::Value)],
            _s: &BybitV5Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            if path.ends_with("/order/create") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query ({method} {path})");
        }
    }
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    for code in [0_i64, 110007] {
        let client = BybitPerpRest {
            signer: BybitV5Signer::new(&creds(&fx), || 0),
            transport: RejectStub(code),
            base_url: "https://stub".into(),
            symbol: "BTCUSDT".into(),
            properties: instruments["BTCUSDT"].properties,
            leverage: 2.0,
        };
        assert_eq!(
            bybit_last_kind(&client.submit_order(&bybit_t1_req())),
            "Rejected",
            "code {code} must stay terminal"
        );
    }
}
