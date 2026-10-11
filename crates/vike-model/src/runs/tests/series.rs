//! The equity series and the trade ledger: decimation, the round trip, and the write ordering.

use super::*;
use std::assert_matches;

/// Under the cap nothing is touched: a stride of `1` is the claim "this is the whole curve",
/// and a reader keys on it to know whether a statistic recomputed from the file is exact.
#[test]
fn a_curve_that_fits_under_the_cap_is_kept_whole_at_stride_one() {
    let samples: Vec<f64> = (0..100).map(f64::from).collect();

    let (kept, stride) = decimate(&samples, 1_000);

    assert_eq!(kept, samples, "nothing may be dropped under the cap");
    assert_eq!(stride, 1, "stride 1 IS the exactness claim");
}

/// The bound is the whole point: a curve far over the cap comes back bounded, and the stride
/// says by how much it was thinned.
#[test]
fn a_curve_over_the_cap_is_thinned_to_the_cap_and_says_by_how_much() {
    let samples: Vec<u32> = (0..100_000).collect();

    let (kept, stride) = decimate(&samples, 1_000);

    assert!(stride > 1, "a thinned curve must not claim stride 1");
    assert!(
        kept.len() <= 1_001,
        "the cap is soft by exactly one — the appended last sample: {}",
        kept.len()
    );
    assert_eq!(kept[0], 0, "the first sample is the run's opening equity");
}

/// The LAST sample is the run's OUTCOME. A decimation that dropped it would make
/// `final_equity` derived from the file disagree with the one in `report.json`, which is the
/// single comparison a reader is most likely to make.
#[test]
fn the_final_sample_survives_a_stride_that_does_not_land_on_it() {
    // 8 samples into a cap of 3 gives stride 3: indices 0, 3, 6 — and 7 is the one that must
    // be appended, because the stride does not land on it.
    let samples: Vec<u32> = (0..8).collect();

    let (kept, stride) = decimate(&samples, 3);

    assert_eq!(stride, 3);
    assert_eq!(*kept.last().unwrap(), 7, "the last sample must survive: {kept:?}");
    assert_eq!(kept, vec![0, 3, 6, 7]);
}

/// A cap of zero is "keep nothing" — the shape a future `--keep-series none` spells — and it
/// reports stride `0` so a reader can tell it apart from an empty run.
#[test]
fn a_cap_of_zero_keeps_nothing_and_says_so_with_stride_zero() {
    let samples: Vec<u32> = (0..10).collect();

    let (kept, stride) = decimate(&samples, 0);

    assert!(kept.is_empty());
    assert_eq!(stride, 0, "stride 0 means NOTHING was kept, not `kept everything`");
}

/// An empty curve is a real run (a slice with no rows), not an error, and it claims exactness.
#[test]
fn an_empty_curve_is_exact_rather_than_thinned() {
    let samples: Vec<f64> = Vec::new();

    let (kept, stride) = decimate(&samples, 1_000);

    assert!(kept.is_empty());
    assert_eq!(stride, 1);
}

fn a_series() -> RunSeries {
    RunSeries {
        schema: SERIES_SCHEMA,
        equity: vec![10_000.0, 10_100.0, 9_950.0],
        equity_ts: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
        per_symbol_equity: vec![("BTCUSDT".to_string(), vec![0.0, 100.0, -50.0])],
        stride: 1,
        source_len: 3,
        diagnostics: RunDiagnostics {
            warmup: 20,
            intrabar_both_hit: 1,
            stale_deferrals: 2,
            impact_unpriced: 3,
            session_deferrals: 4,
            below_min_reversals: 5,
            maker_fills: 6,
            taker_fills: 7,
            fees_paid: 8.5,
            dropped: vec![DroppedOrder {
                symbol: "BTCUSDT".to_string(),
                reason: "risk_gate:max_notional".to_string(),
                size: 0.5,
                weight: 1.0,
            }],
        },
    }
}

fn a_trade(pnl: f64) -> crate::Trade {
    crate::Trade {
        entry_price: 100.0,
        exit_price: 100.0 + pnl,
        size: 1.0,
        pnl,
        fees: 0.1,
        entry_ts: 1_756_000_000_000,
        exit_ts: 1_756_000_060_000,
        symbol: "BTCUSDT".to_string(),
        mae: 0.0,
        mfe: pnl.max(0.0),
        is_long: true,
    }
}

