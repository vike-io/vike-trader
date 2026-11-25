//! The forecasts document (book W35): written only when there is one, read back by a run directory
//! or by a run id through `finished_run_dir`.

use super::*;
use std::assert_matches;
use vike_model::{Forecast, ForecastRow};

fn a_row(timestamp: i64, value: f64) -> ForecastRow {
    ForecastRow {
        timestamp,
        symbol: "BTCUSDT".to_string(),
        forecast: Forecast::new(value, 3_600_000, Some(0.5)).unwrap(),
    }
}

#[test]
fn a_runs_forecasts_round_trip_and_a_run_with_none_writes_no_file() {
    let root = tempfile::tempdir().unwrap();
    let runs = root.path().join("runs");
    let run = create_run_dir(&runs, 1_756_000_000, None).unwrap();
    let written = RunForecasts {
        schema: FORECASTS_SCHEMA,
        forecasts: vec![a_row(1_000, 0.01), a_row(2_000, -0.02)],
        source_len: 2,
    };
    let extras = RunExtras { forecasts: Some(&written), ..Default::default() };
    write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();

    let back = read_forecasts(&run.path).unwrap();
    assert_eq!(back.forecasts, written.forecasts);
    assert_eq!((back.schema, back.source_len), (FORECASTS_SCHEMA, 2));
    // By run id, as a study's host reads it: the same directory, and only a finished run.
    let dir = finished_run_dir(&runs, &run.run_id).unwrap();
    assert_eq!(read_forecasts(&dir).unwrap().forecasts.len(), 2);

    let quiet = create_run_dir(&runs, 1_756_000_001, None).unwrap();
    write_run_with(&quiet.path, &a_manifest(&quiet.run_id), &json!({}), &RunExtras::default())
        .unwrap();
    assert!(!quiet.path.join(FORECASTS_FILE).exists(), "no forecast, no file");
    let err = read_forecasts(&quiet.path).unwrap_err();
    assert_matches!(err, RunReadError::Missing { .. }, "expected Missing, got {err:?}");
}

/// The forecasts are written BEFORE the completion marker, like every irreplaceable document.
#[test]
fn the_forecasts_are_written_before_the_manifest() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    // A directory where the manifest should go makes its write fail after every other document.
    std::fs::create_dir(run.path.join(MANIFEST_FILE)).unwrap();
    let written =
        RunForecasts { schema: FORECASTS_SCHEMA, forecasts: vec![a_row(1, 0.1)], source_len: 1 };
    let extras = RunExtras { forecasts: Some(&written), ..Default::default() };
    assert!(write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).is_err());
    assert!(run.path.join(FORECASTS_FILE).is_file(), "the forecasts reached disk first");
}
