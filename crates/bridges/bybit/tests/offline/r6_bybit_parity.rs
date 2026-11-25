//! R6 slice-4 golden parity: the Bybit V5 adapter against the FROZEN `fixtures/r6/bybit.json`
//! bytes — REST signer (GET query + POST exact-body signing), WS auth frames, spot/perp mappers
//! (incl. BustTrade/AdlTrade liq + the Funding-row drop that routes around the execFee sign trap),
//! funding-settlement decoder, instruments, typed order params, and the reconcile matrix incl.
//! NET SHORT + hedge, read through the `ReconcileClient` production runs.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use vike_bridge_core::execution_drive;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::event_mapper::{map_bybit_perp, map_bybit_private, map_execution};
use vike_bybit::execution::funding::decode_bybit_funding_settlements;
use vike_bybit::execution::rest::{VenueRestBybit, parse_bybit_perp_instruments};
use vike_bybit::signer::BybitV5Signer;
use vike_bybit::transport::ws_auth::{
    build_auth_frame, build_subscribe_frame, bybit_websocket_sign,
};
use vike_model::f64_to_hex_bits;
use vike_secrets::credentials::Credentials;

pub(crate) fn fixture() -> serde_json::Value {
    execution_drive::r6_fixture(env!("CARGO_MANIFEST_DIR"), "bybit.json")
}

fn creds(fx: &serde_json::Value) -> Credentials {
    Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string().into(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string().into(),
        passphrase: None,
    }
}

/// The `BTCUSDT` client every stubbed test drives: the fixture's signer and instrument, and a
/// leverage of 2. Only the TRANSPORT differs from one test to the next.
pub(crate) fn bybit_perp<T: RestTransportJson>(
    fx: &serde_json::Value,
    transport: T,
) -> VenueRestBybit<T> {
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    VenueRestBybit {
        category: vike_bybit::history::Category::Linear,
        signer: BybitV5Signer::new(&creds(fx), || 0),
        transport,
        base_url: "https://stub".into(),
        mainnet: false,
        symbol: "BTCUSDT".into(),
        properties: instruments["BTCUSDT"].properties,
        leverage: 2.0,
    }
}

