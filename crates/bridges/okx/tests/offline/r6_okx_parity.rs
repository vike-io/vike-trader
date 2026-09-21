//! R6 slice-5 golden parity: the OKX V5 adapter against the FROZEN `fixtures/r6/okx.json` bytes —
//! REST signer (ISO-ms ts + base64 over ts+METHOD+path+body), WS login/subscribe frames, the
//! single-orders-channel mappers (incl. -fillFee inversion, liq-category gating, contracts→base
//! enrichment), `to_contracts` (the second pinned Decimal wire site), bills funding decoder,
//! instruments, typed order params, and the reconcile matrix incl. NET SHORT + hedge.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use std::path::PathBuf;
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::OkxV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_model::f64_to_hex_bits;
use vike_okx::event_mapper::{map_okx_order, map_okx_perp, map_okx_private};
use vike_okx::funding::decode_okx_funding_bills;
use vike_okx::perp::{OkxPerpRest, map_okx_open_order, parse_okx_perp_instruments};
use vike_okx::transport::OkxTransport;
use vike_okx::ws_auth::{build_login_frame, build_subscribe_frame, okx_ws_sign};

fn fixture() -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6/okx.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn creds(fx: &serde_json::Value) -> Credentials {
    Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string(),
        passphrase: Some(fx["passphrase"].as_str().unwrap().to_string()),
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
fn rest_signer_byte_identical() {
    let fx = fixture();
    let now = fx["now_ms"].as_i64().unwrap();
    for (i, case) in fx["rest_signer"].as_array().unwrap().iter().enumerate() {
        let signer = OkxV5Signer::new(&creds(&fx), move || now);
        signer.set_offset_ms(-55);
        let owned = typed_pairs(&case["params"]);
        let params: Vec<(&str, serde_json::Value)> =
            owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let prepared = signer.prepare(
            &params,
            case["method"].as_str().unwrap(),
            case["path"].as_str().unwrap(),
        );
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
            okx_ws_sign(fx["api_secret"].as_str().unwrap(), case["ts"].as_str().unwrap()),
            case["signature"].as_str().unwrap()
        );
    }
    let login = build_login_frame(
        fx["api_key"].as_str().unwrap(),
        fx["api_secret"].as_str().unwrap(),
        fx["passphrase"].as_str().unwrap(),
        fx["now_ms"].as_i64().unwrap() / 1000,
    );
    assert_eq!(login, ws["login_frame"], "login frame");
    assert_eq!(build_subscribe_frame("SWAP"), ws["subscribe_frame"], "subscribe frame");
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
            map_okx_private(&case["frame"], "okx", "BTC-USDT-SWAP"),
            &case["events"],
            &format!("spot_mapper[{i}]"),
        );
    }
    for (i, case) in fx["perp_mapper"].as_array().unwrap().iter().enumerate() {
        assert_events(
            map_okx_perp(&case["frame"], "okx", "BTC-USDT-SWAP", 0.01),
            &case["events"],
            &format!("perp_mapper[{i}]"),
        );
    }
    // non-dict frames -> []
    assert!(map_okx_private(&serde_json::json!("pong"), "okx", "X").is_empty());
    assert!(map_okx_perp(&serde_json::json!(["x"]), "okx", "X", 0.01).is_empty());
}

#[test]
fn order_fill_surfaces_the_fill_fee_currency() {
    // OKX orders channel carries `fillFeeCcy` alongside the per-fill `fillFee`.
    let item = serde_json::json!({
        "state": "filled", "clOrdId": "c1", "tradeId": "t1", "instId": "BTC-USDT-SWAP",
        "side": "buy", "fillSz": "1", "fillPx": "100", "fillFee": "-0.1",
        "fillFeeCcy": "USDT", "execType": "T", "accFillSz": "1", "sz": "1"
    });
    let fill = map_okx_order(&item, "okx", "BTC-USDT-SWAP")
        .into_iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.1, "SIGNED fillFee -0.1 negated to +0.1 cost");
    assert_eq!(fill.commission_asset, "USDT");
}

