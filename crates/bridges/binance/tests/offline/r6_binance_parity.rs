//! R6 slice-1 golden parity: the Binance spot adapter against the FROZEN `fixtures/r6/*.json`
//! bytes — signer queries byte-for-byte, format_to_step strings, executionReport/WS-API mapper
//! events, order params, and the connect() reconcile.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use std::path::PathBuf;
use vike_binance::event_mapper::{map_binance_private, map_execution_report};
use vike_binance::spot::{BinanceSpotRest, parse_symbol_properties};
use vike_bridge_core::credentials::Credentials;
use vike_bridge_core::format::{format_to_step, format_to_step_f};
use vike_bridge_core::signer::{BinanceHmacSigner, PreparedRequest, Signer};
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::{SymbolProperties, f64_to_hex_bits};

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../fixtures/r6").join(name);
    // ⚠ This said "run scripts/export_r6_fixtures.py", which is a command no checkout can run: the
    // exporter was deleted with the rest of the Python app and `fixtures/r6/` is now the oracle
    // itself (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). The dead path
    // is still NAMED — it is the evidence for where the bytes came from, and it is carried in
    // `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`.
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{path:?} missing — fixtures/r6 is COMMITTED and frozen; its exporter \
             (scripts/export_r6_fixtures.py) was deleted with the Python app, so restore the file \
             from git rather than regenerating it"
        )
    });
    serde_json::from_str(&text).unwrap()
}

fn pairs(v: &serde_json::Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
        .collect()
}

fn as_ref_pairs(owned: &[(String, String)]) -> Vec<(&str, String)> {
    owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect()
}

#[test]
fn signer_queries_byte_identical() {
    let fx = fixture("signer.json");
    let creds = Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string(),
        passphrase: None,
    };
    let now = fx["now_ms"].as_i64().unwrap();
    for (idx, case) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let signer = BinanceHmacSigner::new(&creds, move || now);
        signer.set_offset_ms(case["offset_ms"].as_i64().unwrap());
        let owned = pairs(&case["params"]);
        let prepared = signer.prepare(&as_ref_pairs(&owned), "POST", "");
        assert_eq!(prepared.query, case["query"].as_str().unwrap(), "case {idx}: signed query");
        let want_key = case["headers"]["X-MBX-APIKEY"].as_str().unwrap();
        assert_eq!(prepared.headers, vec![("X-MBX-APIKEY".to_string(), want_key.to_string())]);
    }
}

/// `format_to_step`/`format_to_step_f` against the frozen `fixtures/r6/format.json` strings — the
/// ONE pinned Decimal wire site.
///
/// Named `format_to_step_matches_python_decimal` until 2026-08-28. Those expected strings WERE
/// produced by the Python app's `decimal` module — that is where they came from — but nothing
/// re-derives them, so the old name promised a comparison this test cannot make. What a failure
/// here means is that this port's own formatting moved.
#[test]
fn format_to_step_matches_the_frozen_decimal_strings() {
    let fx = fixture("format.json");
    for case in fx["str_steps"].as_array().unwrap() {
        let got = format_to_step(case["value"].as_f64().unwrap(), case["step"].as_str().unwrap());
        assert_eq!(
            got,
            case["result"].as_str().unwrap(),
            "format_to_step({}, {:?})",
            case["value"],
            case["step"]
        );
    }
    for case in fx["float_steps"].as_array().unwrap() {
        let got =
            format_to_step_f(case["value"].as_f64().unwrap(), case["step_f"].as_f64().unwrap());
        assert_eq!(
            got,
            case["result"].as_str().unwrap(),
            "format_to_step_f({}, {})",
            case["value"],
            case["step_f"]
        );
    }
}

/// Rust events serialize to the same tagged-JSON shape the exporter wrote — compare as
/// Values (field-for-field, order-insensitive).
fn assert_events(got: Vec<vike_model::events::Event>, want: &serde_json::Value, what: &str) {
    let got_vals: Vec<serde_json::Value> =
        got.iter().map(|e| serde_json::to_value(e).unwrap()).collect();
    let want_vals = want.as_array().unwrap();
    assert_eq!(got_vals.len(), want_vals.len(), "{what}: event count");
    for (g, w) in got_vals.iter().zip(want_vals) {
        assert_eq!(g, w, "{what}");
    }
}

#[test]
fn mapper_execution_report_parity() {
    let fx = fixture("mapper.json");
    for (i, case) in fx["execution_report"].as_array().unwrap().iter().enumerate() {
        let got = map_execution_report(&case["frame"], "binance", "FALLBACK");
        assert_events(got, &case["events"], &format!("execution_report[{i}]"));
    }
}

#[test]
fn mapper_private_dispatch_parity() {
    let fx = fixture("mapper.json");
    for (i, case) in fx["private"].as_array().unwrap().iter().enumerate() {
        let got = map_binance_private(&case["frame"], "binance", "BTCUSDT");
        assert_events(got, &case["events"], &format!("private[{i}]"));
    }
}

