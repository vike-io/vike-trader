//! R6 slice-5 golden parity: the OKX V5 adapter against the FROZEN `fixtures/r6/okx.json` bytes —
//! REST signer (ISO-ms ts + base64 over ts+METHOD+path+body), WS login/subscribe frames, the
//! single-orders-channel mappers (incl. -fillFee inversion, liq-category gating, contracts→base
//! enrichment), `to_contracts` (one of the pinned Decimal wire sites), bills funding decoder,
//! instruments, typed order params, and the reconcile matrix incl. NET SHORT + hedge, read through
//! the `ReconcileClient` production runs.
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
use vike_model::f64_to_hex_bits;
use vike_okx::execution::event_mapper::{map_okx_order, map_okx_perp, map_okx_private};
use vike_okx::execution::funding::decode_okx_funding_bills;
use vike_okx::execution::rest::{VenueRestOkx, parse_okx_perp_instruments};
use vike_okx::signer::OkxV5Signer;
use vike_okx::transport::ws_auth::{build_login_frame, build_subscribe_frame, okx_websocket_sign};
use vike_secrets::credentials::Credentials;

pub(crate) fn fixture() -> serde_json::Value {
    execution_drive::r6_fixture(env!("CARGO_MANIFEST_DIR"), "okx.json")
}

fn creds(fx: &serde_json::Value) -> Credentials {
    Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string().into(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string().into(),
        passphrase: Some(fx["passphrase"].as_str().unwrap().to_string().into()),
    }
}

/// The `BTC-USDT-SWAP` client every stubbed test drives: the fixture's signer and instrument,
/// leverage 2, no broker code. Only the TRANSPORT differs from one test to the next.
pub(crate) fn okx_perp<T: RestTransportJson>(
    fx: &serde_json::Value,
    transport: T,
) -> VenueRestOkx<T> {
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
    let instrument = &instruments["BTC-USDT-SWAP"];
    VenueRestOkx {
        signer: OkxV5Signer::new(&creds(fx), || 0),
        transport,
        base_url: "https://stub".into(),
        symbol: "BTC-USDT-SWAP".into(),
        properties: instrument.properties,
        ct_val: instrument.ct_val,
        leverage: 2.0,
        broker_code: None,
    }
}

