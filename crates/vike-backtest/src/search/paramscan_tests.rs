use super::*;
use std::assert_matches;
// The store-backed tests below build bars into a concrete `HistoryStoreDataFusion` (the `vike-store` DEV dependency).
use vike_marketdata::test_support::flat_bar_zero_volume;

pub(crate) fn base_with_paramscan(sweep: &str) -> ProfileBacktest {
    ProfileBacktest::from_toml_str(&format!(
        r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
from = "0"
to = "100000"
[engine]
cash = 1000.0
[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
{sweep}
"#
    ))
    .unwrap()
    .with_stand_in(vike_strategy_lab::stand_in)
}

#[test]
fn expand_cartesian_product() {
    let base = base_with_paramscan("[paramscan]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
    let pts = expand_paramscan(&base).unwrap();
    assert_eq!(pts.len(), 4, "2 x 2 grid");
    // every point overrides strategy.params.size + .threshold, and its own sweep field is cleared
    for p in &pts {
        assert!(p.profile.paramscan.is_none(), "expanded point is a plain single-run profile");
        assert!(p.profile.strategy.as_ref().unwrap().params.get("size").is_some());
        assert!(p.profile.strategy.as_ref().unwrap().params.get("threshold").is_some());
    }
    // deterministic order + labels present
    assert!(!pts[0].overrides.is_empty());
}

#[test]
fn no_sweep_is_single_point() {
    let base = base_with_paramscan(""); // no [sweep]
    let pts = expand_paramscan(&base).unwrap();
    assert_eq!(pts.len(), 1);
    assert!(pts[0].overrides.is_empty());
}

/// A grid past `PARAMSCAN_POINTS_MAX` points is refused in words before it is allocated: two axes of 5,000
/// values (25 million points), and ten of 100 (a product past `usize`, which a plain `product()` wraps or
/// panics on). A grid of exactly the cap expands.
#[test]
fn a_grid_past_the_points_cap_is_refused_before_it_expands() {
    let axis = |name: &str, n: usize| {
        let values: Vec<String> = (0..n).map(|v| v.to_string()).collect();
        format!("{name} = [{}]\n", values.join(", "))
    };
    let refused = |sweep: String| match expand_paramscan_overrides(&base_with_paramscan(&sweep)) {
        Err(HarnessError::Validation(why)) => why.to_string(),
        other => panic!("refused, not {:?}", other.map(|points| points.len())),
    };
    let why = refused(format!("[paramscan]\n{}{}", axis("fast", 5_000), axis("slow", 5_000)));
    assert!(why.contains("fast 5000 × slow 5000"), "{why}");
    assert!(why.contains(&PARAMSCAN_POINTS_MAX.to_string()), "{why}");
    let wide: String = (0..10).map(|i| axis(&format!("k{i}"), 100)).collect();
    refused(format!("[paramscan]\n{wide}"));
    let at_cap = format!("[paramscan]\n{}", axis("fast", PARAMSCAN_POINTS_MAX));
    let points = expand_paramscan_overrides(&base_with_paramscan(&at_cap)).unwrap();
    assert_eq!(points.len(), PARAMSCAN_POINTS_MAX);
}

#[test]
fn non_array_sweep_value_errors() {
    let base = base_with_paramscan("[paramscan]\nsize = 3.0"); // not an array
    assert_matches!(expand_paramscan(&base), Err(HarnessError::Validation(_)));
}

#[test]
fn expand_is_deterministic() {
    let base = base_with_paramscan("[paramscan]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
    let a = expand_paramscan(&base).unwrap();
    let b = expand_paramscan(&base).unwrap();
    let a_pairs: Vec<Vec<(String, ParamValue)>> = a.iter().map(|p| p.overrides.clone()).collect();
    let b_pairs: Vec<Vec<(String, ParamValue)>> = b.iter().map(|p| p.overrides.clone()).collect();
    assert_eq!(a_pairs, b_pairs);
}

/// ⚠ **THE MEMORY CLAIM, as arithmetic rather than as prose.** This is the measurement that
/// dissolved `--keep-trials series`' refusal, so it is pinned rather than restated: a bucketed
/// column costs `DEFAULT_BUCKETS` f64s where a decimated CURVE costs
/// `vike_runs::MAX_EQUITY_SAMPLES` of them, and both sides are DERIVED from the
/// constants so neither can be typed wrong. Change either constant and this goes RED naming the
/// new numbers, instead of letting a stale "forty-fold" survive in a doc comment.
#[test]
fn bucketed_returns_cost_a_fortieth_of_a_curve() {
    const TRIALS: usize = 500;
    let per_column = std::mem::size_of::<f64>() * OverfitReturnBuckets::DEFAULT_BUCKETS;
    let per_curve = std::mem::size_of::<f64>() * vike_runs::MAX_EQUITY_SAMPLES;
    assert_eq!(per_column, 4_096, "512 x 8 bytes = 4 KB a trial");
    assert_eq!(per_column * TRIALS, 2_048_000, "…so 500 trials is ~2 MB");
    assert_eq!(per_curve * TRIALS, 80_000_000, "…against ~80 MB of decimated curves");
    assert!(
        per_curve / per_column >= 39,
        "the ratio the refusal was rewritten on: {per_curve} / {per_column}"
    );
}

/// The bucketing is BLOCK returns, so a column is the compounded curve rather than a sample of
/// it — and the whole-curve return must survive the round trip. Checked on a curve whose
/// length does not divide the bucket count, which is the ordinary case.
#[test]
fn a_column_compounds_to_the_curve_s_own_return() {
    // 1.00, 1.01, ... 1.00 * 1.01^70 — a curve of 71 points, 70 per-bar returns.
    let curve: Vec<f64> = (0..71).map(|i| libm::pow(1.01, i as f64)).collect();
    let col =
        OverfitReturnBuckets::new(8).capture(&curve).expect("a 70-return curve fills 8 buckets");
    assert_eq!(col.len(), 8, "the requested count, since 8 <= 70");
    let compounded: f64 = col.iter().fold(1.0, |accumulator, r| accumulator * (1.0 + r));
    let whole = curve[curve.len() - 1] / curve[0];
    assert!(
        (compounded - whole).abs() < 1e-9,
        "block returns must compound back to the curve: {compounded} vs {whole}"
    );
}

/// ⚠ **No bucket is ever EMPTY, and a short range therefore yields a SHORTER column rather
/// than one padded with zeros.** A fabricated `0.0` is a real observation to every statistic
/// downstream, so padding would move a 200-bar run's PBO on evidence that does not exist.
#[test]
fn a_short_curve_shortens_the_column_rather_than_padding_it() {
    let curve = vec![100.0, 101.0, 102.0, 103.0, 104.0]; // 4 per-bar returns
    let col = OverfitReturnBuckets::DEFAULT.capture(&curve).expect("4 returns give 4 buckets");
    assert_eq!(col.len(), 4, "min(512, n - 1), never 512 with 508 zeros");
    for r in &col {
        assert!(*r > 0.0, "every bucket spans at least one real bar: {col:?}");
    }
}

/// DISARMED is the default and retains nothing — the property that makes an unarmed search
/// byte-identical to one run before this type existed.
#[test]
fn a_disarmed_capture_retains_nothing() {
    let curve: Vec<f64> = (0..1000).map(|i| 100.0 + i as f64).collect();
    assert_eq!(OverfitReturnBuckets::default(), OverfitReturnBuckets::DISARMED);
    assert!(!OverfitReturnBuckets::DISARMED.is_armed());
    assert!(OverfitReturnBuckets::DISARMED.capture(&curve).is_none());
    assert!(OverfitReturnBuckets::new(0).capture(&curve).is_none(), "0 is DISARMED, one meaning");
}

/// ⚠ A curve that cannot produce a finite column produces NO column. `pbo_cscv` answers `NaN`
/// if any cell anywhere is non-finite, so one degenerate trial would otherwise take the
/// statistic down for every other trial in the search.
#[test]
fn a_curve_that_touches_zero_contributes_no_column_at_all() {
    let wiped = vec![100.0, 50.0, 0.0, 0.0, 0.0];
    assert!(
        OverfitReturnBuckets::new(4).capture(&wiped).is_none(),
        "a zero denominator is refused for the whole trial, not written as a NaN cell"
    );
    // Too short to hold even one bucket.
    assert!(OverfitReturnBuckets::new(4).capture(&[100.0]).is_none());
    assert!(OverfitReturnBuckets::new(4).capture(&[]).is_none());
}

/// A negative equity crossing is NOT refused: the block return is finite and signed, and a
/// leveraged blowup is information a matrix should carry. Stated as a test because the
/// zero-refusal above invites the assumption that any sign change is rejected too.
#[test]
fn a_sign_change_is_a_finite_observation_and_survives() {
    let curve = vec![100.0, 50.0, -10.0, -5.0];
    let col = OverfitReturnBuckets::new(3).capture(&curve).expect("finite throughout");
    assert_eq!(col.len(), 3);
    assert!(col.iter().all(|r| r.is_finite()));
}

#[test]
fn run_sweep_ranks_points() {
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let base = base_with_paramscan("[paramscan]\nsize = [1.0, 2.0]");
    let rep = run_paramscan(&base, store, RankMetric::Sharpe).unwrap();

    assert_eq!(rep.rows.len(), 2, "one row per sweep point");
    assert!(rep.rows.iter().all(|r| r.report.is_some() && r.error.is_none()), "both points ran");
    assert_eq!(rep.rank_by, RankBy::Metric(RankMetric::Sharpe));
    assert!(
        rep.rows.iter().all(|r| r.score.is_none()),
        "the classic run_paramscan never stamps a score"
    );

    // Ranked best-first: non-increasing Sharpe across successful rows (NaN, if any, sorts
    // last per `sort_key`'s `partial_cmp` fallback, so this holds even on a degenerate curve).
    let sharpes: Vec<f64> = rep.rows.iter().map(|r| r.report.as_ref().unwrap().sharpe).collect();
    for w in sharpes.windows(2) {
        if w[0].is_nan() || w[1].is_nan() {
            continue;
        }
        assert!(w[0] >= w[1], "rows must be ranked best-first by sharpe: {sharpes:?}");
    }

    // Display renders a compact ranked table, winner marked first — and no score column
    // (objective path only), keeping the default table unchanged.
    let s = rep.to_string();
    assert!(s.contains("paramscan ranked by sharpe"));
    assert!(s.contains("*#1"));
    assert!(s.contains("size="));
    assert!(!s.contains("score="), "default table must not grow a score column");

    // Default JSON is unchanged too: rank_by is the bare metric string and no `score` key.
    let json = serde_json::to_string(&rep).unwrap();
    assert!(json.contains("\"rank_by\":\"sharpe\""), "rank_by must serialize as before: {json}");
    assert!(!json.contains("\"score\""), "default JSON must not grow a score field: {json}");
}

/// DETERMINISM GATE (the rayon lane): the SAME sweep run in parallel and sequentially must
/// produce byte-identical output — same rows, same rank order, same table, same JSON. Ranking
/// must never depend on which point finished first; `par_iter().map().collect::<Vec<_>>()`
/// reassembles by index (not completion), and the ranking sort is stable over that order.
#[test]
fn parallel_and_sequential_sweeps_are_byte_identical() {
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    // Enough points that the pool really interleaves them.
    let base = base_with_paramscan("[paramscan]\nsize = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]");

    for rank_by in [
        RankMetric::Sharpe,
        RankMetric::TotalReturn,
        RankMetric::MaxDrawdown,
        RankMetric::FinalEquity,
    ] {
        let run = |exec| run_paramscan_execution(&base, store.clone(), rank_by, exec).unwrap();
        let seq = run(ParamscanExecution::Sequential);
        let par = run(ParamscanExecution::Parallel);

        assert_eq!(seq.rows.len(), 8, "{rank_by:?}: one row per grid point");
        assert_eq!(
            seq.to_string(),
            par.to_string(),
            "{rank_by:?}: the ranked table must be byte-identical"
        );
        assert_eq!(
            serde_json::to_string(&seq).unwrap(),
            serde_json::to_string(&par).unwrap(),
            "{rank_by:?}: --json output must be byte-identical"
        );
        // And bit-exact on the floats the table rounds for display.
        for (a, b) in seq.rows.iter().zip(&par.rows) {
            assert_eq!(a.overrides, b.overrides, "{rank_by:?}: same point at the same rank");
            match (&a.report, &b.report) {
                (Some(ra), Some(rb)) => {
                    assert_eq!(ra.final_equity.to_bits(), rb.final_equity.to_bits());
                    assert_eq!(ra.total_return.to_bits(), rb.total_return.to_bits());
                    assert_eq!(ra.max_drawdown.to_bits(), rb.max_drawdown.to_bits());
                }
                (None, None) => {}
                _ => panic!("{rank_by:?}: success/failure disagreed between exec modes"),
            }
        }
    }
}

/// The escape hatch is the EXACT string `"1"`, never a fuzzy truthy parse.
#[test]
fn sweep_execution_defaults_to_parallel() {
    assert_eq!(ParamscanExecution::default(), ParamscanExecution::Parallel);
    assert_eq!(ParamscanExecution::from_pin(Some("1")), ParamscanExecution::Sequential);
    for other in [None, Some(""), Some("0"), Some("true"), Some(" 1")] {
        assert_eq!(ParamscanExecution::from_pin(other), ParamscanExecution::Parallel, "{other:?}");
    }
    // Nothing in this binary installs one, so the plain entry points stay parallel.
    assert_eq!(paramscan_execution(), ParamscanExecution::Parallel);
}

/// The concurrency bound is a CAP, not a suggestion: parallelism multiplies the materialized
/// data slice (module doc), so with no installed cap the pool must be sized
/// `min(DEFAULT_PARAMSCAN_THREADS, cores)` — never one worker per logical core, and never zero
/// (rayon reads `num_threads(0)` as "all cores", the exact shape this cap prevents).
#[test]
fn sweep_threads_defaults_to_a_small_cap() {
    // ⚠ The bound is asserted on `default_sweep_threads`, not on `paramscan_threads`, and that is
    // not a weakening — it is the only order-independent way to assert it. `paramscan_threads`
    // now consults a process-wide `OnceLock` that a sibling test writes, and `cargo test`
    // defines no order between the two, so reading the resolved answer here would pass or
    // fail by scheduling.
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let n = default_sweep_threads();
    assert!(n >= 1, "a zero-sized pool would mean 'all cores' to rayon");
    assert!(n <= DEFAULT_PARAMSCAN_THREADS, "default concurrency must stay capped, got {n}");
    assert!(n <= cores, "never more workers than cores, got {n} on {cores} cores");

    // …and the RESOLVED answer goes through it when no cap is installed.
    if INSTALLED_SWEEP_THREADS.get().is_none() {
        assert_eq!(paramscan_threads(), n, "with no cap installed, the fallback IS the answer");
    }
}

/// **The INSTALLED cap is what the pool reads** — `preferences.paramscan_threads` reaching rayon,
/// asserted at the seam rather than at the composition roots (which cannot be run in a unit
/// test, and which `vike_config::CONSUMPTION`'s row names by file and needle so the gate
/// reddens if either one is deleted).
///
/// ⚠ **This test OWNS the process-wide `OnceLock`, and that is why it is one test rather than
/// three.** `INSTALLED_SWEEP_THREADS` can be written once per PROCESS, so a second test
/// installing a different value would race this one under `cargo test`'s thread pool and the
/// pair would pass or fail by scheduling. Everything the seam promises is therefore asserted
/// here, in order: nothing installed, install, re-install refused.
///
/// ⚠ The value installed is deliberately NOT one [`default_sweep_threads`] could return, so
/// "the pool read the installed cap" cannot be satisfied by the fallback happening to agree.
/// That is also why the sibling test asserts the bound on `default_sweep_threads` rather than
/// on `paramscan_threads`: it is then independent of whether this test ran first.
///
/// ⚠ **Mutation proof, MEASURED (production code):** make [`install_paramscan_threads`]'s
/// `Some(n) if n > 0` arm return `false` without touching the `OnceLock` — the seam doing
/// nothing, which is what the composition roots' calls would amount to — and this reddens on
/// "the first real install must take".
///
/// ⚠ **This used to need a FEATURE to run at all**, worth knowing because a stale assumption
/// here would have hidden a real gap: `harness` was `#[cfg(feature = "hist-replay")]` until the
/// 2026-09-27 feature collapse made it this crate's DEFAULT build, so a plain
/// `cargo test -p vike-backtest` now compiles and runs this file's tests with no feature flag
/// at all — including in the roster's default lane, not only the lane that
/// builds the `backtesthub` bin this key's composition root lives in.
#[test]
fn an_installed_cap_is_what_the_pool_reads_and_a_second_install_is_refused() {
    // `None` installs nothing — a process whose database says nothing keeps the compiled-in
    // fallback.
    assert!(!install_paramscan_threads(None), "`None` must install nothing");
    // …and neither does a zero, which `vike_config::Preferences::apply` refuses long before
    // here (it would build a pool that never runs a point).
    assert!(!install_paramscan_threads(Some(0)), "a zero must install nothing");

    // Above the compiled-in cap, so the fallback can never produce it by accident.
    const INSTALLED: usize = DEFAULT_PARAMSCAN_THREADS + 7;
    assert!(install_paramscan_threads(Some(INSTALLED)), "the first real install must take");
    assert_eq!(
        paramscan_threads(),
        INSTALLED,
        "the pool must read the cap the composition root installed — this is the whole of \
             what makes `preferences.sweep_threads` a setting rather than a declaration"
    );

    // A SECOND install is refused rather than honoured: two roots in one process is a bug, and
    // silently moving the bound under a pool that may already be built is the worse failure.
    assert!(!install_paramscan_threads(Some(INSTALLED + 1)), "a second install must be refused");
    assert_eq!(paramscan_threads(), INSTALLED, "…and must not have changed the answer");
}

#[test]
fn run_sweep_records_failure() {
    // A synthetic failed row (no live backtest error path is easy to trigger here without
    // network/venue setup) exercises the sort-puts-failures-last rule and the Display FAILED
    // branch directly — the real error-capture wiring (`Err(e) => ... error: Some(..)`) is
    // exercised by every OTHER run_paramscan test succeeding without ever populating `error`.
    let ok_report = TradingResults {
        name: None,
        final_equity: 1100.0,
        total_return: 0.1,
        n_trades: 1,
        win_rate: 1.0,
        sharpe: 1.5,
        max_drawdown: 0.02,
        profit_factor: 2.0,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };
    let mut rows = vec![
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(9.0))],
            report: None,
            error: Some("boom".to_string()),
            score: None,
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(1.0))],
            report: Some(ok_report),
            error: None,
            score: None,
        },
    ];
    rows.sort_by(|a, b| match (&a.report, &b.report) {
        (Some(ra), Some(rb)) => RankMetric::Sharpe.cmp_reports(ra, rb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let rep = ParamscanReport {
        rows,
        rank_by: RankBy::Metric(RankMetric::Sharpe),
        summary: None,
        account_currency: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
    };

    // ⚠ AN ABSENT SUMMARY ADDS NO KEY, which is what keeps a GRID document byte-identical to
    // the one this crate emitted before the field existed — the property
    // `crates/vike-backtesthub/tests/backtesthub_profile_roundtrip.rs`'s
    // `profile_sweep_is_byte_identical_local_and_remote` compares bytes for. A present one is
    // carried, so a remote euler or tpe run can report the budget the engine prints to a stderr
    // no socket carries.
    let json = serde_json::to_string(&rep).expect("serializes");
    assert!(!json.contains("summary"), "an absent summary adds NO key: {json}");
    let searched = ParamscanReport {
        rows: Vec::new(),
        rank_by: RankBy::Objective("multi".to_string()),
        summary: Some("tpe: 128 trials".to_string()),
        account_currency: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
    };
    let json = serde_json::to_string(&searched).expect("serializes");
    assert!(json.contains("tpe: 128 trials"), "a present summary is carried: {json}");

    assert!(rep.rows[0].report.is_some(), "successful row ranks first");
    assert!(rep.rows[1].error.is_some(), "failed row ranks last");

    let s = rep.to_string();
    assert!(s.contains("FAILED: boom"));
}

#[test]
fn nan_sharpe_ranks_last_never_first() {
    // Regression: a NaN Sharpe (e.g. a zero-variance equity curve) must sort LAST among
    // successes — with the old `partial_cmp(...).unwrap_or(Equal)` it compared Equal to every
    // finite point and, being placed first here, stayed ranked #1.
    let good = TradingResults {
        name: None,
        final_equity: 1200.0,
        total_return: 0.2,
        n_trades: 3,
        win_rate: 1.0,
        sharpe: 2.0,
        max_drawdown: 0.01,
        profit_factor: 3.0,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };
    let degenerate = TradingResults {
        name: None,
        final_equity: 1000.0,
        total_return: 0.0,
        n_trades: 0,
        win_rate: 0.0,
        sharpe: f64::NAN,
        max_drawdown: 0.0,
        profit_factor: 0.0,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };
    // NaN row deliberately placed first, so a broken comparator would leave it ranked #1.
    let mut rows = [
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(1.0))],
            report: Some(degenerate),
            error: None,
            score: None,
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(2.0))],
            report: Some(good),
            error: None,
            score: None,
        },
    ];
    rows.sort_by(|a, b| match (&a.report, &b.report) {
        (Some(ra), Some(rb)) => RankMetric::Sharpe.cmp_reports(ra, rb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    assert_eq!(rows[0].report.as_ref().unwrap().sharpe, 2.0, "finite-Sharpe point wins");
    assert!(
        rows[1].report.as_ref().unwrap().sharpe.is_nan(),
        "NaN-Sharpe point ranks last, not first"
    );
}

#[test]
fn rank_metric_from_str() {
    assert_eq!(RankMetric::from_str_ci("SHARPE"), Some(RankMetric::Sharpe));
    assert_eq!(RankMetric::from_str_ci("return"), Some(RankMetric::TotalReturn));
    assert_eq!(RankMetric::from_str_ci("max_dd"), Some(RankMetric::MaxDrawdown));
    assert_eq!(RankMetric::from_str_ci("Equity"), Some(RankMetric::FinalEquity));
    assert_eq!(RankMetric::from_str_ci("nope"), None);
}

/// A varied report for the ranking-equivalence regression below.
fn rep(total_return: f64, sharpe: f64, max_dd: f64, final_eq: f64) -> TradingResults {
    TradingResults {
        name: None,
        final_equity: final_eq,
        total_return,
        n_trades: 5,
        win_rate: 0.5,
        sharpe,
        max_drawdown: max_dd,
        profit_factor: 1.5,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    }
}

/// REGRESSION (default ranking unchanged): for EVERY `RankMetric`, ranking through its
/// objective constructor (`RankMetric::objective` + the objective comparator) orders a varied
/// report set — ties, negatives, and a NaN — EXACTLY like the classic `cmp_reports`
/// comparator `run_paramscan` still uses. So the two paths cannot drift apart.
///
/// NB the equivalence covers TIED rows (rows 0 and 3 tie on `max_dd`) only because BOTH
/// paths sort stably (`slice::sort_by`) and neither comparator has a secondary tie-break key,
/// so ties keep input order on both sides. If `cmp_reports` ever grows a tie-break, the
/// objective comparator must grow the same one — this test would catch it.
#[test]
fn metric_objectives_rank_identically_to_cmp_reports() {
    let reports = [
        rep(0.2, 1.5, 0.05, 1200.0),
        rep(-0.1, -0.3, 0.30, 900.0),
        rep(0.2, f64::NAN, 0.00, 1000.0), // NaN sharpe, zero drawdown
        rep(0.05, 0.9, 0.05, 1050.0),     // max_dd tie with row 0
        rep(0.4, 2.5, 0.10, 1400.0),
    ];
    for metric in [
        RankMetric::Sharpe,
        RankMetric::TotalReturn,
        RankMetric::MaxDrawdown,
        RankMetric::FinalEquity,
    ] {
        let objective = metric.objective();

        let mut classic: Vec<usize> = (0..reports.len()).collect();
        classic.sort_by(|&a, &b| metric.cmp_reports(&reports[a], &reports[b]));

        let scores: Vec<f64> = reports.iter().map(&objective).collect();
        let mut via_objective: Vec<usize> = (0..reports.len()).collect();
        via_objective.sort_by(|&a, &b| cmp_scores_desc(scores[a], scores[b]));

        assert_eq!(classic, via_objective, "{metric:?}: objective ranking must match cmp_reports");
    }
}

/// A report with every field the composite objective reads, for the losing-grid regression.
fn scored_rep(total_return: f64, max_dd: f64, pf: f64, win_rate: f64, n: usize) -> TradingResults {
    TradingResults {
        name: None,
        final_equity: 1000.0 * (1.0 + total_return),
        total_return,
        n_trades: n,
        win_rate,
        sharpe: -1.0,
        max_drawdown: max_dd,
        profit_factor: pf,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    }
}

/// REGRESSION (the sign law, through the RANKING comparator): on an ALL-LOSING grid — the
/// common case when tuning a bad strategy — `--rank-by multi` must crown the LEAST-bad point.
/// The pre-fix score multiplied a negative base by sub-1 shaping factors, so the catastrophic
/// point (win_rate 0 -> coeff 0, pf 0 -> ln(1) = 0) collapsed to `-0.0` and ranked #1, and the
/// thin 1-trade sample outranked the 100-trade one for the same reason.
#[test]
fn all_losing_grid_never_crowns_the_worst_point() {
    use crate::search::objective::{SearchObjectiveMultiMetricConfig, multi_metric_score};

    let p = SearchObjectiveMultiMetricConfig::default();
    let points = [
        ("catastrophic", scored_rep(-0.95, 0.95, 0.0, 0.0, 200)),
        ("bad", scored_rep(-0.40, 0.45, 0.4, 0.2, 120)),
        ("mild", scored_rep(-0.01, 0.02, 0.9, 0.45, 80)),
        ("mild-but-thin", scored_rep(-0.01, 0.02, 0.9, 0.45, 1)),
    ];
    let scores: Vec<f64> = points.iter().map(|(_, r)| multi_metric_score(r, &p)).collect();
    assert!(scores.iter().all(|s| *s < 0.0), "every point loses money: {scores:?}");

    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by(|&a, &b| cmp_scores_desc(scores[a], scores[b]));
    let ranked: Vec<&str> = order.iter().map(|&i| points[i].0).collect();
    assert_eq!(
        ranked,
        ["mild", "mild-but-thin", "bad", "catastrophic"],
        "losers must rank by (quality-amplified) loss size: {scores:?}"
    );
}

/// The objective path's JSON shape: a stamped finite score serializes as a NUMBER, and an
/// "unrankable" NaN score goes through `serialize_opt_score` to `null` — never a bare `NaN` token
/// (invalid JSON) and never a serialization error. `None` staying skipped is pinned by
/// `run_sweep_ranks_points`.
#[test]
fn objective_json_stamps_scores_and_nulls_unrankable() {
    let rows = vec![
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(1.0))],
            report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
            error: None,
            score: Some(1.25),
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(2.0))],
            report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
            error: None,
            score: Some(f64::NAN),
        },
    ];
    let report = ParamscanReport {
        rows,
        rank_by: RankBy::Objective("multi".to_string()),
        summary: None,
        account_currency: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
    };

    let json = serde_json::to_string(&report).expect("an unrankable row must not fail --json");
    assert!(!json.contains("NaN"), "a raw NaN token would be invalid JSON: {json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rank_by"], "multi", "an objective serializes as its label");
    assert_eq!(v["rows"][0]["score"], 1.25);
    assert!(v["rows"][1]["score"].is_null(), "NaN score must serialize as null: {json}");
}

