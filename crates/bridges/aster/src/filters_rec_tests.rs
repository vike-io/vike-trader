//! PIT properties recording (Task 12): `record_properties_all` writes one row per symbol into
//! the store when a recorder is present, keyed by venue `"aster"`, over BOTH the spot
//! (`vike_binance::family::order_map::parse_symbol_properties`) and perp (`parse_aster_perp_instruments`, mapped to
//! `.properties`) parse outputs. Uses the DataFusion-free `MemHistStore` test double (vike-data's
//! `test-support` dev-feature) so this venue's default build never pulls DataFusion in.
use std::sync::Arc;

use indexmap::IndexMap;
use vike_data::HistStore;
use vike_model::SymbolProperties;

use crate::perp::parse_aster_perp_instruments;
use vike_binance::family::order_map::parse_symbol_properties;

const TS_NS: i64 = 1_577_836_800_000_000_000;

#[test]
fn aster_spot_records_all_properties() {
    let payload = serde_json::json!({
        "symbols": [
            {
                "symbol": "BTCUSDT",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                    {"filterType": "LOT_SIZE", "stepSize": "0.001", "minQty": "0.001", "maxQty": "9000.0"},
                    {"filterType": "NOTIONAL", "minNotional": "5.0"},
                ]
            },
            {
                "symbol": "ETHUSDT",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                    {"filterType": "LOT_SIZE", "stepSize": "0.01", "minQty": "0.01", "maxQty": "5000.0"},
                    {"filterType": "NOTIONAL", "minNotional": "10.0"},
                ]
            },
        ]
    });
    let filters = parse_symbol_properties(&payload);
    assert_eq!(filters.len(), 2, "sanity: canned exchangeInfo should parse to 2 symbols");

    let store = Arc::new(vike_data::MemHistStore::new());
    let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
    super::record_properties_all(&rec, filters.clone(), TS_NS);

    for (symbol, f) in &filters {
        assert_eq!(
            store.scan_symbol_properties("aster", symbol, vike_data::TsRange::all()).unwrap(),
            vec![(1_577_836_800_000i64, *f)],
            "symbol {symbol} should have one recorded row"
        );
    }
}

#[test]
fn aster_perp_records_all_properties() {
    let payload = serde_json::json!({
        "symbols": [
            {
                "symbol": "BTCUSDT",
                "baseAsset": "BTC",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.10"},
                    {"filterType": "LOT_SIZE", "stepSize": "0.001", "minQty": "0.001"},
                    {"filterType": "MARKET_LOT_SIZE", "maxQty": "1000.0"},
                    {"filterType": "MIN_NOTIONAL", "notional": "5.0"},
                ]
            },
            {
                "symbol": "ETHUSDT",
                "baseAsset": "ETH",
                "filters": [
                    {"filterType": "PRICE_FILTER", "tickSize": "0.01"},
                    {"filterType": "LOT_SIZE", "stepSize": "0.01", "minQty": "0.01"},
                    {"filterType": "MARKET_LOT_SIZE", "maxQty": "500.0"},
                    {"filterType": "MIN_NOTIONAL", "notional": "10.0"},
                ]
            },
        ]
    });
    let instruments = parse_aster_perp_instruments(&payload);
    assert_eq!(instruments.len(), 2, "sanity: canned exchangeInfo should parse to 2 symbols");

    let entries: IndexMap<String, SymbolProperties> =
        instruments.iter().map(|(sym, inst)| (sym.clone(), inst.properties)).collect();

    let store = Arc::new(vike_data::MemHistStore::new());
    let rec = Some(Arc::new(vike_data::PropertiesRecorder::new(store.clone(), true)));
    super::record_properties_all(&rec, entries.clone(), TS_NS);

    for (symbol, f) in &entries {
        assert_eq!(
            store.scan_symbol_properties("aster", symbol, vike_data::TsRange::all()).unwrap(),
            vec![(1_577_836_800_000i64, *f)],
            "symbol {symbol} should have one recorded row"
        );
    }
}

#[test]
fn aster_no_recorder_writes_nothing() {
    let store = Arc::new(vike_data::MemHistStore::new());
    let entries: IndexMap<String, SymbolProperties> =
        [("BTCUSDT".to_string(), SymbolProperties::default())].into_iter().collect();
    super::record_properties_all(&None, entries, TS_NS);
    assert!(
        store
            .scan_symbol_properties("aster", "BTCUSDT", vike_data::TsRange::all())
            .unwrap()
            .is_empty()
    );
}