#[test]
fn funding_bills_parity() {
    let fx = fixture();
    let bills: Vec<serde_json::Value> = fx["funding_bills"].as_array().unwrap().clone();
    let got: Vec<serde_json::Value> = decode_okx_funding_bills(&bills, "okx", "BTC-USDT-SWAP")
        .iter()
        .map(|e| {
            let mut v = serde_json::to_value(e).unwrap();
            v["type"] = serde_json::json!("FundingEvent");
            v
        })
        .collect();
    let want = fx["funding_events"].as_array().unwrap();
    assert_eq!(got.len(), want.len(), "funding count");
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "funding event");
    }
}

struct Stub {
    position_rows: serde_json::Value,
    balance: serde_json::Value,
    balance_fails: bool,
}

impl OkxTransport for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/positions") {
            Ok(serde_json::json!({"code": "0", "msg": "", "data": self.position_rows}))
        } else if path.ends_with("/balance") {
            if self.balance_fails {
                Err(VenueApiError { code: 0, msg: "balance down".into() })
            } else {
                Ok(self.balance.clone())
            }
        } else if path.ends_with("/set-leverage") {
            Ok(serde_json::json!({"code": "0", "msg": "", "data": [{"lever": "2"}]}))
        } else if path.ends_with("/orders-pending") {
            // Reconcile now also fetches resting orders (feeds apply_snapshot's stale-order reap);
            // this matrix pins the POSITION legs, so an empty resting set keeps the snapshot shape.
            Ok(serde_json::json!({"code": "0", "msg": "", "data": []}))
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

/// Reconcile open-order mapping: an `/orders-pending` row → a reconcile-seeded ACCEPTED
/// `ManagedOrder` (feeds `apply_snapshot`'s stale-order reap). `clOrdId` is our client id; `sz` is
/// in CONTRACTS so it converts to base via `ct_val`; a limit row keeps its price; a market row's ""
/// price maps to None.
#[test]
fn perp_open_order_mapping() {
    let ct_val = 0.01; // BTC-USDT-SWAP: 0.01 BTC per contract
    let limit = serde_json::json!({
        "clOrdId": "vperpL", "ordId": "6120ab", "instId": "BTC-USDT-SWAP",
        "side": "sell", "ordType": "limit", "sz": "3", "px": "65000.5"
    });
    let mo = map_okx_open_order(&limit, ct_val);
    assert_eq!(mo.client_order_id(), "vperpL");
    assert_eq!(mo.request.venue, "okx");
    assert_eq!(mo.request.symbol, "BTC-USDT-SWAP");
    assert_eq!(mo.request.side, -1, "sell → -1");
    assert_eq!(mo.request.qty, 3.0 * ct_val, "3 contracts × ct_val → base");
    assert_eq!(mo.request.order_type, "limit");
    assert_eq!(mo.request.price, Some(65000.5));
    assert_eq!(mo.status, vike_exec::OrderStatus::Accepted);
    assert_eq!(mo.venue_order_id.as_deref(), Some("6120ab"));

    let market = serde_json::json!({
        "clOrdId": "vperpM", "ordId": "6120ac", "instId": "BTC-USDT-SWAP",
        "side": "buy", "ordType": "market", "sz": "2", "px": ""
    });
    let mo = map_okx_open_order(&market, ct_val);
    assert_eq!(mo.request.side, 1, "buy → +1");
    assert_eq!(mo.request.order_type, "market");
    assert_eq!(mo.request.price, None, "market row empty px → None");
}

#[test]
fn instruments_contracts_params_and_reconcile_parity() {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
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
            ("ct_val", inst.ct_val),
            ("ct_mult", inst.ct_mult),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{sym}.{name}");
        }
        assert_eq!(inst.base_asset, w["base_asset"].as_str().unwrap());
    }

    let make = |rows: serde_json::Value, fails: bool| OkxPerpRest {
        signer: OkxV5Signer::new(&creds(&fx), || 0),
        transport: Stub {
            position_rows: rows,
            balance: fx["balance_resp"].clone(),
            balance_fails: fails,
        },
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: instruments["BTC-USDT-SWAP"].properties,
        ct_val: instruments["BTC-USDT-SWAP"].ct_val,
        leverage: 2.0,
        broker_code: None,
    };

    // to_contracts — THE second Decimal wire site, bit-for-bit
    let client = make(serde_json::json!([]), false);
    for case in fx["to_contracts"].as_array().unwrap() {
        let base_qty = case["base_qty"].as_f64().unwrap();
        assert_eq!(
            f64_to_hex_bits(client.to_contracts(base_qty)),
            case["contracts"].as_str().unwrap(),
            "to_contracts({base_qty})"
        );
    }

    // set-leverage: any '0' code path succeeds (no benign swallow — stub returns '0')
    client.set_leverage().expect("set_leverage");

    // typed order params
    let limit_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6okxL", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": -1, "qty": 0.0155, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "vtr6okxM", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": 1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&limit_req, None)
        .expect("unset margin_mode always builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&market_req, None)
        .expect("unset margin_mode always builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["market"]), "market params");

    // margin-mode step-2 (the un-hardcode): the GOLDEN byte-identity pins. An UNSET margin_mode
    // (every golden fixture) and an explicit Cross both produce the historical `tdMode:"cross"`
    // golden bytes; Isolated flips ONLY the tdMode pair; Cash is denied before the wire
    // (`swap_td_mode` Err — surfaced as a terminal OrderRejected by the submit paths).
    let mut cross_req = limit_req.clone();
    cross_req.margin_mode = Some(vike_model::MarginMode::Cross);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&cross_req, None)
        .expect("cross builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["limit"]), "explicit Cross == golden bytes");
    let mut iso_req = limit_req.clone();
    iso_req.margin_mode = Some(vike_model::MarginMode::Isolated);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&iso_req, None)
        .expect("isolated builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let want: Vec<(String, serde_json::Value)> = typed_pairs(&fx["order_params"]["limit"])
        .into_iter()
        .map(|(k, v)| if k == "tdMode" { (k, serde_json::json!("isolated")) } else { (k, v) })
        .collect();
    assert_eq!(got, want, "Isolated flips ONLY the tdMode pair vs the golden bytes");
    let mut cash_req = limit_req.clone();
    cash_req.margin_mode = Some(vike_model::MarginMode::Cash);
    assert!(
        client.build_order_params(&cash_req, None).is_err(),
        "Cash is denied on the SWAP adapter"
    );

    // tif step-2 (the okx flip): TIF rides `ordType` — the GOLDEN byte-identity pins. A
    // default/unset request (every golden fixture) and an explicit Gtc both yield the golden
    // bytes unchanged (`ordType:"limit"` — the venue default good-till-cancel rules); Ioc/Fok
    // flip ONLY the ordType pair to the `ioc`/`fok` ordTypes; a market request never reads the
    // TIF. Gtd/Day are denied at submit (`tests/offline/tif_gate.rs`) and never reach this builder.
    let mut gtc_req = limit_req.clone();
    gtc_req.time_in_force = vike_model::TimeInForce::Gtc;
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&gtc_req, None)
        .expect("gtc builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, typed_pairs(&fx["order_params"]["limit"]), "explicit Gtc == golden bytes");
    assert_eq!(
        vike_bridge_core::tif::venue_tif("okx", vike_model::TimeInForce::Gtc),
        vike_bridge_core::tif::TifOutcome::NotEmitted
    );
    for (tif, wire) in
        [(vike_model::TimeInForce::Ioc, "ioc"), (vike_model::TimeInForce::Fok, "fok")]
    {
        assert_eq!(
            vike_bridge_core::tif::venue_tif("okx", tif),
            vike_bridge_core::tif::TifOutcome::Mapped(wire),
            "{tif:?}"
        );
        let mut req = limit_req.clone();
        req.time_in_force = tif;
        let got: Vec<(String, serde_json::Value)> = client
            .build_order_params(&req, None)
            .expect("mapped tif builds")
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let want: Vec<(String, serde_json::Value)> = typed_pairs(&fx["order_params"]["limit"])
            .into_iter()
            .map(|(k, v)| if k == "ordType" { (k, serde_json::json!(wire)) } else { (k, v) })
            .collect();
        assert_eq!(got, want, "{tif:?} flips ONLY the ordType pair vs the golden bytes");
        // market orders keep today's no-TIF shape (the table's resting-path scope)
        let mut mkt = market_req.clone();
        mkt.time_in_force = tif;
        let got: Vec<(String, serde_json::Value)> = client
            .build_order_params(&mkt, None)
            .expect("market builds")
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(got, typed_pairs(&fx["order_params"]["market"]), "market ignores {tif:?}");
    }

    // reconcile matrix — net long / NET SHORT / hedge / flat / balance-fail
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let client = make(case["position_rows"].clone(), case["balance_fails"].as_bool().unwrap());
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

/// Stub for the native-modify path: records the params it received and acks (or rejects) the modify.
struct ModifyStub {
    fail: bool,
    seen: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl OkxTransport for ModifyStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/amend-order"), "unexpected path {path}");
        *self.seen.borrow_mut() = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        if self.fail {
            Ok(serde_json::json!({"code": "1", "msg": "modify failed",
                "data": [{"sCode": "51000", "sMsg": "order does not exist"}]}))
        } else {
            Ok(serde_json::json!({"code": "0", "msg": "",
                "data": [{"clOrdId": "vtModify", "ordId": "888", "sCode": "0", "sMsg": ""}]}))
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
fn native_modify_maps_to_order_modified() {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    let properties = instruments["BTC-USDT-SWAP"].properties;
    let ct_val = instruments["BTC-USDT-SWAP"].ct_val;
    let make = |fail: bool| OkxPerpRest {
        signer: OkxV5Signer::new(&creds(&fx), || 0),
        transport: ModifyStub { fail, seen: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties,
        ct_val,
        leverage: 2.0,
        broker_code: None,
    };

    // success: the venue acks → exactly one OrderModified carrying the venue ordId + new terms
    let ok = make(false);
    let ev = ok.modify_order("vtModify", Some(0.5), Some(60000.0));
    assert_eq!(ev.len(), 1);
    match &ev[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.client_order_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("888"));
            assert_eq!(a.new_qty, Some(0.5));
            assert_eq!(a.new_price, Some(60000.0));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    // the request carried the venue-native newSz (contracts) + newPx (tick-rounded) fields
    let seen = ok.transport.seen.borrow();
    assert!(seen.iter().any(|(k, _)| k == "newSz"), "newSz sent: {seen:?}");
    assert!(seen.iter().any(|(k, _)| k == "newPx"), "newPx sent: {seen:?}");

    // failure: the venue rejects (sCode != 0) → the order keeps its terms, NO event emitted
    let bad = make(true);
    assert!(
        bad.modify_order("vtModify", Some(0.5), None).is_empty(),
        "a rejected modify surfaces nothing (order unchanged)"
    );
}

/// Stub for the native ARRAY-body batch endpoints: records the body and returns a partial-batch
/// response (one accepted, one rejected) for batch-orders, and an idempotent ok for cancel-batch.
struct BatchStub {
    last_body: std::cell::RefCell<String>,
}

impl OkxTransport for BatchStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        panic!("batch must use signed_json, not signed ({path})")
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        body: &str,
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        *self.last_body.borrow_mut() = body.to_string();
        if path.ends_with("/batch-orders") {
            Ok(serde_json::json!({"code": "1", "msg": "", "data": [
                {"clOrdId": "bat0", "ordId": "1001", "sCode": "0", "sMsg": ""},
                {"clOrdId": "bat1", "ordId": "", "sCode": "51000", "sMsg": "param error"}
            ]}))
        } else if path.ends_with("/cancel-batch-orders") {
            Ok(serde_json::json!({"code": "0", "msg": "", "data": [
                {"clOrdId": "a", "sCode": "0"}, {"clOrdId": "b", "sCode": "51400"}
            ]}))
        } else {
            panic!("unexpected signed_json path {path}")
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
fn native_batch_submit_and_cancel() {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    let make = || OkxPerpRest {
        signer: OkxV5Signer::new(&creds(&fx), || 0),
        transport: BatchStub { last_body: std::cell::RefCell::new(String::new()) },
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: instruments["BTC-USDT-SWAP"].properties,
        ct_val: instruments["BTC-USDT-SWAP"].ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let req = |coid: &str| -> vike_model::OrderRequest {
        serde_json::from_value(serde_json::json!({
            "client_order_id": coid, "venue": "okx", "symbol": "BTC-USDT-SWAP",
            "side": 1, "qty": 0.002, "order_type": "limit", "price": 60000.0
        }))
        .unwrap()
    };

    // batch-submit: 2x OrderSubmitted up front, then per-order Accepted/Rejected from data[i].sCode
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
            assert_eq!(a.venue_order_id.as_deref(), Some("1001"));
            assert_eq!(r.client_order_id, "bat1");
            assert_eq!(r.reason, "param error");
        }
        other => panic!("unexpected tail {other:?}"),
    }
    // the body was a JSON ARRAY of 2 order objects (not the flat object `signed` builds)
    let body: serde_json::Value =
        serde_json::from_str(&client.transport.last_body.borrow()).unwrap();
    assert_eq!(body.as_array().map(|a| a.len()), Some(2), "array body");

    // batch-cancel: idempotent even with a per-order "already gone" (51400), body is an array
    let client = make();
    assert!(client.cancel_batch(&["a".into(), "b".into()]).is_ok());
    let body: serde_json::Value =
        serde_json::from_str(&client.transport.last_body.borrow()).unwrap();
    assert_eq!(body.as_array().map(|a| a.len()), Some(2), "cancel array body");
}

/// Stub for the algo (conditional-stop) endpoint: records the params, returns an algoId.
struct AlgoStub {
    seen: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl OkxTransport for AlgoStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/order-algo"), "stops must route to order-algo ({path})");
        *self.seen.borrow_mut() = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        Ok(serde_json::json!({"code": "0", "msg": "",
            "data": [{"algoId": "algo-1", "algoClOrdId": "sl1", "sCode": "0", "sMsg": ""}]}))
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
fn stop_order_routes_to_order_algo() {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    let client = OkxPerpRest {
        signer: OkxV5Signer::new(&creds(&fx), || 0),
        transport: AlgoStub { seen: std::cell::RefCell::new(Vec::new()) },
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: instruments["BTC-USDT-SWAP"].properties,
        ct_val: instruments["BTC-USDT-SWAP"].ct_val,
        leverage: 2.0,
        broker_code: None,
    };
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "sl1", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": -1, "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap();
    // VenueRest::submit_order must route the stop to the algo endpoint (not /order)
    let ev = client.submit_order(&stop);
    assert_eq!(ev.len(), 2, "Submitted + Accepted");
    match (&ev[0], &ev[1]) {
        (
            vike_model::events::Event::OrderSubmitted(_),
            vike_model::events::Event::OrderAccepted(a),
        ) => {
            assert_eq!(a.client_order_id, "sl1");
            assert_eq!(a.venue_order_id.as_deref(), Some("algo-1"), "venue id = algoId");
        }
        other => panic!("expected Submitted+Accepted, got {other:?}"),
    }
    let seen = client.transport.seen.borrow();
    assert!(
        seen.iter().any(|(k, _)| k == "slTriggerPx"),
        "conditional stop carries slTriggerPx: {seen:?}"
    );
    assert!(
        seen.iter().any(|(k, v)| k == "ordType" && v == &serde_json::json!("conditional")),
        "ordType conditional"
    );
}

/// Audit T1 (OKX): an ambiguous submit timeout must re-query GET /api/v5/trade/order by clOrdId
/// and resolve live-vs-dead instead of a false terminal. Absent = top-level code "51603".
struct OkxTimeoutStub {
    get: Option<Result<serde_json::Value, i64>>, // Some(Ok(data0)) live | Some(Err(code)) absent/business | None double-timeout
}
impl OkxTransport for OkxTimeoutStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &OkxV5Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/trade/order") && method.eq_ignore_ascii_case("POST") {
            return Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "network error: timeout".into(),
            });
        }
        if path.ends_with("/trade/order") && method.eq_ignore_ascii_case("GET") {
            return match &self.get {
                Some(Ok(d0)) => Ok(serde_json::json!({"code": "0", "msg": "", "data": [d0]})),
                Some(Err(code)) => {
                    Ok(serde_json::json!({"code": code.to_string(), "msg": "err", "data": []}))
                }
                None => Err(VenueApiError {
                    code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                    msg: "network error: timeout".into(),
                }),
            };
        }
        panic!("unexpected {method} {path}")
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

fn okx_t1_client(get: Option<Result<serde_json::Value, i64>>) -> OkxPerpRest<OkxTimeoutStub> {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    OkxPerpRest {
        signer: OkxV5Signer::new(&creds(&fx), || 0),
        transport: OkxTimeoutStub { get },
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: instruments["BTC-USDT-SWAP"].properties,
        ct_val: instruments["BTC-USDT-SWAP"].ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

fn okx_t1_req() -> vike_model::OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": "t1", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
    }))
    .unwrap()
}

fn okx_last_kind(ev: &[vike_model::events::Event]) -> &'static str {
    use vike_model::events::Event;
    match ev.last().unwrap() {
        Event::OrderAccepted(_) => "Accepted",
        Event::OrderRejected(_) => "Rejected",
        _ => "other",
    }
}