/// **An INVALID run (its equity reached zero) scores NaN whatever the objective** (owner 2026-11-16), so the one
/// ordering rule ranks it after every valid point though its Sharpe is the higher, and it can never steer a search.
#[test]
fn an_invalid_run_scores_nan_and_ranks_after_every_valid_point() {
    let sharpe: crate::search::objective::SearchObjective = Box::new(|r: &TradingResults| r.sharpe);
    let row = |size: f64, report: TradingResults| ParamscanRow {
        overrides: vec![("size".to_string(), ParamValue::Float(size))],
        report: Some(report),
        error: None,
        score: None,
    };
    let dead = TradingResults {
        invalid: Some("equity reached 0 on 2018-02-05: metrics are not meaningful".to_string()),
        ..rep(-40.0, 3.0, 34.0, -392_646.0)
    };
    let (dead, dead_score) = grid::score_row(row(1.0, dead), &sharpe);
    let (alive, alive_score) = grid::score_row(row(0.1, rep(0.2, 0.5, 0.1, 12_000.0)), &sharpe);
    assert!(dead_score.is_nan() && dead.score.is_some_and(f64::is_nan), "{:?}", dead.score);
    assert_eq!((alive_score, alive.score), (0.5, Some(0.5)));
    let mut rows = vec![dead, alive];
    sort_scored_rows(&mut rows);
    let sizes: Vec<&ParamValue> = rows.iter().map(|r| &r.overrides[0].1).collect();
    assert_eq!(sizes, [&ParamValue::Float(0.1), &ParamValue::Float(1.0)], "the valid point first");
}

