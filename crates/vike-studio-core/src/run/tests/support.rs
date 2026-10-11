//! Fixtures two or more `run` test children share: the seeded 1m store and the strategy-spec helper.

use std::sync::Arc;
use vike_data::{DataFusionHist, HistStore, TsRange};
use vike_model::Bar;

use crate::run::{DataSlice, StoreHandle};
use crate::spec::StrategySpec;
use crate::test_support::walk::random_walk_bars;

pub(super) fn rhai(src: &str) -> StrategySpec {
    StrategySpec::rhai(src)
}

pub(super) fn seeded_store() -> (tempfile::TempDir, StoreHandle, Vec<Bar>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let bars = random_walk_bars(400, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store) as StoreHandle, bars)
}

pub(super) fn slice() -> DataSlice {
    DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all())
}

pub(super) fn bar_at(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