#[test]
fn rest_signer_query_and_body_byte_identical() {
    let fx = fixture();
    let now = fx["now_ms"].as_i64().unwrap();
    for (i, case) in fx["rest_signer"].as_array().unwrap().iter().enumerate() {
        let signer = BybitV5Signer::new(&creds(&fx), move || now);
        signer.set_offset_ms(-77); // matches the exporter
        let owned = execution_drive::typed_pairs(&case["params"]);
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
fn websocket_auth_parity() {
    let fx = fixture();
    let ws = &fx["ws_auth"];
    for case in ws["sign"].as_array().unwrap() {
        assert_eq!(
            bybit_websocket_sign(
                fx["api_secret"].as_str().unwrap(),
                case["expires"].as_i64().unwrap()
            ),
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

#[test]
fn mappers_parity() {
    let fx = fixture();
    for (i, case) in fx["spot_mapper"].as_array().unwrap().iter().enumerate() {
        execution_drive::assert_events_json(
            map_bybit_private(&case["frame"], "bybit", "BTCUSDT"),
            &case["events"],
            &format!("spot_mapper[{i}]"),
        );
    }
    for (i, case) in fx["perp_mapper"].as_array().unwrap().iter().enumerate() {
        execution_drive::assert_events_json(
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

impl RestTransportJson for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
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
        } else {
            panic!("unexpected path {path}")
        }
    }
}

// `perp_open_order_mapping` retired (unit A2): `map_bybit_open_order` was the row map of the legacy
// `reconcile_positions` snapshot, deleted with the dead `Command::ApplySnapshot` path; production
// reads open orders as reports (`crates/bridges/bybit/src/reconcile/mod.rs`'s `parse_orders`).

#[test]
fn instruments_and_params_parity() {
    let fx = fixture();
    let instruments = parse_bybit_perp_instruments(&fx["instruments_info"]);
    let want = fx["instruments"].as_object().unwrap();
    assert_eq!(instruments.len(), want.len());
    for (symbol, instrument) in &instruments {
        let w = &want[symbol];
        for (name, val) in [
            ("tick_size", instrument.properties.tick_size),
            ("step_size", instrument.properties.step_size),
            ("min_qty", instrument.properties.min_qty),
            ("max_qty", instrument.properties.max_qty),
            ("min_notional", instrument.properties.min_notional),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{symbol}.{name}");
        }
        assert_eq!(instrument.base_asset, w["base_asset"].as_str().unwrap());
    }

    let make = |rows: serde_json::Value, fails: bool| {
        bybit_perp(
            &fx,
            Stub { position_rows: rows, wallet: fx["wallet_resp"].clone(), wallet_fails: fails },
        )
    };

    // set-leverage: 110043 must be swallowed (benign "already at target")
    make(serde_json::json!([]), false).set_leverage().expect("110043 swallowed");

    // typed order params (bools/ints preserved)
    let limit_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6byL", "venue": "bybit", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.0025999, "order_type": "limit", "price": 61999.96
    }))
    .unwrap();
    let market_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6byM", "venue": "bybit", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let client = make(serde_json::json!([]), false);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&limit_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, execution_drive::typed_pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&market_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, execution_drive::typed_pairs(&fx["order_params"]["market"]), "market params");

    // tif step-2 (FLIPPED bybit row): the golden `limit` fixture uses the DEFAULT TIF (Gtc) —
    // its bytes above are already the byte-identity pin (timeInForce=GTC, same slot as the old
    // hardcode). A limit asking Ioc now yields the SAME golden bytes with ONLY the timeInForce
    // value swapped to "IOC" — the requested TIF reaches the wire, nothing else moves.
    let mut ioc_request = limit_request.clone();
    ioc_request.time_in_force = vike_model::TimeInForce::Ioc;
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&ioc_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let want_ioc: Vec<(String, serde_json::Value)> =
        execution_drive::typed_pairs(&fx["order_params"]["limit"])
            .into_iter()
            .map(|(k, v)| if k == "timeInForce" { (k, serde_json::json!("IOC")) } else { (k, v) })
            .collect();
    assert_eq!(got, want_ioc, "requested IOC must be on the wire, all other bytes unchanged");
    assert_eq!(
        vike_venues::venue_time_in_force::venue_time_in_force(
            "bybit",
            vike_model::TimeInForce::Ioc
        ),
        vike_venues::venue_time_in_force::TimeInForceOutcome::Mapped("IOC")
    );
    // The `reconcile` matrix this test also pinned through the legacy `reconcile_positions` snapshot
    // is retired (unit A2: that reader was deleted with the dead `Command::ApplySnapshot` path); the
    // frozen rows stay pinned through production's reader below.
}

/// The frozen `reconcile` matrix (net long / NET SHORT / hedge / flat / wallet-fail) through the
/// reader production runs, `ReconcileClientBybit` (`crates/bridges/bybit/src/reconcile/mod.rs`'s
/// `parse_positions` and `parse_wallet_balance`). Its `snapshot` bytes were exported from the legacy
/// snapshot reader (deleted in unit A2); where production's answer differs from them:
///
/// - **flat rows:** production keeps EVERY row, a flat (`size` 0) leg included, because the reconcile
///   diff needs the row present; the frozen snapshot drops it (and holds one zero row when nothing is
///   left). The live legs agree bit for bit: signed quantity, average price, side;
/// - **failed wallet read:** production's read is an `Err`; the frozen snapshot booked 0.0;
/// - **mark price:** production carries none (a position report has no mark); the snapshot does;
/// - **symbol:** production names a row of the venue symbol it read by the client's USER name (decision 0290)
///   and any other row by the row's own `symbol` (these frozen rows carry none, so it is empty); the snapshot
///   names the client's.
///
/// The client is on the USER name `BTCUSDT.P`, the kept row's linear `BTCUSDT` the frozen rows were read in.
#[test]
fn production_reads_the_frozen_rows_through_the_reconcile_client() {
    use vike_execution::recon::ReconcileClient;
    use vike_model::events::PositionSide;
    vike_bybit::history::kept_list_for_tests();
    let fx = fixture();
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fails = case["wallet_fails"].as_bool().unwrap();
        let rows = case["position_rows"].as_array().unwrap();
        let recon = vike_bybit::ReconcileClientBybit::new(
            BybitV5Signer::new(&creds(&fx), || 0),
            Stub {
                position_rows: case["position_rows"].clone(),
                wallet: fx["wallet_resp"].clone(),
                wallet_fails: fails,
            },
            "https://stub",
            "BTCUSDT.P",
        );
        let want = &case["snapshot"];

        let balance = recon.fetch_balance();
        if fails {
            assert!(balance.is_err(), "{name}: production fails the read: {balance:?}");
            assert_eq!(want["balance"], "0000000000000000", "{name}: the snapshot booked 0.0");
        } else {
            let got = balance.unwrap().map(f64_to_hex_bits);
            assert_eq!(got.as_deref(), want["balance"].as_str(), "{name}: balance");
        }

        let reports = recon.fetch_position_status_reports().unwrap();
        assert_eq!(reports.len(), rows.len(), "{name}: production keeps every row");
        assert!(reports.iter().all(|r| r.symbol.is_empty()), "{name}: the row's own symbol");
        let live: Vec<_> = reports.iter().filter(|r| r.qty != 0.0).collect();
        let legs: Vec<(&str, &str)> = want["positions"]
            .as_array()
            .unwrap()
            .iter()
            .zip(want["position_average_price"].as_array().unwrap())
            .map(|(q, a)| (q[1].as_str().unwrap(), a[1].as_str().unwrap()))
            .filter(|(q, _)| *q != "0000000000000000")
            .collect();
        assert_eq!(live.len(), legs.len(), "{name}: the live legs");
        let sides = want["position_sides"].as_array().unwrap();
        for (i, (r, (qty, avg))) in live.iter().zip(&legs).enumerate() {
            assert_eq!(f64_to_hex_bits(r.qty), *qty, "{name}: qty");
            assert_eq!(f64_to_hex_bits(r.average_price), *avg, "{name}: avg_px");
            let side: PositionSide = sides
                .get(i)
                .map_or(PositionSide::Both, |s| serde_json::from_value(s[1].clone()).unwrap());
            assert_eq!(r.position_side, side, "{name}: side");
        }
    }
}

/// Stub for the native-amend path: acks (or rejects via a non-zero retCode) `/v5/order/amend`.
struct ModifyStub {
    fail: bool,
}

impl RestTransportJson for ModifyStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/order/amend"), "unexpected path {path}");
        if self.fail {
            // the envelope a refusal comes in, as captured in
            // `crates/bridges/bybit/tests/fixtures/classes/create_refused_110101_inverse.json`
            Ok(serde_json::json!({"retCode": 110001, "retMsg": "order not exists or too late",
                "result": {}, "retExtInfo": {}, "time": 1_672_211_918_502_i64}))
        } else {
            Ok(serde_json::json!({"retCode": 0, "retMsg": "OK",
                "result": {"orderId": "bb-999", "orderLinkId": "vtModify"}, "time": 1_672_211_918_471_i64}))
        }
    }
}

