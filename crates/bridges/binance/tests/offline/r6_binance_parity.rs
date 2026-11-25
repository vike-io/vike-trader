//! R6 slice-1 golden parity: the Binance spot adapter against the FROZEN `fixtures/r6/*.json`
//! bytes — signer queries byte-for-byte, format_to_step strings, executionReport/WS-API mapper
//! events, order params, and the frozen reconcile rows read through the `ReconcileClient` production
//! runs.
//!
//! Those bytes were exported from the PySide6 vterminal app before it was retired: that is
//! PROVENANCE, not a live comparison. This header read "vs the Python oracle"; no exporter
//! survives in this tree and nothing here consults Python at run time, so what the exact
//! assertions below now claim is that THIS port's arithmetic has not moved unnoticed
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`).

use vike_binance::ReconcileClientBinance;
use vike_binance::execution::event_mapper::spot::{map_binance_private, map_execution_report};
use vike_binance::execution::rest::spot::{VenueRestBinanceSpot, parse_symbol_properties};
use vike_binance::family::signer::BinanceHmacSigner;
use vike_bridge_core::execution_drive::{assert_events_json, r6_fixture};
use vike_bridge_core::format::{format_to_step, format_to_step_f};
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_execution::recon::ReconcileClient;
use vike_model::{SymbolProperties, f64_to_hex_bits};
use vike_secrets::credentials::Credentials;

use crate::time_in_force_gate::NullSigner;

pub(crate) fn pairs(v: &serde_json::Value) -> Vec<(String, String)> {
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
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "signer.json");
    let creds = Credentials {
        api_key: fx["api_key"].as_str().unwrap().to_string().into(),
        api_secret: fx["api_secret"].as_str().unwrap().to_string().into(),
        passphrase: None,
    };
    let now = fx["now_ms"].as_i64().unwrap();
    for (index, case) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let signer = BinanceHmacSigner::new(&creds, move || now);
        signer.set_offset_ms(case["offset_ms"].as_i64().unwrap());
        let owned = pairs(&case["params"]);
        let prepared = signer.prepare(&as_ref_pairs(&owned), "POST", "");
        assert_eq!(prepared.query, case["query"].as_str().unwrap(), "case {index}: signed query");
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
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "format.json");
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

#[test]
fn mapper_execution_report_parity() {
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "mapper.json");
    for (i, case) in fx["execution_report"].as_array().unwrap().iter().enumerate() {
        // The mounted symbol, as every production caller passes it (the frames here are all BTCUSDT):
        // a fill on a DIFFERENT symbol carries a symbol-qualified trade id (`family::fill_trade_id`),
        // which the frozen expectations do not cover. The fixture itself is untouched.
        let got = map_execution_report(&case["frame"], "binance", "BTCUSDT", None);
        assert_events_json(got, &case["events"], &format!("execution_report[{i}]"));
    }
}

#[test]
fn mapper_private_dispatch_parity() {
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "mapper.json");
    for (i, case) in fx["private"].as_array().unwrap().iter().enumerate() {
        let got = map_binance_private(&case["frame"], "binance", "BTCUSDT", None);
        assert_events_json(got, &case["events"], &format!("private[{i}]"));
    }
}

#[test]
fn execution_report_surfaces_the_commission_asset() {
    // Binance executionReport carries `N` = commissionAsset alongside `n` = commission.
    let frame = serde_json::json!({
        "x": "TRADE", "X": "FILLED", "c": "coid1", "S": "BUY",
        "l": "1.0", "L": "100.0", "n": "0.5", "N": "BNB", "t": 42, "m": false
    });
    let got = map_execution_report(&frame, "binance", "BTCUSDT", None);
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
    exchange_info: serde_json::Value,
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
        if path.ends_with("/exchangeInfo") {
            return Ok(self.exchange_info.clone());
        }
        assert!(path.ends_with("/ticker/price"));
        Ok(self.ticker.clone())
    }
}

#[test]
fn properties_and_params_parity() {
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "reconcile.json");

    // parse_symbol_properties — bit-identical floats per symbol/field
    let filters = parse_symbol_properties(&fx["exchange_info"]);
    let want_properties = fx["parsed_filters"].as_object().unwrap();
    assert_eq!(filters.len(), want_properties.len());
    for (symbol_name, f) in &filters {
        let w = &want_properties[symbol_name];
        for (name, val) in [
            ("tick_size", f.tick_size),
            ("step_size", f.step_size),
            ("min_qty", f.min_qty),
            ("max_qty", f.max_qty),
            ("min_notional", f.min_notional),
        ] {
            assert_eq!(f64_to_hex_bits(val), w[name].as_str().unwrap(), "{symbol_name}.{name}");
        }
    }

    let client = VenueRestBinanceSpot {
        link_id: None,
        signer: NullSigner,
        transport: StubTransport {
            account: fx["account_resp"].clone(),
            open_orders: fx["open_orders_resp"].clone(),
            ticker: fx["ticker_resp"].clone(),
            exchange_info: fx["exchange_info"].clone(),
        },
        base_url: "https://stub".to_string(),
        symbol: "BTCUSDT".to_string(),
        properties: filters["BTCUSDT"],
        base_asset: "BTC".to_string(),
    };

    // build_order_params — ordered key/value parity (limit + market + cancel)
    let limit_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "deadbeef1", "venue": "binance", "symbol": "BTCUSDT",
        "side": 1, "qty": 0.30000000000000004, "order_type": "limit", "price": 62584.567
    }))
    .unwrap();
    let market_request: vike_model::OrderRequest = serde_json::from_value(serde_json::json!({
        "order_client_id": "deadbeef2", "venue": "binance", "symbol": "BTCUSDT",
        "side": -1, "qty": 0.12345678901, "order_type": "market"
    }))
    .unwrap();
    let got_limit: Vec<(String, String)> = client
        .build_order_params(&limit_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got_limit, pairs(&fx["order_params"]["limit"]), "limit params");
    let got_market: Vec<(String, String)> = client
        .build_order_params(&market_request)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    assert_eq!(got_market, pairs(&fx["order_params"]["market"]), "market params");
    // The `connect()` reconcile pin that stood here was retired (unit A2): that legacy snapshot
    // reader was deleted with the dead `Command::ApplySnapshot` path, and the same frozen rows stay
    // pinned through production's reader below.
    let _ = SymbolProperties::default(); // keep the type exercised in this test's public API
}

/// The frozen reconcile bytes through the reader production runs, the spot `ReconcileClientBinance`
/// (`crates/bridges/binance/src/family/recon.rs`'s `parse_spot_balance` and `parse_spot_open_orders`).
/// The fixture's `snapshot` block was exported for the legacy snapshot reader, deleted with the dead
/// `Command::ApplySnapshot` path (unit A2); it stays frozen, and production's answer relates to it as
/// follows:
///
/// - **balance:** the frozen `snapshot` holds 0.0; production reads the USDT row, free + locked;
/// - **position:** the frozen `snapshot` seeds the BASE coin (free 0.75 plus the 0.2 still locked by
///   resting SELLs) as a position at the ticker mark; production reports NO spot position at all;
/// - **open orders:** both read the three rows with the same id, order client id, side, kind and quantity;
///   production carries the filled quantity and no limit price (an `OrderStatusReport` has none), and
///   reads the rows' absent `status` word as live.
#[test]
fn production_reads_the_frozen_spot_rows_through_the_reconcile_client() {
    let fx = r6_fixture(env!("CARGO_MANIFEST_DIR"), "reconcile.json");
    let recon = ReconcileClientBinance::spot(
        NullSigner,
        StubTransport {
            account: fx["account_resp"].clone(),
            open_orders: fx["open_orders_resp"].clone(),
            ticker: fx["ticker_resp"].clone(),
            exchange_info: fx["exchange_info"].clone(),
        },
        "https://stub",
        "BTCUSDT",
    );
    let number = |v: &serde_json::Value| v.as_str().unwrap().parse::<f64>().unwrap();

    let usdt = &fx["account_resp"]["balances"][0];
    assert_eq!(usdt["asset"], "USDT");
    assert_eq!(
        recon.fetch_balance().unwrap().map(f64_to_hex_bits),
        Some(f64_to_hex_bits(number(&usdt["free"]) + number(&usdt["locked"]))),
        "production's spot balance is USDT free + locked"
    );
    assert_eq!(fx["snapshot"]["positions"][0][1], "3fee666666666666", "the snapshot's 0.95 BTC");
    // The spot rule: the pair's row is its BASE coin's wallet figure, free + locked (0.75 + 0.1), a `Cash`
    // floor row with no price, named by the fixture's `exchangeInfo` row (base coin BTC).
    let btc = &fx["account_resp"]["balances"][1];
    assert_eq!(btc["asset"], "BTC");
    let rows = recon.fetch_position_status_reports().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].symbol, "BTCUSDT");
    assert_eq!(rows[0].qty.to_bits(), (number(&btc["free"]) + number(&btc["locked"])).to_bits());
    assert_eq!((rows[0].average_price, rows[0].margin_mode), (0.0, vike_model::MarginMode::Cash));

    let reports = recon.fetch_order_status_reports(0).unwrap();
    let want = fx["snapshot"]["open_orders"].as_array().unwrap();
    let wire = fx["open_orders_resp"].as_array().unwrap();
    assert_eq!(reports.len(), want.len());
    for ((r, w), row) in reports.iter().zip(want).zip(wire) {
        assert_eq!(r.order_client_id.as_deref(), w["coid"].as_str());
        assert_eq!(Some(r.venue_order_id.as_str()), w["venue_order_id"].as_str());
        assert_eq!(i64::from(r.side), w["side"].as_i64().unwrap());
        assert_eq!(f64_to_hex_bits(r.qty), w["qty"].as_str().unwrap());
        assert_eq!(r.order_type.as_str(), w["order_type"].as_str().unwrap());
        assert_eq!(r.status.as_str(), w["status"].as_str().unwrap());
        assert_eq!(f64_to_hex_bits(r.filled_qty), f64_to_hex_bits(number(&row["executedQty"])));
    }
}
