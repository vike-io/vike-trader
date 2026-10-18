//! Multi-symbol bar slices: every symbol registers, a misaligned pick is a data error.

use std::assert_matches;
use std::sync::Arc;
use vike_data::{HistStore, StoreHandle, TsRange};
use vike_marketdata::Bar;
use vike_sim::EngineParams;
use vike_store::DataFusionHist;

use super::support::bar_at;
use crate::run::{DataSlice, RunError, load_slice_bars, run_slice};
use crate::spec::StrategySpec;

// ---- multi-symbol ------------------------------------------------------------------------

#[test]
fn multi_symbol_bar_slice_registers_every_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mk = |sym: &str, base: f64| -> Vec<Bar> {
        (0..50)
            .map(|i| {
                let c = base + (i % 7) as f64;
                Bar {
                    symbol: Some(sym.to_string()),
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    ..bar_at(60_000 * (i as i64 + 1))
                }
            })
            .collect()
    };
    store.append_bars("binance", "BTCUSDT", "1m", &mk("BTCUSDT", 100.0), None).unwrap();
    store.append_bars("binance", "ETHUSDT", "1m", &mk("ETHUSDT", 50.0), None).unwrap();
    let store: StoreHandle = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        "1m",
        TsRange::all(),
    );
    let series = load_slice_bars(&slice, &store).unwrap();
    assert_eq!(series.len(), 2);
    assert_eq!(series[0].0, "BTCUSDT");
    assert_eq!(series[1].0, "ETHUSDT");

    let res = run_slice(
        &StrategySpec::native_default("rotation_top_k"),
        &slice,
        &store,
        EngineParams::default(),
    )
    .expect("a two-symbol run should backtest");
    assert!(!res.equity_curve.is_empty());
}

/// Misaligned per-symbol bar counts are `StrategyEngine::new`'s own assert — surfaced as a
/// `RunError` here rather than panicking the worker thread.
#[test]
fn misaligned_multi_symbol_slice_is_a_data_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars_a: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    let bars_b: Vec<Bar> = (0..5).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
    store.append_bars("binance", "AAA", "1m", &bars_a, None).unwrap();
    store.append_bars("binance", "BBB", "1m", &bars_b, None).unwrap();
    let store: StoreHandle = Arc::new(store);

    let slice = DataSlice::multi_bars(
        "binance",
        vec!["AAA".to_string(), "BBB".to_string()],
        "1m",
        TsRange::all(),
    );
    assert_matches!(load_slice_bars(&slice, &store), Err(RunError::Data(_)));
}

#[test]
fn slice_label_and_symbol_accessor() {
    let s = DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all());
    assert_eq!(s.symbol(), "BTCUSDT");
    assert_eq!(s.label(), "binance · BTCUSDT · 1m");
    let t = DataSlice::ticks("polymarket", vec!["A".into(), "B".into()], TsRange::all());
    assert_eq!(t.label(), "polymarket · A+1 · ticks");
}
