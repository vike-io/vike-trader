//! The rules [`vike_user_research::StudyOutcome`] enforces, tested apart from the pipeline because
//! they are properties of the CONTRACT rather than of the scan.
//!
//! The one that matters most is the artifact name: it becomes a FILE NAME in a run directory the
//! caller creates, so a study must not be able to name a path the writer then has to defend
//! against. Refusing at the point of RECORDING keeps that check in one place instead of in every
//! consumer that ever writes a run out.

use std::sync::Arc;

use vike_data::test_support::MemHistStore;
use vike_data::{CohortRow, HistStore, PerpMetricRow, TsRange};
use vike_user_research::{
    MAX_ARTIFACT_NAME, StudyContext, StudyError, StudyOutcome, valid_artifact_name,
};

#[test]
fn metrics_keep_emission_order_and_refuse_a_duplicate() {
    let mut out = StudyOutcome::new();
    out.metric("sharpe", 5.454).unwrap();
    out.metric("n_trades", 812.0).unwrap();
    let names: Vec<&str> = out.metrics().iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["sharpe", "n_trades"], "emission order, not sorted");
    assert_eq!(out.metric_value("sharpe"), Some(5.454));
    assert!(out.metric_value("nope").is_none());

    let err = out.metric("sharpe", 1.0).expect_err("two rows called sharpe are ambiguous");
    assert!(matches!(err, StudyError::Study(ref m) if m.contains("sharpe")), "{err}");
}

/// A non-finite metric is ACCEPTED. A Sharpe over a fold that never traded is `NaN`, and
/// `vike_analytics::signal_backtest::neg_sharpe_mini_backtest` deliberately scores a degenerate
/// slice `INFINITY`; rounding either to zero here would be the `unwrap_or(0.0)` that turns a gap
/// into an observation.
#[test]
fn a_non_finite_metric_is_recorded_rather_than_rounded() {
    let mut out = StudyOutcome::new();
    out.metric("sharpe", f64::NAN).unwrap();
    out.metric("objective", f64::INFINITY).unwrap();
    assert!(out.metric_value("sharpe").unwrap().is_nan());
    assert_eq!(out.metric_value("objective"), Some(f64::INFINITY));
}

#[test]
fn an_artifact_name_that_is_not_a_safe_file_name_is_refused() {
    assert!(valid_artifact_name("per_rule.tsv").is_ok());
    assert!(valid_artifact_name("model.txt").is_ok());
    assert!(valid_artifact_name("report-2.html").is_ok());

    for bad in [
        "",
        "..",
        ".hidden",
        "../../secrets.env",
        "sub/dir.tsv",
        r"sub\dir.tsv",
        "with space.tsv",
        "naïve.tsv",
    ] {
        assert!(valid_artifact_name(bad).is_err(), "{bad:?} must be refused");
    }
    let too_long = "a".repeat(MAX_ARTIFACT_NAME + 1);
    assert!(valid_artifact_name(&too_long).is_err());
    assert!(valid_artifact_name(&"a".repeat(MAX_ARTIFACT_NAME)).is_ok());
}

#[test]
fn recording_a_bad_or_duplicate_artifact_is_an_error_naming_it() {
    let mut out = StudyOutcome::new();
    out.artifact("per_rule.tsv", "a\tb\n").unwrap();
    assert_eq!(out.artifacts().len(), 1);

    let dup = out.artifact("per_rule.tsv", "x").expect_err("two files cannot share a name");
    assert!(matches!(dup, StudyError::Study(ref m) if m.contains("per_rule.tsv")), "{dup}");

    let bad = out.artifact("../escape.tsv", "x").expect_err("a traversal must not be recordable");
    assert!(matches!(bad, StudyError::Study(ref m) if m.contains("escape")), "{bad}");
    assert_eq!(out.artifacts().len(), 1, "a refused artifact is not half-recorded");
}