#[test]
fn okx_timeout_submit_with_live_order_resolves_to_accepted() {
    let client = okx_t1_client(Some(Ok(serde_json::json!({"ordId": "okx-777", "clOrdId": "t1"}))));
    assert_eq!(okx_last_kind(&client.submit_order(&okx_t1_req())), "Accepted");
}

#[test]
fn okx_timeout_submit_venue_absent_resolves_to_rejected() {
    let client = okx_t1_client(Some(Err(51603))); // "Order does not exist"
    assert_eq!(okx_last_kind(&client.submit_order(&okx_t1_req())), "Rejected");
}

#[test]
fn okx_timeout_submit_double_timeout_is_optimistic() {
    let client = okx_t1_client(None);
    assert_eq!(okx_last_kind(&client.submit_order(&okx_t1_req())), "Accepted");
}

/// Audit T1 (OKX) regression: a definite failure (business sCode, or code 0 connect) stays a
/// terminal reject and must NOT re-query — the stub panics on any GET to prove it.
#[test]
fn okx_definite_failure_never_requeries() {
    struct RejectStub(i64);
    impl OkxTransport for RejectStub {
        fn signed(
            &self,
            _b: &str,
            path: &str,
            method: &str,
            _p: &[(&str, serde_json::Value)],
            _s: &OkxV5Signer,
        ) -> Result<serde_json::Value, VenueApiError> {
            if path.ends_with("/trade/order") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query ({method} {path})");
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
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    for code in [0_i64, 51008] {
        let client = OkxPerpRest {
            signer: OkxV5Signer::new(&creds(&fx), || 0),
            transport: RejectStub(code),
            base_url: "https://stub".into(),
            symbol: "BTC-USDT-SWAP".into(),
            properties: instruments["BTC-USDT-SWAP"].properties,
            ct_val: instruments["BTC-USDT-SWAP"].ct_val,
            leverage: 2.0,
            broker_code: None,
        };
        assert_eq!(
            okx_last_kind(&client.submit_order(&okx_t1_req())),
            "Rejected",
            "code {code} must stay terminal"
        );
    }
}