#[test]
fn execution_report_surfaces_the_commission_asset() {
    // Binance executionReport carries `N` = commissionAsset alongside `n` = commission.
    let frame = serde_json::json!({
        "x": "TRADE", "X": "FILLED", "c": "coid1", "S": "BUY",
        "l": "1.0", "L": "100.0", "n": "0.5", "N": "BNB", "t": 42, "m": false
    });
    let got = map_execution_report(&frame, "binance", "BTCUSDT");
    let fill = got
        .iter()
        .find_map(|e| match e {
            vike_model::events::Event::OrderFilled(f) => Some(&f.fill),
            vike_model::events::Event::OrderPartiallyFilled(f) => Some(&f.fill),
            _ => None,
        })
        .expect("a fill event");
    assert_eq!(fill.commission, 0.5);
    assert_eq!(fill.commission_asset, "BNB");
}

/// Canned-JSON transport replaying the exporter's stub responses.
struct StubTransport {
    account: serde_json::Value,
    open_orders: serde_json::Value,
    ticker: serde_json::Value,
}

impl RestTransport for StubTransport {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        _params: &[(&str, String)],
        _signer: &dyn Signer,
    ) -> Result<serde_json::Value, VenueApiError> {
        if path.ends_with("/account") {
            Ok(self.account.clone())
        } else if path.ends_with("/openOrders") {
            Ok(self.open_orders.clone())
        } else {
            panic!("unexpected signed path {path}")
        }
    }

    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, VenueApiError> {
        assert!(path.ends_with("/ticker/price"));
        Ok(self.ticker.clone())
    }
}

struct NullSigner;
impl Signer for NullSigner {
    fn prepare(&self, _p: &[(&str, String)], _m: &str, _path: &str) -> PreparedRequest {
        PreparedRequest::default()
    }
}

#[test]
fn properties_params_and_reconcile_parity() {
    let fx = fixture("reconcile.json");

    // parse_symbol_properties — bit-identical floats per symbol/field
    let filters = parse_symbol_properties(&fx["exchange_info"]);
    let want_properties = fx["parsed_filters"].as_object().unwrap();
    assert_eq!(filters.len(), want_properties.len());
    for (sym, f) in &filters {
        let w = &want_properties[sym];
        for (name, val) in [
            ("tick_size", f.tick_size),
            ("step_size", f.step_size),
            ("min_qty", f.min_qty),
            ("max_qty", f.max_qty),
            ("min_notional", f.min_notional),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{sym}.{name}");
        }
    }

    let client = BinanceSpotRest {
        link_id: None,
        signer: NullSigner,
        transport: StubTransport {
            account: fx["account_resp"].clone(),
            open_orders: fx["open_orders_resp"].clone(),
            ticker: fx["ticker_resp"].clone(),
        },
        base_url: "https://stub".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: filters["BTCUSDT"],
        base_asset: "BTC".to_string(),
    };

    // build_order_params — ordered key/value parity (limit + market + cancel)
    let limit_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "deadbeef1", "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.30000000000000004, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_req: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "deadbeef2", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.12345678901, "order_type": "market"
    }))
    .unwrap();
    let got_limit: Vec<(String, String)> = client
        .build_order_params(&limit_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got_limit, pairs(&fx["order_params"]["limit"]), "limit params");
    let got_market: Vec<(String, String)> = client
        .build_order_params(&market_req)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got_market, pairs(&fx["order_params"]["market"]), "market params");

    // connect() reconcile — positions/avg/open-orders identical to the frozen `snapshot` block
    let snap = client.connect().unwrap();
    let want = &fx["snapshot"];
    let want_pos = want["positions"].as_array().unwrap();
    assert_eq!(snap.positions.len(), want_pos.len());
    for ((sym, qty), w) in snap.positions.iter().zip(want_pos) {
        assert_eq!(sym, w[0].as_str().unwrap());
        assert_eq!(f64_to_hex_bits(*qty), w[1].as_str().unwrap(), "position qty");
    }
    let want_avg = want["position_avg_px"].as_array().unwrap();
    for ((sym, px), w) in snap.position_avg_px.iter().zip(want_avg) {
        assert_eq!(sym, w[0].as_str().unwrap());
        assert_eq!(f64_to_hex_bits(*px), w[1].as_str().unwrap(), "avg px");
    }
    let want_orders = want["open_orders"].as_array().unwrap();
    assert_eq!(snap.open_orders.len(), want_orders.len());
    for (mo, w) in snap.open_orders.iter().zip(want_orders) {
        assert_eq!(mo.client_order_id(), w["coid"].as_str().unwrap());
        assert_eq!(mo.status.as_str(), w["status"].as_str().unwrap());
        assert_eq!(mo.venue_order_id.as_deref(), w["venue_order_id"].as_str());
        assert_eq!(i64::from(mo.request.side), w["side"].as_i64().unwrap());
        assert_eq!(f64_to_hex_bits(mo.request.qty), w["qty"].as_str().unwrap());
        assert_eq!(mo.request.order_type, w["order_type"].as_str().unwrap());
        match (mo.request.price, w["price"].as_str()) {
            (Some(p), Some(hex)) => assert_eq!(f64_to_hex_bits(p), hex, "order price"),
            (None, None) => {}
            (a, b) => panic!("price presence mismatch {a:?} vs {b:?}"),
        }
    }
    let _ = SymbolProperties::default(); // keep the type exercised in this test's public API
}
