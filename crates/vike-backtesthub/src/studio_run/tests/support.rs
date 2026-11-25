//! Fixtures two or more `studio_run` test children share: the seeded 1m store and the strategy-spec helper.

use std::sync::Arc;
use vike_data::{HistoryStore, TimestampRange};
use vike_marketdata::Bar;
use vike_store::HistoryStoreDataFusion;

use vike_studio_core::test_support::walk::random_walk_bars;
use vike_studio_core::{DataSlice, StrategySpec};

pub(super) fn rhai(src: &str) -> StrategySpec {
    StrategySpec::rhai(src)
}

pub(super) fn seeded_store() -> (tempfile::TempDir, Arc<dyn HistoryStore + Send + Sync>, Vec<Bar>) {
    let dir = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let bars = random_walk_bars(400, 60_000, Some("BTCUSDT"));
    store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    (dir, Arc::new(store) as Arc<dyn HistoryStore + Send + Sync>, bars)
}

pub(super) fn slice() -> DataSlice {
    DataSlice::bars("binance", "BTCUSDT", "1m", TimestampRange::all())
}

pub(super) fn bar_at(ts: i64) -> Bar {
    vike_marketdata::test_support::flat_bar_zero_volume(ts, 1.0)
}