// ---------------------------------------------------------------------------------------------
// the cohort read verb
// ---------------------------------------------------------------------------------------------

const VENUE: &str = "hyperliquid";

fn cohort_row(
    ts_ms: i64,
    axis: &str,
    cohort: &str,
    grading: &str,
    long: f64,
    total: f64,
) -> CohortRow {
    CohortRow {
        ts: ts_ms,
        asset: "BTC".into(),
        axis: axis.into(),
        cohort: cohort.into(),
        grading: grading.into(),
        label_basis: "point_in_time".into(),
        long_usd: long,
        total_usd: total,
    }
}

/// A context over a store seeded with `rows` for `(hyperliquid, BTC)`.
fn ctx_over(rows: &[CohortRow]) -> StudyContext {
    let store = MemHistStore::new();
    store.append_cohort(VENUE, "BTC", rows, Some("seed")).expect("MemHistStore accepts the batch");
    StudyContext::new(Arc::new(store), TsRange::all(), std::env::temp_dir())
}

/// The verb forwards `scan_cohort`: what the store holds comes back, and the RANGE is the store's
/// own filter rather than something this crate re-implements.
#[test]
fn the_cohort_verb_forwards_the_store_read_and_its_range() {
    let rows = vec![
        cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0),
        cohort_row(2_000, "size", "Whale", "realized", 70.0, 100.0),
        cohort_row(3_000, "size", "Whale", "realized", 80.0, 100.0),
    ];
    let ctx = ctx_over(&rows);

    assert_eq!(ctx.cohort(VENUE, "BTC", TsRange::all()).unwrap(), rows);
    let mid = ctx.cohort(VENUE, "BTC", TsRange::of(2_000, 2_000)).unwrap();
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].ts, 2_000);

    // A series the store does not hold is EMPTY, not an error — the ordinary uncollected state.
    assert!(ctx.cohort(VENUE, "ETH", TsRange::all()).unwrap().is_empty());
    assert!(ctx.cohort("binance", "BTC", TsRange::all()).unwrap().is_empty());
}

/// ⚠ THE PROPERTY A STUDY MUST HANDLE, gated rather than only documented: `(venue, asset)` does
/// not identify a series here. One scan returns every axis, label, grading and label basis, and
/// the three gradings produce SHAPE-IDENTICAL rows over the same hour for the same asset — so a
/// study that sums whatever comes back reports one number over two tapes.
///
/// This test is the reason the verb passes rows through undiminished instead of filtering: the
/// four discriminating dimensions are all present on the returned rows, which is what makes the
/// study's own filter writable. A verb that quietly picked one grading would make the wrong answer
/// unobservable.
#[test]
fn one_scan_returns_every_axis_and_grading_so_the_study_must_filter() {
    let rows = vec![
        cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0),
        cohort_row(1_000, "pnl", "Smart", "realized", 10.0, 40.0),
        cohort_row(1_000, "pnl", "Smart", "unrealized", 30.0, 40.0),
    ];
    let ctx = ctx_over(&rows);
    let got = ctx.cohort(VENUE, "BTC", TsRange::of(1_000, 1_000)).unwrap();
    assert_eq!(got.len(), 3, "one hour, one asset, three rows — the panel is LONG");

    // Every dimension the path cannot carry is on the row, so a study can select on it.
    let pnl_realized: Vec<&CohortRow> =
        got.iter().filter(|r| r.axis == "pnl" && r.grading == "realized").collect();
    assert_eq!(pnl_realized.len(), 1);
    assert_eq!(pnl_realized[0].long_usd, 10.0);

    // ...and the aliasing hazard is real on this exact data: summing the pnl axis without the
    // grading filter double-counts one hour's cohort.
    let unfiltered: f64 = got.iter().filter(|r| r.axis == "pnl").map(|r| r.total_usd).sum();
    assert_eq!(unfiltered, 80.0, "40 of open interest reported as 80 — this is the wrong answer");
    assert_eq!(pnl_realized.iter().map(|r| r.total_usd).sum::<f64>(), 40.0);
}