#[test]
fn rest_signer_byte_identical() {
    let fx = fixture();
    let now = fx["now_ms"].as_i64().unwrap();
    for (i, case) in fx["rest_signer"].as_array().unwrap().iter().enumerate() {
        let signer = OkxV5Signer::new(&creds(&fx), move || now);
        signer.set_offset_ms(-55);
        let owned = execution_drive::typed_pairs(&case["params"]);
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
fn websocket_auth_parity() {
    let fx = fixture();
    let ws = &fx["ws_auth"];
    for case in ws["sign"].as_array().unwrap() {
        assert_eq!(
            okx_websocket_sign(fx["api_secret"].as_str().unwrap(), case["ts"].as_str().unwrap()),
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
    assert_eq!(build_subscribe_frame("orders", "SWAP"), ws["subscribe_frame"], "subscribe frame");
}

#[test]
fn mappers_parity() {
    let fx = fixture();
    for (i, case) in fx["spot_mapper"].as_array().unwrap().iter().enumerate() {
        execution_drive::assert_events_json(
            map_okx_private(&case["frame"], "okx", "BTC-USDT-SWAP"),
            &case["events"],
            &format!("spot_mapper[{i}]"),
        );
    }
    for (i, case) in fx["perp_mapper"].as_array().unwrap().iter().enumerate() {
        execution_drive::assert_events_json(
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

impl RestTransportJson for Stub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
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
        } else {
            panic!("unexpected path {path}")
        }
    }

    vike_bridge_core::no_public_call!();
}

// `perp_open_order_mapping` retired (unit A2): `map_okx_open_order` was the row map of the legacy
// `reconcile_positions` snapshot, deleted with the dead `Command::ApplySnapshot` path; production
// reads open orders as reports (`crates/bridges/okx/src/reconcile/mod.rs`'s `parse_orders_pending`).

#[test]
fn instruments_contracts_and_params_parity() {
    let fx = fixture();
    let instruments = parse_okx_perp_instruments(&fx["instruments_payload"]);
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
            ("ct_val", instrument.ct_val),
            ("ct_mult", instrument.ct_mult),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{symbol}.{name}");
        }
        assert_eq!(instrument.base_asset, w["base_asset"].as_str().unwrap());
    }

    let make = |rows: serde_json::Value, fails: bool| {
        okx_perp(
            &fx,
            Stub { position_rows: rows, balance: fx["balance_resp"].clone(), balance_fails: fails },
        )
    };

    // to_contracts — THE second Decimal wire site, bit-for-bit
    let client = make(serde_json::json!([]), false);
    for case in fx["to_contracts"].as_array().unwrap() {
        let base_quantity = case["base_qty"].as_f64().unwrap();
        assert_eq!(
            f64_to_hex_bits(client.to_contracts(base_quantity)),
            case["contracts"].as_str().unwrap(),
            "to_contracts({base_quantity})"
        );
    }

    // set-leverage: any '0' code path succeeds (no benign swallow — stub returns '0')
    client.set_leverage().expect("set_leverage");

    // typed order params
    let limit_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6okxL", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": -1, "qty": 0.0155, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "vtr6okxM", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": 1, "qty": 0.002, "order_type": "market", "reduce_only": true
    }))
    .unwrap();
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&limit_request, None)
        .expect("unset margin_mode always builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, execution_drive::typed_pairs(&fx["order_params"]["limit"]), "limit params");
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&market_request, None)
        .expect("unset margin_mode always builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got, execution_drive::typed_pairs(&fx["order_params"]["market"]), "market params");

    // margin-mode step-2 (the un-hardcode): the GOLDEN byte-identity pins. An UNSET margin_mode
    // (every golden fixture) and an explicit Cross both produce the historical `tdMode:"cross"`
    // golden bytes; Isolated flips ONLY the tdMode pair; Cash is denied before the wire
    // (`swap_td_mode` Err — surfaced as a terminal OrderRejected by the submit paths).
    let mut cross_request = limit_request.clone();
    cross_request.margin_mode = Some(vike_model::MarginMode::Cross);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&cross_request, None)
        .expect("cross builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(
        got,
        execution_drive::typed_pairs(&fx["order_params"]["limit"]),
        "explicit Cross == golden bytes"
    );
    let mut iso_request = limit_request.clone();
    iso_request.margin_mode = Some(vike_model::MarginMode::Isolated);
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&iso_request, None)
        .expect("isolated builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let want: Vec<(String, serde_json::Value)> =
        execution_drive::typed_pairs(&fx["order_params"]["limit"])
            .into_iter()
            .map(|(k, v)| if k == "tdMode" { (k, serde_json::json!("isolated")) } else { (k, v) })
            .collect();
    assert_eq!(got, want, "Isolated flips ONLY the tdMode pair vs the golden bytes");
    let mut cash_request = limit_request.clone();
    cash_request.margin_mode = Some(vike_model::MarginMode::Cash);
    assert!(
        client.build_order_params(&cash_request, None).is_err(),
        "Cash is denied on the SWAP adapter"
    );

    // tif step-2 (the okx flip): TIF rides `ordType` — the GOLDEN byte-identity pins. A
    // default/unset request (every golden fixture) and an explicit Gtc both yield the golden
    // bytes unchanged (`ordType:"limit"` — the venue default good-till-cancel rules); Ioc/Fok
    // flip ONLY the ordType pair to the `ioc`/`fok` ordTypes; a market request never reads the
    // TIF. Gtd/Day are denied at submit (`tests/offline/time_in_force_gate.rs`) and never reach this builder.
    let mut gtc_request = limit_request.clone();
    gtc_request.time_in_force = vike_model::TimeInForce::Gtc;
    let got: Vec<(String, serde_json::Value)> = client
        .build_order_params(&gtc_request, None)
        .expect("gtc builds")
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(
        got,
        execution_drive::typed_pairs(&fx["order_params"]["limit"]),
        "explicit Gtc == golden bytes"
    );
    assert_eq!(
        vike_venues::venue_time_in_force::venue_time_in_force("okx", vike_model::TimeInForce::Gtc),
        vike_venues::venue_time_in_force::TimeInForceOutcome::NotEmitted
    );
    for (tif, wire) in
        [(vike_model::TimeInForce::Ioc, "ioc"), (vike_model::TimeInForce::Fok, "fok")]
    {
        assert_eq!(
            vike_venues::venue_time_in_force::venue_time_in_force("okx", tif),
            vike_venues::venue_time_in_force::TimeInForceOutcome::Mapped(wire),
            "{tif:?}"
        );
        let mut req = limit_request.clone();
        req.time_in_force = tif;
        let got: Vec<(String, serde_json::Value)> = client
            .build_order_params(&req, None)
            .expect("mapped tif builds")
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let want: Vec<(String, serde_json::Value)> =
            execution_drive::typed_pairs(&fx["order_params"]["limit"])
                .into_iter()
                .map(|(k, v)| if k == "ordType" { (k, serde_json::json!(wire)) } else { (k, v) })
                .collect();
        assert_eq!(got, want, "{tif:?} flips ONLY the ordType pair vs the golden bytes");
        // market orders keep today's no-TIF shape (the table's resting-path scope)
        let mut mkt = market_request.clone();
        mkt.time_in_force = tif;
        let got: Vec<(String, serde_json::Value)> = client
            .build_order_params(&mkt, None)
            .expect("market builds")
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(
            got,
            execution_drive::typed_pairs(&fx["order_params"]["market"]),
            "market ignores {tif:?}"
        );
    }
    // The `reconcile` matrix this test also pinned through the legacy `reconcile_positions` snapshot
    // is retired (unit A2: that reader was deleted with the dead `Command::ApplySnapshot` path); the
    // frozen rows stay pinned through production's reader below.
}