/// The objective path end-to-end over a real store: scores are stamped, rows sort best-first
/// by score, the header names the objective, and the Sharpe objective reproduces the classic
/// `run_paramscan` order on the same data.
#[test]
fn run_sweep_with_ranks_by_objective() {
    use crate::search::objective::{SearchObjectiveMultiMetricConfig, multi_metric};
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let base = base_with_paramscan("[paramscan]\nsize = [1.0, 2.0]");

    // (a) multi objective: every successful row gets a score, sorted best-first (NaN-aware).
    let obj = multi_metric(SearchObjectiveMultiMetricConfig::default());
    let rep = run_paramscan_with(&base, store.clone(), &obj, "multi").unwrap();
    assert_eq!(rep.rank_by, RankBy::Objective("multi".to_string()));
    assert_eq!(rep.rows.len(), 2);
    assert!(rep.rows.iter().all(|r| r.report.is_some() && r.score.is_some()));
    let scores: Vec<f64> = rep.rows.iter().map(|r| r.score.unwrap()).collect();
    for w in scores.windows(2) {
        if w[0].is_nan() || w[1].is_nan() {
            assert!(!w[0].is_nan(), "a NaN score must not rank above a finite one: {scores:?}");
            continue;
        }
        assert!(w[0] >= w[1], "rows must be ranked best-first by score: {scores:?}");
    }
    let s = rep.to_string();
    assert!(s.contains("paramscan ranked by multi"));
    assert!(s.contains("score="), "objective table shows the score column");

    // (b) the Sharpe built-in objective orders the rows exactly like the classic path.
    let classic = run_paramscan(&base, store.clone(), RankMetric::Sharpe).unwrap();
    let sharpe_obj = RankMetric::Sharpe.objective();
    let via_obj = run_paramscan_with(&base, store, &sharpe_obj, "sharpe").unwrap();
    let order = |r: &ParamscanReport| -> Vec<Vec<(String, ParamValue)>> {
        r.rows.iter().map(|row| row.overrides.clone()).collect()
    };
    assert_eq!(order(&classic), order(&via_obj), "built-in objective must match run_paramscan");
}