/// The whole point of the stage: what the run computed comes BACK, typed, through the reader
/// that lives beside the writer. A blob would have made the schema version meaningless.
#[test]
fn a_written_series_reads_back_with_every_sample_and_every_counter_intact() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = a_series();
    let extras = RunExtras { series: Some(&written), ..Default::default() };

    write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
    let back = read_series(&run.path).unwrap();

    assert_eq!(back.schema, SERIES_SCHEMA);
    assert_eq!(back.equity, written.equity);
    assert_eq!(back.equity_ts, written.equity_ts);
    assert_eq!(back.per_symbol_equity, written.per_symbol_equity);
    assert_eq!(back.stride, 1);
    assert_eq!(back.source_len, 3);
    assert_eq!(back.diagnostics.warmup, 20);
    assert_eq!(back.diagnostics.stale_deferrals, 2);
    assert_eq!(back.diagnostics.dropped.len(), 1);
    assert_eq!(back.diagnostics.dropped[0].reason, "risk_gate:max_notional");
}

/// The parallel-vector hazard, refused by an invariant a reader can CHECK rather than by a
/// convention it has to trust: `vike_analytics::metrics::returns` SKIPS zero-denominator steps,
/// so a returns vector is not index-alignable with a timestamp vector — which is exactly why
/// this record persists the CURVE and lets a reader derive returns from it.
#[test]
fn a_series_is_aligned_when_its_timestamps_match_its_samples_or_are_absent() {
    assert!(a_series().is_aligned(), "equal lengths are aligned");

    let untimed = RunSeries { equity_ts: Vec::new(), ..a_series() };
    assert!(untimed.is_aligned(), "an untracked-timestamp run is the other valid shape");

    let ragged = RunSeries { equity_ts: vec![1, 2], ..a_series() };
    assert!(!ragged.is_aligned(), "two lengths that are neither equal nor empty is not a document");
}

/// A ledger is a PREFIX when it is bounded, never a sample: `source_len` is what tells a reader
/// it is holding part of a ledger rather than all of a short one.
#[test]
fn a_bounded_trade_ledger_says_how_many_trades_the_run_actually_closed() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    let written = RunTrades {
        schema: TRADES_SCHEMA,
        trades: vec![a_trade(1.0), a_trade(-2.0)],
        source_len: 900,
    };
    let extras = RunExtras { trades: Some(&written), ..Default::default() };

    write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap();
    let back = read_trades(&run.path).unwrap();

    assert_eq!(back.trades.len(), 2);
    assert_eq!(back.source_len, 900, "the ledger is a prefix and must say so");
    assert_eq!(back.trades[1].pnl, -2.0);
}

/// A run that kept no series is the ordinary shape of every run written before this stage, and
/// of any producer with no curve. It must read as MISSING — the same distinct answer the
/// manifest's absence already carries — never as a corrupt document.
#[test]
fn a_run_with_no_series_reads_as_missing_rather_than_broken() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    write_run(&run.path, &a_manifest(&run.run_id), &json!({ "sharpe": 1.0 })).unwrap();

    let err = read_series(&run.path).unwrap_err();

    assert_matches!(err, RunReadError::Missing { .. }, "expected Missing, got {err:?}");
    assert!(err.to_string().contains(SERIES_FILE), "the message must name the file: {err}");
}

/// The ORDERING contract, extended to the new documents and proved rather than asserted in
/// prose: the manifest is the COMPLETION MARKER, so a failure part-way through must leave the
/// documents that DID land and no manifest. A directory where `report.json` belongs is an
/// unwritable path on every platform this ships to, reached without changing permissions.
#[test]
fn a_failure_writing_the_report_leaves_the_extras_and_no_manifest() {
    let root = tempfile::tempdir().unwrap();
    let run = create_run_dir(&root.path().join("runs"), 1_756_000_000, None).unwrap();
    std::fs::create_dir(run.path.join(REPORT_FILE)).unwrap();
    let series = a_series();
    let extras = RunExtras { series: Some(&series), ..Default::default() };

    let err = write_run_with(&run.path, &a_manifest(&run.run_id), &json!({}), &extras).unwrap_err();

    assert_matches!(err, RunPersistError::Write { .. }, "expected Write, got {err:?}");
    assert!(run.path.join(SERIES_FILE).is_file(), "the series landed before the report");
    assert!(
        !run.path.join(MANIFEST_FILE).is_file(),
        "the manifest is the completion marker and must NOT exist after a failed write"
    );
}