/// The frozen `reconcile` matrix (net long / NET SHORT / hedge / flat / balance-fail) through the
/// reader production runs, `ReconcileClientOkx` (`crates/bridges/okx/src/reconcile/mod.rs`'s
/// `parse_positions` and `parse_balance`). Its `snapshot` bytes were exported from the legacy
/// snapshot reader (deleted in unit A2); where production's answer differs from them:
///
/// - **flat rows:** production keeps EVERY row, a flat (`pos` 0) leg included, because the reconcile
///   diff needs the row present (it synthesizes a flat row only when the venue lists none); the
///   frozen snapshot drops it (and holds one zero row when nothing is left). The live legs agree bit
///   for bit: contracts x `ctVal`, average price, side;
/// - **failed balance read:** production's read is an `Err`; the frozen snapshot booked 0.0;
/// - **mark price:** production carries none (a position report has no mark); the snapshot does;
/// - **symbol:** production names a row by the row's own `instId` (these frozen rows carry none, so
///   it is empty); the snapshot names the client's.
#[test]
fn production_reads_the_frozen_rows_through_the_reconcile_client() {
    use vike_execution::recon::ReconcileClient;
    use vike_model::events::PositionSide;
    let fx = fixture();
    let ct_val = parse_okx_perp_instruments(&fx["instruments_payload"])["BTC-USDT-SWAP"].ct_val;
    for case in fx["reconcile"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fails = case["balance_fails"].as_bool().unwrap();
        let rows = case["position_rows"].as_array().unwrap();
        let recon = vike_okx::ReconcileClientOkx::new(
            OkxV5Signer::new(&creds(&fx), || 0),
            Stub {
                position_rows: case["position_rows"].clone(),
                balance: fx["balance_resp"].clone(),
                balance_fails: fails,
            },
            "https://stub",
            "BTC-USDT-SWAP",
            ct_val,
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
        assert!(reports.iter().all(|r| r.symbol.is_empty()), "{name}: the row's own instId");
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

/// Stub for the native-modify path: records the params it received and acks (or rejects) the modify.
struct ModifyStub {
    fail: bool,
    seen: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl RestTransportJson for ModifyStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/amend-order"), "unexpected path {path}");
        *self.seen.borrow_mut() = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        if self.fail {
            Ok(serde_json::json!({"code": "1", "msg": "modify failed",
                "data": [{"ts": "1695190491502", "sCode": "51000", "sMsg": "order does not exist"}]}))
        } else {
            Ok(serde_json::json!({"code": "0", "msg": "",
                "data": [{"clOrdId": "vtModify", "ordId": "888", "ts": "1695190491421",
                    "sCode": "0", "sMsg": ""}]}))
        }
    }
    vike_bridge_core::no_public_call!();
}

#[test]
fn native_modify_maps_to_order_modified() {
    let fx = fixture();
    let make =
        |fail: bool| okx_perp(&fx, ModifyStub { fail, seen: std::cell::RefCell::new(Vec::new()) });

    // success: the venue acks → exactly one OrderModified carrying the venue ordId + new terms
    let ok = make(false);
    let event = ok.modify_order("vtModify", Some(0.5), Some(60000.0));
    assert_eq!(event.len(), 1);
    match &event[0] {
        vike_model::events::Event::OrderModified(a) => {
            assert_eq!(a.order_client_id, "vtModify");
            assert_eq!(a.venue_order_id.as_deref(), Some("888"));
            assert_eq!(a.new_qty, Some(0.5));
            assert_eq!(a.new_price, Some(60000.0));
            assert_eq!(
                a.timestamp, 1_695_190_491_421,
                "the reply's `ts`: when the venue finished the amend"
            );
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    // the request carried the venue-native newSz (contracts) + newPx (tick-rounded) fields
    let seen = ok.transport.seen.borrow();
    assert!(seen.iter().any(|(k, _)| k == "newSz"), "newSz sent: {seen:?}");
    assert!(seen.iter().any(|(k, _)| k == "newPx"), "newPx sent: {seen:?}");

    // failure: the venue rejects (sCode != 0) → the order keeps its terms, and the refusal is
    // surfaced as a NON-terminal OrderModifyRejected carrying the venue's sMsg (never swallowed)
    let bad = make(true);
    let event = bad.modify_order("vtModify", Some(0.5), None);
    assert_eq!(event.len(), 1, "a rejected modify surfaces exactly one advisory: {event:?}");
    match &event[0] {
        vike_model::events::Event::OrderModifyRejected(r) => {
            assert_eq!(r.order_client_id, "vtModify");
            assert_eq!(r.reason.as_str(), "order does not exist");
            assert_eq!(
                r.timestamp, 1_695_190_491_502,
                "the refusal row's `ts`, not the receipt time"
            );
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
}

/// Stub for the native ARRAY-body batch endpoints: records the body and returns a partial-batch
/// response (one accepted, one rejected) for batch-orders, and an idempotent ok for cancel-batch.
struct BatchStub {
    last_body: std::cell::RefCell<String>,
}

impl RestTransportJson for BatchStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, VenueApiError> {
        // the one read the cancel retry makes: `cancel-algos` said "unknown", the algo book is asked
        // once before that is read as gone — and it does not hold the order either
        if path.ends_with("/order-algo") {
            return Ok(serde_json::json!({"code": "51603", "msg": "", "data": []}));
        }
        panic!("batch must use signed_json, not signed ({path})")
    }
    fn signed_json(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        body: &str,
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/cancel-algos") {
            // the retry of the row the regular book did not know: unknown here too = gone
            return Ok(serde_json::json!({"code": "1", "msg": "", "data": [
                {"algoClOrdId": "b", "sCode": "51400", "sMsg": "unknown"}
            ]}));
        }
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
    vike_bridge_core::no_public_call!();
}

#[test]
fn native_batch_submit_and_cancel() {
    let fx = fixture();
    let make = || okx_perp(&fx, BatchStub { last_body: std::cell::RefCell::new(String::new()) });
    let req = |order_client_id: &str| -> vike_model::OrderRequest {
        serde_json::from_value(serde_json::json!({
            "order_client_id": order_client_id, "venue": "okx", "symbol": "BTC-USDT-SWAP",
            "side": 1, "qty": 0.002, "order_type": "limit", "price": 60000.0
        }))
        .unwrap()
    };

    // batch-submit: 2x OrderSubmitted up front, then per-order Accepted/Rejected from data[i].sCode
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
            assert_eq!(a.venue_order_id.as_deref(), Some("1001"));
            assert_eq!(r.order_client_id, "bat1");
            assert_eq!(r.reason, "param error");
        }
        other => panic!("unexpected tail {other:?}"),
    }
    // the body was a JSON ARRAY of 2 order objects (not the flat object `signed` builds)
    let body: serde_json::Value =
        serde_json::from_str(&client.transport.last_body.borrow()).unwrap();
    assert_eq!(body.as_array().map(|a| a.len()), Some(2), "array body");
}

/// Unified cross-venue attribution: a configured FD-broker code rides EVERY row of a batch submit as
/// `tag`, exactly as it rides a single order (`margin_mode_tests`' `tag_is_stamped_when_broker_code_...`);
/// with none configured no row carries the key at all.
#[test]
fn batch_rows_carry_the_broker_tag_when_one_is_configured() {
    let fx = fixture();
    let req = |order_client_id: &str| -> vike_model::OrderRequest {
        serde_json::from_value(serde_json::json!({
            "order_client_id": order_client_id, "venue": "okx", "symbol": "BTC-USDT-SWAP",
            "side": 1, "qty": 0.002, "order_type": "limit", "price": 60000.0
        }))
        .unwrap()
    };
    let rows_of = |code: Option<&str>| -> Vec<serde_json::Value> {
        let mut client =
            okx_perp(&fx, BatchStub { last_body: std::cell::RefCell::new(String::new()) });
        client.broker_code = code.map(str::to_string);
        client.submit_batch(&[req("bat0"), req("bat1")]);
        let body: serde_json::Value =
            serde_json::from_str(&client.transport.last_body.borrow()).unwrap();
        body.as_array().cloned().expect("array body")
    };
    let tagged = rows_of(Some("5328c82e5542BCDE"));
    assert_eq!(tagged.len(), 2);
    assert!(
        tagged.iter().all(|r| r["tag"] == "5328c82e5542BCDE"),
        "every row is tagged: {tagged:?}"
    );
    let plain = rows_of(None);
    assert_eq!(plain.len(), 2);
    assert!(plain.iter().all(|r| r.get("tag").is_none()), "no code, no key: {plain:?}");
}

/// Stub for the algo (conditional-stop) endpoint: records the params, returns an algoId.
struct AlgoStub {
    seen: std::cell::RefCell<Vec<(String, serde_json::Value)>>,
}

impl RestTransportJson for AlgoStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/order-algo"), "stops must route to order-algo ({path})");
        *self.seen.borrow_mut() = params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        Ok(serde_json::json!({"code": "0", "msg": "",
            "data": [{"algoId": "algo-1", "algoClOrdId": "sl1", "sCode": "0", "sMsg": ""}]}))
    }
    vike_bridge_core::no_public_call!();
}

#[test]
fn stop_order_routes_to_order_algo() {
    let client = okx_perp(&fixture(), AlgoStub { seen: std::cell::RefCell::new(Vec::new()) });
    let stop: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "sl1", "venue": "okx", "symbol": "BTC-USDT-SWAP",
        "side": -1, "qty": 0.01, "order_type": "stop", "trigger_price": 58000.0, "reduce_only": true
    }))
    .unwrap();
    // VenueRest::submit_order must route the stop to the algo endpoint (not /order)
    let event = client.submit_order(&stop);
    assert_eq!(event.len(), 2, "Submitted + Accepted");
    match (&event[0], &event[1]) {
        (
            vike_model::events::Event::OrderSubmitted(_),
            vike_model::events::Event::OrderAccepted(a),
        ) => {
            assert_eq!(a.order_client_id, "sl1");
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
impl RestTransportJson for OkxTimeoutStub {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, serde_json::Value)],
        _signer: &dyn SignerJson,
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
    vike_bridge_core::no_public_call!();
}