/// ParamscanReport wiring: a zero-trade / flat row surfaces its top probable cause inline in the
/// ranked table, while a row that traded is untouched (byte-identical). Built from literal rows
/// so it does not depend on a strategy that happens to produce a flat run.
#[test]
fn sweep_table_annotates_zero_trade_rows_only() {
    let traded = TradingResults {
        name: None,
        final_equity: 1200.0,
        total_return: 0.2,
        n_trades: 4,
        win_rate: 0.75,
        sharpe: 1.8,
        max_drawdown: 0.03,
        profit_factor: 2.5,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };
    let flat = TradingResults {
        name: None,
        final_equity: 1000.0,
        total_return: 0.0,
        n_trades: 0,
        win_rate: 0.0,
        sharpe: f64::NAN,
        max_drawdown: 0.0,
        profit_factor: 0.0,
        per_symbol_pnl: Vec::new(),
        funding_paid: 0.0,
        invalid: None,
        zero_trade: Some(vike_analytics::zero_trade::ZeroTradeReport {
            causes: vec![vike_analytics::zero_trade::ZeroTradeCause {
                code: "orders-denied".to_string(),
                headline: "All 5 submitted order(s) were rejected before filling.".to_string(),
                detail: "d".to_string(),
            }],
        }),
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    };
    let report = ParamscanReport {
        rows: vec![
            ParamscanRow {
                overrides: vec![("size".to_string(), ParamValue::Float(1.0))],
                report: Some(traded),
                error: None,
                score: None,
            },
            ParamscanRow {
                overrides: vec![("size".to_string(), ParamValue::Float(2.0))],
                report: Some(flat),
                error: None,
                score: None,
            },
        ],
        rank_by: RankBy::Metric(RankMetric::Sharpe),
        summary: None,
        account_currency: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
    };
    let s = report.to_string();
    assert!(
        s.contains("-- All 5 submitted order(s) were rejected before filling."),
        "the zero-trade row must show its top cause: {s}"
    );
    // The row that traded carries no `--` cause hint.
    let traded_line = s.lines().find(|l| l.contains("trades=4")).unwrap();
    assert!(!traded_line.contains("--"), "a row that traded is untouched: {traded_line}");
}

