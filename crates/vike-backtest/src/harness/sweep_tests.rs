use super::*;
// Only the store-backed tests below build bars into a concrete `DataFusionHist`; gated with them.
#[cfg(feature = "datafusion-store")]
use vike_marketdata::test_support::flat_bar_zero_volume;

pub(crate) fn base_with_paramscan(sweep: &str) -> BacktestProfile {
    BacktestProfile::from_toml_str(&format!(
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
name = "buy_hold"
[strategy.params]
size = 1.0
{sweep}
"#
    ))
    .unwrap()
}

#[test]
fn expand_cartesian_product() {
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
    let pts = expand_paramscan(&base).unwrap();
    assert_eq!(pts.len(), 4, "2 x 2 grid");
    // every point overrides strategy.params.size + .threshold, and its own sweep field is cleared
    for p in &pts {
        assert!(p.profile.paramscan.is_none(), "expanded point is a plain single-run profile");
        assert!(p.profile.strategy.params.get("size").is_some());
        assert!(p.profile.strategy.params.get("threshold").is_some());
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

#[test]
fn non_array_sweep_value_errors() {
    let base = base_with_paramscan("[sweep]\nsize = 3.0"); // not an array
    assert!(matches!(expand_paramscan(&base), Err(HarnessError::Validation(_))));
}

#[test]
fn expand_is_deterministic() {
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
    let a = expand_paramscan(&base).unwrap();
    let b = expand_paramscan(&base).unwrap();
    let a_pairs: Vec<Vec<(String, toml::Value)>> = a.iter().map(|p| p.overrides.clone()).collect();
    let b_pairs: Vec<Vec<(String, toml::Value)>> = b.iter().map(|p| p.overrides.clone()).collect();
    assert_eq!(a_pairs, b_pairs);
}

/// ⚠ **THE MEMORY CLAIM, as arithmetic rather than as prose.** This is the measurement that
/// dissolved `--keep-trials series`' refusal, so it is pinned rather than restated: a bucketed
/// column costs `DEFAULT_BUCKETS` f64s where a decimated CURVE costs
/// `vike_model::runs::MAX_EQUITY_SAMPLES` of them, and both sides are DERIVED from the
/// constants so neither can be typed wrong. Change either constant and this goes RED naming the
/// new numbers, instead of letting a stale "forty-fold" survive in a doc comment.
#[test]
fn bucketed_returns_cost_a_fortieth_of_a_curve() {
    const TRIALS: usize = 500;
    let per_column = std::mem::size_of::<f64>() * ReturnBuckets::DEFAULT_BUCKETS;
    let per_curve = std::mem::size_of::<f64>() * vike_model::runs::MAX_EQUITY_SAMPLES;
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
    let col = ReturnBuckets::new(8).capture(&curve).expect("a 70-return curve fills 8 buckets");
    assert_eq!(col.len(), 8, "the requested count, since 8 <= 70");
    let compounded: f64 = col.iter().fold(1.0, |acc, r| acc * (1.0 + r));
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
    let col = ReturnBuckets::DEFAULT.capture(&curve).expect("4 returns give 4 buckets");
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
    assert_eq!(ReturnBuckets::default(), ReturnBuckets::DISARMED);
    assert!(!ReturnBuckets::DISARMED.is_armed());
    assert!(ReturnBuckets::DISARMED.capture(&curve).is_none());
    assert!(ReturnBuckets::new(0).capture(&curve).is_none(), "0 is DISARMED, one meaning");
}

/// ⚠ A curve that cannot produce a finite column produces NO column. `pbo_cscv` answers `NaN`
/// if any cell anywhere is non-finite, so one degenerate trial would otherwise take the
/// statistic down for every other trial in the search.
#[test]
fn a_curve_that_touches_zero_contributes_no_column_at_all() {
    let wiped = vec![100.0, 50.0, 0.0, 0.0, 0.0];
    assert!(
        ReturnBuckets::new(4).capture(&wiped).is_none(),
        "a zero denominator is refused for the whole trial, not written as a NaN cell"
    );
    // Too short to hold even one bucket.
    assert!(ReturnBuckets::new(4).capture(&[100.0]).is_none());
    assert!(ReturnBuckets::new(4).capture(&[]).is_none());
}

/// A negative equity crossing is NOT refused: the block return is finite and signed, and a
/// leveraged blowup is information a matrix should carry. Stated as a test because the
/// zero-refusal above invites the assumption that any sign change is rejected too.
#[test]
fn a_sign_change_is_a_finite_observation_and_survives() {
    let curve = vec![100.0, 50.0, -10.0, -5.0];
    let col = ReturnBuckets::new(3).capture(&curve).expect("finite throughout");
    assert_eq!(col.len(), 3);
    assert!(col.iter().all(|r| r.is_finite()));
}

#[cfg(feature = "datafusion-store")]
#[test]
fn run_sweep_ranks_points() {
    use vike_data::DataFusionHist;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0]");
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
    assert!(s.contains("sweep ranked by sharpe"));
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
#[cfg(feature = "datafusion-store")]
#[test]
fn parallel_and_sequential_sweeps_are_byte_identical() {
    use vike_data::DataFusionHist;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    // Enough points that the pool really interleaves them.
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]");

    for rank_by in [
        RankMetric::Sharpe,
        RankMetric::TotalReturn,
        RankMetric::MaxDrawdown,
        RankMetric::FinalEquity,
    ] {
        let run = |exec| run_paramscan_exec(&base, store.clone(), rank_by, exec).unwrap();
        let seq = run(ParamscanExec::Sequential);
        let par = run(ParamscanExec::Parallel);

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
fn sweep_exec_defaults_to_parallel() {
    assert_eq!(ParamscanExec::default(), ParamscanExec::Parallel);
    // `from_env` is only asserted here for the UNSET/other-value case that every CI process
    // has; mutating process env from a test would race the other tests in this binary.
    if std::env::var(SWEEP_SEQUENTIAL_ENV).is_err() {
        assert_eq!(ParamscanExec::from_env(), ParamscanExec::Parallel);
    }
}

/// The concurrency bound is a CAP, not a suggestion: parallelism multiplies the materialized
/// data slice (module doc), so with `VIKE_SWEEP_THREADS` unset the pool must be sized
/// `min(DEFAULT_SWEEP_THREADS, cores)` — never one worker per logical core, and never zero
/// (rayon reads `num_threads(0)` as "all cores", the exact shape this cap prevents).
#[test]
fn sweep_threads_defaults_to_a_small_cap() {
    // ⚠ The bound is asserted on `default_sweep_threads`, not on `sweep_threads`, and that is
    // not a weakening — it is the only order-independent way to assert it. `sweep_threads`
    // now consults a process-wide `OnceLock` that a sibling test writes, and `cargo test`
    // defines no order between the two, so reading the resolved answer here would pass or
    // fail by scheduling.
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let n = default_sweep_threads();
    assert!(n >= 1, "a zero-sized pool would mean 'all cores' to rayon");
    assert!(n <= DEFAULT_SWEEP_THREADS, "default concurrency must stay capped, got {n}");
    assert!(n <= cores, "never more workers than cores, got {n} on {cores} cores");

    // …and the RESOLVED answer goes through it when neither lever is set. Same reason as
    // `ParamscanExec::from_env` above: only asserted for the unset case, never by mutating
    // process env.
    if std::env::var(SWEEP_THREADS_ENV).is_err() && INSTALLED_SWEEP_THREADS.get().is_none() {
        assert_eq!(sweep_threads(), n, "with no lever set, the fallback IS the answer");
    }
}

/// **The INSTALLED cap is what the pool reads** — `preferences.sweep_threads` reaching rayon,
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
/// on `sweep_threads`: it is then independent of whether this test ran first.
///
/// ⚠ **Mutation proof, MEASURED (production code):** make [`install_sweep_threads`]'s
/// `Some(n) if n > 0` arm return `false` without touching the `OnceLock` — the seam doing
/// nothing, which is what the composition roots' calls would amount to — and this reddens on
/// "the first real install must take".
///
/// ⚠ **This used to need a FEATURE to run at all**, worth knowing because a stale assumption
/// here would have hidden a real gap: `harness` was `#[cfg(feature = "hist-replay")]` until the
/// 2026-09-27 feature collapse made it this crate's DEFAULT build, so a plain
/// `cargo test -p vike-backtest` now compiles and runs this file's tests with no feature flag
/// at all — including in the roster's default lane, not only the `datafusion-store` one that
/// builds the `backtest` bin this key's composition root lives in.
#[test]
fn an_installed_cap_is_what_the_pool_reads_and_a_second_install_is_refused() {
    // `None` installs nothing — a process whose file and environment both say nothing keeps
    // the compiled-in fallback.
    assert!(!install_sweep_threads(None), "`None` must install nothing");
    // …and neither does a zero, which `vike_config::Preferences::apply` refuses long before
    // here (it would build a pool that never runs a point).
    assert!(!install_sweep_threads(Some(0)), "a zero must install nothing");

    // Above the compiled-in cap, so the fallback can never produce it by accident.
    const INSTALLED: usize = DEFAULT_SWEEP_THREADS + 7;
    assert!(install_sweep_threads(Some(INSTALLED)), "the first real install must take");
    assert_eq!(
        sweep_threads(),
        INSTALLED,
        "the pool must read the cap the composition root installed — this is the whole of \
             what makes `preferences.sweep_threads` a setting rather than a declaration, and it \
             must hold whether or not VIKE_SWEEP_THREADS is exported (the installed value IS the \
             resolved env-over-file answer)"
    );

    // A SECOND install is refused rather than honoured: two roots in one process is a bug, and
    // silently moving the bound under a pool that may already be built is the worse failure.
    assert!(!install_sweep_threads(Some(INSTALLED + 1)), "a second install must be refused");
    assert_eq!(sweep_threads(), INSTALLED, "…and must not have changed the answer");
}

#[test]
fn run_sweep_records_failure() {
    // A synthetic failed row (no live backtest error path is easy to trigger here without
    // network/venue setup) exercises the sort-puts-failures-last rule and the Display FAILED
    // branch directly — the real error-capture wiring (`Err(e) => ... error: Some(..)`) is
    // exercised by every OTHER run_paramscan test succeeding without ever populating `error`.
    let ok_report = BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
    };
    let mut rows = vec![
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(9.0))],
            report: None,
            error: Some("boom".to_string()),
            score: None,
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
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
    let rep = ParamscanReport { rows, rank_by: RankBy::Metric(RankMetric::Sharpe), summary: None };

    // ⚠ AN ABSENT SUMMARY ADDS NO KEY, which is what keeps a GRID document byte-identical to
    // the one this crate emitted before the field existed — the property
    // `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
    // `profile_sweep_is_byte_identical_local_and_remote` compares bytes for. A present one is
    // carried, so a remote euler or tpe run can report the budget the engine prints to a stderr
    // no socket carries.
    let json = serde_json::to_string(&rep).expect("serializes");
    assert!(!json.contains("summary"), "an absent summary adds NO key: {json}");
    let searched = ParamscanReport {
        rows: Vec::new(),
        rank_by: RankBy::Objective("multi".to_string()),
        summary: Some("tpe: 128 trials".to_string()),
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
    let good = BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
    };
    let degenerate = BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
    };
    // NaN row deliberately placed first, so a broken comparator would leave it ranked #1.
    let mut rows = [
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
            report: Some(degenerate),
            error: None,
            score: None,
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
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
fn rep(total_return: f64, sharpe: f64, max_dd: f64, final_eq: f64) -> BacktestReport {
    BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
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
fn scored_rep(total_return: f64, max_dd: f64, pf: f64, win_rate: f64, n: usize) -> BacktestReport {
    BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
    }
}

/// REGRESSION (the sign law, through the RANKING comparator): on an ALL-LOSING grid — the
/// common case when tuning a bad strategy — `--rank-by multi` must crown the LEAST-bad point.
/// The pre-fix score multiplied a negative base by sub-1 shaping factors, so the catastrophic
/// point (win_rate 0 -> coeff 0, pf 0 -> ln(1) = 0) collapsed to `-0.0` and ranked #1, and the
/// thin 1-trade sample outranked the 100-trade one for the same reason.
#[test]
fn all_losing_grid_never_crowns_the_worst_point() {
    use crate::search::objective::{MultiMetricParams, multi_metric_score};

    let p = MultiMetricParams::default();
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
/// "unrankable" NaN score goes through `ser_opt_score` to `null` — never a bare `NaN` token
/// (invalid JSON) and never a serialization error. `None` staying skipped is pinned by
/// `run_sweep_ranks_points`.
#[test]
fn objective_json_stamps_scores_and_nulls_unrankable() {
    let rows = vec![
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
            report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
            error: None,
            score: Some(1.25),
        },
        ParamscanRow {
            overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
            report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
            error: None,
            score: Some(f64::NAN),
        },
    ];
    let report =
        ParamscanReport { rows, rank_by: RankBy::Objective("multi".to_string()), summary: None };

    let json = serde_json::to_string(&report).expect("an unrankable row must not fail --json");
    assert!(!json.contains("NaN"), "a raw NaN token would be invalid JSON: {json}");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["rank_by"], "multi", "an objective serializes as its label");
    assert_eq!(v["rows"][0]["score"], 1.25);
    assert!(v["rows"][1]["score"].is_null(), "NaN score must serialize as null: {json}");
}

/// The objective path end-to-end over a real store: scores are stamped, rows sort best-first
/// by score, the header names the objective, and the Sharpe objective reproduces the classic
/// `run_paramscan` order on the same data.
#[cfg(feature = "datafusion-store")]
#[test]
fn run_sweep_with_ranks_by_objective() {
    use crate::search::objective::{MultiMetricParams, multi_metric};
    use vike_data::DataFusionHist;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0]");

    // (a) multi objective: every successful row gets a score, sorted best-first (NaN-aware).
    let obj = multi_metric(MultiMetricParams::default());
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
    assert!(s.contains("sweep ranked by multi"));
    assert!(s.contains("score="), "objective table shows the score column");

    // (b) the Sharpe built-in objective orders the rows exactly like the classic path.
    let classic = run_paramscan(&base, store.clone(), RankMetric::Sharpe).unwrap();
    let sharpe_obj = RankMetric::Sharpe.objective();
    let via_obj = run_paramscan_with(&base, store, &sharpe_obj, "sharpe").unwrap();
    let order = |r: &ParamscanReport| -> Vec<Vec<(String, toml::Value)>> {
        r.rows.iter().map(|row| row.overrides.clone()).collect()
    };
    assert_eq!(order(&classic), order(&via_obj), "built-in objective must match run_paramscan");
}

/// ParamscanReport wiring: a zero-trade / flat row surfaces its top probable cause inline in the
/// ranked table, while a row that traded is untouched (byte-identical). Built from literal rows
/// so it does not depend on a strategy that happens to produce a flat run.
#[test]
fn sweep_table_annotates_zero_trade_rows_only() {
    let traded = BacktestReport {
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
        extended: None,
        honesty: None,
        realism: None,
    };
    let flat = BacktestReport {
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
    };
    let report = ParamscanReport {
        rows: vec![
            ParamscanRow {
                overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
                report: Some(traded),
                error: None,
                score: None,
            },
            ParamscanRow {
                overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
                report: Some(flat),
                error: None,
                score: None,
            },
        ],
        rank_by: RankBy::Metric(RankMetric::Sharpe),
        summary: None,
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