fn okx_t1_client(get: Option<Result<serde_json::Value, i64>>) -> VenueRestOkx<OkxTimeoutStub> {
    okx_perp(&fixture(), OkxTimeoutStub { get })
}

fn okx_t1_request() -> vike_model::OrderRequest {
    execution_drive::limit("okx", "t1", "BTC-USDT-SWAP", 0.01, 50000.0)
}

#[test]
fn okx_timeout_submit_with_live_order_resolves_to_accepted() {
    let client = okx_t1_client(Some(Ok(serde_json::json!({"ordId": "okx-777", "clOrdId": "t1"}))));
    assert_eq!(execution_drive::last_kind(&client.submit_order(&okx_t1_request())), "Accepted");
}

#[test]
fn okx_timeout_submit_venue_absent_says_nothing() {
    let client = okx_t1_client(Some(Err(51603))); // "Order does not exist", read just after the POST
    let event = client.submit_order(&okx_t1_request());
    assert_eq!(event.len(), 1, "only OrderSubmitted, the stuck-order ladder decides: {event:?}");
}

#[test]
fn okx_timeout_submit_double_timeout_says_nothing() {
    // inconclusive → nothing past `OrderSubmitted`, so the stuck-order ladder keeps the order.
    let client = okx_t1_client(None);
    let event = client.submit_order(&okx_t1_request());
    assert_eq!(event.len(), 1, "only OrderSubmitted: {event:?}");
}

/// Audit T1 (OKX) regression: a definite failure (business sCode, or code 0 connect) stays a
/// terminal reject and must NOT re-query — the stub panics on any GET to prove it.
#[test]
fn okx_definite_failure_never_requeries() {
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
            if path.ends_with("/trade/order") && method.eq_ignore_ascii_case("POST") {
                return Err(VenueApiError { code: self.0, msg: "boom".into() });
            }
            panic!("definite reject must NOT re-query ({method} {path})");
        }
        vike_bridge_core::no_public_call!();
    }

    let fx = fixture();
    for code in [0_i64, 51008] {
        let client = okx_perp(&fx, RejectStub(code));
        assert_eq!(
            execution_drive::last_kind(&client.submit_order(&okx_t1_request())),
            "Rejected",
            "code {code} must stay terminal"
        );
    }
}
