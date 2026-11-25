//! `kind=exec_forecast` HistoryStore series (book W35) — what live strategies published, materialized from the
//! journal. Both halves round-trip the same rows, a stated and an absent confidence included, under the commit-key
//! rule every append keeps; a range reads its window.

use vike_data::{ExecForecastRow, HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_store::HistoryStoreDataFusion;

fn forecast(ts: i64, mount_id: &str, value: f64, confidence: Option<f64>) -> ExecForecastRow {
    ExecForecastRow { ts, mount_id: mount_id.into(), value, horizon_ms: 3_600_000, confidence }
}

fn round_trip(store: &dyn HistoryStore) {
    let rows = vec![
        forecast(1, "m1", 0.02, None),
        forecast(2, "m2", -0.5, Some(0.75)),
        forecast(3, "m1", 0.0, Some(1.0)),
    ];
    assert_eq!(store.append_forecasts("binance", "BTCUSDT", &rows, Some("k")).unwrap(), 3);
    assert_eq!(
        store.append_forecasts("binance", "BTCUSDT", &rows, Some("k")).unwrap(),
        0,
        "same key"
    );
    assert_eq!(store.scan_forecasts("binance", "BTCUSDT", TimestampRange::all()).unwrap(), rows);
    let window = store.scan_forecasts("binance", "BTCUSDT", TimestampRange::of(2, 3)).unwrap();
    assert_eq!(window, rows[1..], "the range's rows, ts-ascending");
    assert!(store.scan_forecasts("binance", "ETHUSDT", TimestampRange::all()).unwrap().is_empty());
    // The bounded read the datahub's `StrategyForecasts` makes: a complete prefix of at least the budget.
    let head =
        store.scan_forecasts_capped("binance", "BTCUSDT", TimestampRange::all(), Some(1)).unwrap();
    assert!(!head.is_empty() && rows.starts_with(&head), "a prefix of the rows: {head:?}");
    assert_eq!(
        store.scan_forecasts_capped("binance", "BTCUSDT", TimestampRange::all(), None).unwrap(),
        rows,
        "no budget is the whole range"
    );
}

#[test]
fn forecast_series_roundtrips_datafusion() {
    let dir = tempfile::tempdir().unwrap();
    round_trip(&HistoryStoreDataFusion::open(dir.path()).unwrap());
}

#[test]
fn forecast_series_roundtrips_memory() {
    round_trip(&HistoryStoreMemory::new());
}