/// The table has no column for what a search found in its data, and a local text search has no other
/// place to read it. Every point reads one slice, so the first row that carries any speaks for all; a
/// failed row before it carries none. A clean table is what it always was.
#[test]
fn a_search_table_prints_what_its_data_lacked_once() {
    let table = |data_warnings: Vec<String>| {
        let ok = TradingResults {
            name: None,
            final_equity: 1100.0,
            total_return: 0.1,
            n_trades: 1,
            win_rate: 1.0,
            sharpe: 1.5,
            max_drawdown: 0.02,
            profit_factor: 2.0,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
            invalid: None,
            extended: None,
            honesty: None,
            realism: None,
            data_warnings,
            in_dollars: None,
            closed_trades: None,
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
            returns_interval_ms: None,
            mounts: None,
            strategy_failures: Default::default(),
            source: None,
            period: None,
        };
        let row = |report, error: Option<&str>| ParamscanRow {
            overrides: vec![("size".to_string(), ParamValue::Float(1.0))],
            report,
            error: error.map(str::to_string),
            score: None,
        };
        ParamscanReport {
            rows: vec![row(None, Some("boom")), row(Some(ok.clone()), None), row(Some(ok), None)],
            rank_by: RankBy::Metric(RankMetric::Sharpe),
            summary: None,
            account_currency: None,
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
        }
        .to_string()
    };

    let warned = table(vec!["ETHUSDT: did not run".to_string()]);
    assert!(warned.contains("data_warnings:\n  ETHUSDT: did not run\n"), "{warned}");
    assert_eq!(warned.matches("ETHUSDT").count(), 1, "said once, not per row: {warned}");

    let clean = table(Vec::new());
    assert!(!clean.contains("data_warnings"), "{clean}");
    assert_eq!(warned.replace("data_warnings:\n  ETHUSDT: did not run\n", ""), clean);
}

