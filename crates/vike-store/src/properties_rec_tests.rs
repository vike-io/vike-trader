use super::*;
use std::sync::Arc;
use vike_data::{HistStore, TsRange};
use vike_model::SymbolProperties;

fn tmp() -> (Arc<dyn HistStore + Send + Sync>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    (store as Arc<dyn HistStore + Send + Sync>, dir)
}

const NS_2020_01_01: i64 = 1_577_836_800_000_000_000; // 2020-01-01T00:00:00Z in ns

#[test]
fn disabled_recorder_writes_nothing() {
    let (store, _t) = tmp();
    let rec = PropertiesRecorder::new(store.clone(), false);
    rec.record(
        "bybit",
        "BTCUSDT",
        SymbolProperties { tick_size: 0.1, ..Default::default() },
        NS_2020_01_01,
    );
    assert!(store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap().is_empty());
}

#[test]
fn enabled_recorder_writes_one_row_at_ms() {
    let (store, _t) = tmp();
    let rec = PropertiesRecorder::new(store.clone(), true);
    let f = SymbolProperties {
        tick_size: 0.1,
        step_size: 0.01,
        min_qty: 0.01,
        max_qty: 0.0,
        min_notional: 5.0,
        contract_size: 0.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        asset_class: None,
    };
    rec.record("bybit", "BTCUSDT", f, NS_2020_01_01);
    let got = store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, f);
    assert_eq!(got[0].0, NS_2020_01_01 / 1_000_000, "ts stored in ms");
}

/// The recorder round-trip for a grid that HAS a contract size (the deribit case): it must
/// survive record → parquet → scan, since `make_engine`'s multiplier grid and the PIT
/// `properties_as_of` replay both read back through this path.
#[test]
fn recorder_round_trips_a_contract_size() {
    let (store, _t) = tmp();
    let rec = PropertiesRecorder::new(store.clone(), true);
    let f = SymbolProperties {
        tick_size: 0.0005,
        step_size: 0.1,
        min_qty: 0.1,
        max_qty: 0.0,
        min_notional: 0.0,
        contract_size: 10.0,
        tick_scheme: None,
        taker_hold_ms: 0,
        // BTC-PERPETUAL, and the class is the point of carrying it here: the recorder must
        // round-trip it through parquet like any other column (0061 STEP 2).
        asset_class: Some(vike_model::AssetClass::CryptoPerp),
    };
    rec.record("deribit", "BTC-PERPETUAL", f, NS_2020_01_01);
    let got = store.scan_symbol_properties("deribit", "BTC-PERPETUAL", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, f, "the whole grid, contract_size and asset_class included");
    assert_eq!(got[0].1.contract_size, 10.0);
    assert_eq!(got[0].1.multiplier(), 10.0);
    assert_eq!(
        got[0].1.asset_class,
        Some(vike_model::AssetClass::CryptoPerp),
        "the class survives record → parquet → scan"
    );
}

/// `open_properties_recorder` gates on its PARAMETER, before the store is touched.
#[test]
fn open_gates_on_its_parameter() {
    let dir = tempfile::tempdir().unwrap();
    let unused = dir.path().join("gate-off");
    assert!(open_properties_recorder(&unused, false).is_none(), "disabled → None");
    assert!(!unused.exists(), "and the store is never even opened/created when disabled");
    let rec = open_properties_recorder(dir.path(), true).expect("enabled → Some");
    assert!(rec.enabled(), "an enabled recorder");
}

/// `record_opt` forwards through a `Some` handle and is a total no-op on `None` — the exact
/// contract the bybit/okx/deribit adapters depend on (they only hold `Option<Arc<Self>>`).
#[test]
fn record_opt_forwards_some_and_noops_none() {
    let (store, _t) = tmp();
    let f = SymbolProperties { tick_size: 0.1, ..Default::default() };

    // None → nothing written.
    PropertiesRecorder::record_opt(&None, "bybit", "BTCUSDT", f, NS_2020_01_01);
    assert!(store.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap().is_empty());

    // Some(enabled) → one row, venue/symbol routed through.
    let rec = Some(Arc::new(PropertiesRecorder::new(store.clone(), true)));
    PropertiesRecorder::record_opt(&rec, "okx", "ETH-USDT-SWAP", f, NS_2020_01_01);
    let got = store.scan_symbol_properties("okx", "ETH-USDT-SWAP", TsRange::all()).unwrap();
    assert_eq!(got, vec![(NS_2020_01_01 / 1_000_000, f)]);
}

#[test]
fn record_all_one_row_per_entry_and_daily_idempotent() {
    let (store, _t) = tmp();
    let rec = PropertiesRecorder::new(store.clone(), true);
    let f = SymbolProperties { tick_size: 0.1, ..Default::default() };
    rec.record_all(
        "okx",
        [("BTC-USDT-SWAP".to_string(), f), ("ETH-USDT-SWAP".to_string(), f)],
        NS_2020_01_01,
    );
    rec.record_all(
        "okx",
        [("BTC-USDT-SWAP".to_string(), f)],
        NS_2020_01_01 + 3_600_000_000_000, // +1h same day
    );
    assert_eq!(
        store.scan_symbol_properties("okx", "BTC-USDT-SWAP", TsRange::all()).unwrap().len(),
        1
    );
    assert_eq!(
        store.scan_symbol_properties("okx", "ETH-USDT-SWAP", TsRange::all()).unwrap().len(),
        1
    );
}