/// The short side stays DERIVED across the crossing: the store carries `total`, and
/// `CohortRow::short_usd` is the one subtraction — never a stored third number that could disagree.
#[test]
fn the_short_side_is_derived_from_the_row_the_verb_returned() {
    let ctx = ctx_over(&[cohort_row(1_000, "size", "Whale", "realized", 60.0, 100.0)]);
    let got = ctx.cohort(VENUE, "BTC", TsRange::all()).unwrap();
    assert_eq!(got[0].short_usd(), 40.0);
}

// ---------------------------------------------------------------------------------------------
// the perp-metrics read verb
// ---------------------------------------------------------------------------------------------

/// A context over a store seeded with perp market-context rows for `(hyperliquid, BTC)`.
fn perp_ctx_over(rows: &[PerpMetricRow]) -> StudyContext {
    let store = MemHistStore::new();
    store
        .append_perp_metrics(VENUE, "BTC", rows, Some("seed-perp"))
        .expect("MemHistStore accepts the batch");
    StudyContext::new(Arc::new(store), TsRange::all(), std::env::temp_dir())
}

/// The verb forwards `scan_perp_metrics`: what the store holds comes back, and the RANGE is the
/// store's own filter rather than something this crate re-implements.
#[test]
fn the_perp_metrics_verb_forwards_the_store_read_and_its_range() {
    let rows = vec![
        PerpMetricRow { ts: 1_000, premium: 0.000_335, open_interest: None },
        PerpMetricRow { ts: 2_000, premium: -0.000_623, open_interest: None },
        PerpMetricRow { ts: 3_000, premium: 0.0, open_interest: None },
    ];
    let ctx = perp_ctx_over(&rows);

    assert_eq!(ctx.perp_metrics(VENUE, "BTC", TsRange::all()).unwrap(), rows);
    let mid = ctx.perp_metrics(VENUE, "BTC", TsRange::of(2_000, 2_000)).unwrap();
    assert_eq!(mid.len(), 1);
    assert_eq!(mid[0].ts, 2_000);
    assert!(mid[0].premium < 0.0, "a NEGATIVE premium survives the round trip");

    // A series the store does not hold is EMPTY, not an error — the ordinary uncollected state.
    assert!(ctx.perp_metrics(VENUE, "ETH", TsRange::all()).unwrap().is_empty());
    assert!(ctx.perp_metrics("binance", "BTC", TsRange::all()).unwrap().is_empty());
}

/// ⚠ The premium is its OWN series, NOT a column on the funding bar — so a store holding perp
/// metrics for a symbol says nothing about that symbol's `interval="funding"` bars, and a study
/// that wants both reads both. This pins the split rather than only documenting it.
#[test]
fn perp_metrics_and_the_funding_bars_are_separate_series() {
    let ctx =
        perp_ctx_over(&[PerpMetricRow { ts: 1_000, premium: 0.000_335, open_interest: None }]);
    assert_eq!(ctx.perp_metrics(VENUE, "BTC", TsRange::all()).unwrap().len(), 1);
    assert!(
        ctx.bars(VENUE, "BTC", "funding", TsRange::all()).unwrap().is_empty(),
        "the premium series carries no funding rate — they are filled by two appends"
    );
}

/// A store read failure travels as itself, so a caller can tell a query fault from an I/O one
/// without parsing a sentence — and `?` works inside a study body because of the `From` impl.
#[test]
fn a_store_error_converts_and_keeps_its_source() {
    let e: StudyError = vike_data::DataError::Io("disk went away".into()).into();
    assert!(matches!(e, StudyError::Data(_)));
    assert!(e.to_string().contains("disk went away"), "{e}");
    assert!(std::error::Error::source(&e).is_some(), "the DataError stays reachable");
    assert!(std::error::Error::source(&StudyError::Study("x".into())).is_none());
}