/// **A sweep may vary the run's slippage**, a strategy param like any other (the owner's ruling of
/// 2026-11-18): `[paramscan] slippage = [0, 1, 2, 4]` beside `slippage_unit = "ticks"` runs each level,
/// and the report's table shows each. On flat bars `buy_hold`'s one market entry pays `ticks * tick`,
/// so each level's final equity is the zero level's less exactly that.
#[test]
fn a_sweep_over_slippage_runs_each_level() {
    use vike_store::HistoryStoreDataFusion;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    let bars: Vec<_> = (0..4).map(|i| flat_bar_zero_volume(i * 1000, 100.0)).collect();
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let base = base_with_paramscan(
        "slippage_unit = \"ticks\"\nslippage_tick = 0.5\n[paramscan]\nslippage = [0, 1, 2, 4]",
    );
    let points = expand_paramscan(&base).unwrap();
    assert_eq!(points.len(), 4);
    for p in &points {
        let ticks = p
            .profile
            .strategy
            .as_ref()
            .unwrap()
            .params
            .get("slippage")
            .and_then(|v| v.as_integer())
            .unwrap();
        assert_eq!(p.overrides, vec![("slippage".to_string(), ParamValue::Int(ticks))]);
    }

    let rep =
        run_paramscan_execution(&base, store, RankMetric::Sharpe, ParamscanExecution::Sequential)
            .unwrap();
    let equity_at = |ticks: i64| {
        let row = rep
            .rows
            .iter()
            .find(|r| r.overrides[0].1 == ParamValue::Int(ticks))
            .unwrap_or_else(|| panic!("no row for {ticks} ticks"));
        row.report.as_ref().unwrap_or_else(|| panic!("{:?}", row.error)).final_equity
    };
    let flat = equity_at(0);
    for ticks in [1, 2, 4] {
        let paid = flat - equity_at(ticks);
        assert!((paid - ticks as f64 * 0.5).abs() < 1e-9, "{ticks} ticks paid {paid}");
    }
    assert!(rep.to_string().contains("slippage="), "the table shows each level");
}

/// The grid varies strategy params alone: an `engine.` key is refused by name, pointing at the cost
/// params that replaced the run's own cost keys.
#[test]
fn an_engine_sweep_key_is_refused_naming_the_cost_params() {
    for extra in [
        "[paramscan]\n\"engine.fee_rate\" = [0.001, 0.002]",
        "[paramscan]\n\"engine.slippage.ticks\" = [1, 2]",
        // the run's mount leverage cap is a risk figure, never a searched axis
        "[paramscan]\n\"engine.max_mount_leverage\" = [2.0, 3.0]",
    ] {
        let base = base_with_paramscan(extra);
        let Err(HarnessError::Validation(msg)) = expand_paramscan(&base) else {
            panic!("{extra} must be refused");
        };
        let msg = msg.to_string();
        assert!(msg.contains("a grid varies strategy params only"), "{msg}");
        assert!(msg.contains("scan commission or slippage"), "{msg}");
        // The search lanes read the same axes and refuse the same way.
        let Err(HarnessError::Validation(msg)) = numeric_sweep_axes(&base, "tpe") else {
            panic!("{extra} must be refused by the search reader too");
        };
        let msg = msg.to_string();
        assert!(msg.contains("a grid varies strategy params only"), "{msg}");
    }
}