#[test]
fn native_modify_maps_to_order_modified() {
    let fx = fixture();
    let make = |fail: bool| bybit_perp(&fx, ModifyStub { fail });

    // success: the venue acks → one OrderModified carrying the venue orderId + new terms
    let event = make(false).modify_order("vtModify", Some(0.01), Some(50000.0));
    assert_eq!(event.len(), 1);
    match &event[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.order_client_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("bb-999"));
            assert_eq!(a.new_qty, Some(0.01));
            assert_eq!(a.new_price, Some(50000.0));
            assert_eq!(
                a.timestamp, 1_672_211_918_471,
                "the envelope's `time`: the venue's stamp of its answer"
            );
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }

    // failure (retCode != 0) → the resting order keeps its terms, and the refusal is surfaced as a
    // NON-terminal OrderModifyRejected carrying the venue's reason (never swallowed)
    let event = make(true).modify_order("vtModify", Some(0.01), None);
    assert_eq!(event.len(), 1, "a rejected modify surfaces exactly one advisory: {event:?}");
    match &event[0] {
        vike_model::events::Event::OrderModifyRejected(r) => {
            assert_eq!(r.order_client_id, "vtModify");
            // The venue's words after the request and the code (`FailedRequest`): they were the words alone.
            let reason = r.reason.as_str();
            assert!(
                reason.starts_with(
                    "bybit POST /v5/order/amend (category linear, symbol BTCUSDT, orderLinkId vtModify, qty 0.01"
                ) && reason.ends_with("): venue error 110001, message \"order not exists or too late\""),
                "{reason}"
            );
            assert_eq!(
                r.timestamp, 1_672_211_918_502,
                "the refusal envelope's `time`, not the receipt time"
            );
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

/// Stub for the native batch endpoints: records the body, returns a partial-batch create response
/// (order 0 ok, order 1 rejected via retExtInfo) and an idempotent cancel-batch.
struct BatchStub {
    last_body: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl RestTransportJson for BatchStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
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
    let make = || bybit_perp(&fx, BatchStub { last_body: std::cell::RefCell::new(Vec::new()) });
    let req = |order_client_id: &str| -> vike_model::OrderRequest {
        serde_json::from_value(serde_json::json!({
            "order_client_id": order_client_id, "venue": "bybit", "symbol": "BTCUSDT",
            "side": 1, "qty": 0.01, "order_type": "limit", "price": 50000.0
        }))
        .unwrap()
    };

    // batch-submit: 2x OrderSubmitted, then per-order Accepted/Rejected from retExtInfo.list[i].code
    let client = make();
    let event = client.submit_batch(&[req("bat0"), req("bat1")]);
    let kinds: Vec<String> = event
        .iter()
        .map(|e| serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, vec!["OrderSubmitted", "OrderSubmitted", "OrderAccepted", "OrderRejected"]);
    match (&event[2], &event[3]) {
        (
            vike_model::events::Event::OrderAccepted(a),
            vike_model::events::Event::OrderRejected(r),
        ) => {
            assert_eq!(a.order_client_id, "bat0");
            assert_eq!(a.venue_order_id.as_deref(), Some("bb-1"));
            assert_eq!(r.order_client_id, "bat1");
            assert_eq!(
                r.reason,
                "bybit POST /v5/order/create-batch (category linear, symbol BTCUSDT, orderLinkId bat1): \
                 venue error 10001, message \"qty invalid\""
            );
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
}

#[test]
fn stop_order_maps_to_conditional_trigger() {
    let client =
        bybit_perp(&fixture(), BatchStub { last_body: std::cell::RefCell::new(Vec::new()) });
    // bracket stop-loss on a LONG (exit side SELL) → fires as price FALLS → triggerDirection 2
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "sl1", "venue": "bybit", "symbol": "BTCUSDT",
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
    let client =
        bybit_perp(&fixture(), BatchStub { last_body: std::cell::RefCell::new(Vec::new()) });
    for (tb, wire) in [
        (vike_model::TriggerBy::Last, "LastPrice"),
        (vike_model::TriggerBy::Mark, "MarkPrice"),
        (vike_model::TriggerBy::Index, "IndexPrice"),
    ] {
        let mut stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
            "order_client_id": "sl1", "venue": "bybit", "symbol": "BTCUSDT",
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
        limit.order_type = vike_model::OrderType::Limit;
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

impl RestTransportJson for BybitTimeoutStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
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

fn bybit_t1_client(realtime: Requery, history: Requery) -> VenueRestBybit<BybitTimeoutStub> {
    bybit_perp(&fixture(), BybitTimeoutStub { realtime, history })
}

fn bybit_t1_request() -> vike_model::OrderRequest {
    execution_drive::limit("bybit", "t1", "BTCUSDT", 0.01, 50000.0)
}

#[test]
fn bybit_timeout_submit_with_live_order_resolves_to_accepted() {
    // realtime HAS the order → Accepted; history is NEVER consulted (Requery::Never proves the
    // fallback is skipped once realtime is authoritative — no extra REST call on the common path).
    let client = bybit_t1_client(
        Requery::Rows(serde_json::json!([{"orderId": "bb-777", "orderLinkId": "t1"}])),
        Requery::Never,
    );
    assert_eq!(execution_drive::last_kind(&client.submit_order(&bybit_t1_request())), "Accepted");
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
    let event = client.submit_order(&bybit_t1_request());
    assert_eq!(
        execution_drive::last_kind(&event),
        "Accepted",
        "history-confirmed order must NOT be rejected"
    );
    match event.last().unwrap() {
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
fn bybit_timeout_submit_venue_absent_says_nothing() {
    // BOTH realtime AND history empty, read just after the POST: it may still land, so nothing is said
    // and the stuck-order ladder's confirm asks again.
    let client =
        bybit_t1_client(Requery::Rows(serde_json::json!([])), Requery::Rows(serde_json::json!([])));
    let event = client.submit_order(&bybit_t1_request());
    assert_eq!(event.len(), 1, "only OrderSubmitted: {event:?}");
}

#[test]
fn bybit_timeout_submit_double_timeout_says_nothing() {
    // the realtime re-query itself times out → inconclusive → nothing past `OrderSubmitted`, so the
    // stuck-order ladder keeps the order; the `?` short-circuits before history, so Requery::Never
    // proves the fallback does NOT fire on an Err.
    let client = bybit_t1_client(Requery::Timeout, Requery::Never);
    let event = client.submit_order(&bybit_t1_request());
    assert_eq!(event.len(), 1, "only OrderSubmitted: {event:?}");
}

/// Audit T1 (Bybit) regression: a definite failure (business retCode, or code 0 connect-refused)
/// must stay a terminal reject and must NOT re-query — the stub panics on any GET to prove it.
#[test]
fn bybit_definite_failure_never_requeries() {
    struct RejectStub(i64);
    impl RestTransportJson for RejectStub {
        fn signed(
            &self,
            _b: &str,
            path: &str,
            method: &str,
            _p: &[(&str, serde_json::Value)],
            _s: &dyn SignerJson,
        ) -> Result<serde_json::Value, VenueApiError> {
            if path.ends_with("/order/create") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query ({method} {path})");
        }
    }
    let fx = fixture();
    for code in [0_i64, 110007] {
        let client = bybit_perp(&fx, RejectStub(code));
        assert_eq!(
            execution_drive::last_kind(&client.submit_order(&bybit_t1_request())),
            "Rejected",
            "code {code} must stay terminal"
        );
    }
}
