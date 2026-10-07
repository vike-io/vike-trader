use super::*;
use crate::harness::sweep::sweep_tests::base_with_paramscan;
// `RankMetric`, the concrete `DataFusionHist` and the flat-bar builder are used ONLY by the
// store-backed tests below (the pure axis/budget tests need none of them), so all three are gated
// behind `datafusion-store`; the `HistStore` trait those tests call `append_bars` through is
// already in scope via `super::*`.
#[cfg(feature = "datafusion-store")]
use crate::harness::sweep::{RankBy, RankMetric};
#[cfg(feature = "datafusion-store")]
use vike_data::DataFusionHist;
#[cfg(feature = "datafusion-store")]
use vike_marketdata::test_support::flat_bar_zero_volume;

/// The tempdir is returned alongside the store: dropping it would delete the parquet tree the
/// store still reads from, so the caller must keep it alive for the whole test.
#[cfg(feature = "datafusion-store")]
fn store_with_bars() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let bars = vec![
        flat_bar_zero_volume(0, 100.0),
        flat_bar_zero_volume(1000, 101.0),
        flat_bar_zero_volume(2000, 102.0),
        flat_bar_zero_volume(3000, 99.0),
    ];
    store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();
    (dir, store)
}

#[test]
fn float_and_integer_axes_are_classified() {
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0]\nn = [1, 5, 9]");
    let axes = numeric_axes(&base).unwrap();
    assert_eq!(axes.len(), 2);
    // Axes sort by key: n, size.
    assert_eq!(axes[0].0.key, "n");
    assert_eq!(axes[0].1, AxisKind::Integer);
    assert!(axes[0].0.integral);
    assert_eq!(axes[1].0.key, "size");
    assert_eq!(axes[1].1, AxisKind::Float);
}

#[test]
fn non_numeric_axis_is_a_validation_error_pointing_at_grid() {
    let base = base_with_paramscan("[sweep]\nmode = [\"a\", \"b\"]");
    let err = numeric_axes(&base).unwrap_err();
    match err {
        // The refusal names the METHOD an operator would reach for, read off `GridSearch`'s
        // own `name()` — never a flag spelling this assertion would then be pinning.
        HarnessError::Validation(m) => assert!(m.contains(GridSearch.name()), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn non_sweep_profile_is_rejected() {
    let base = base_with_paramscan("");
    assert!(matches!(numeric_axes(&base), Err(HarnessError::Validation(_))));
}

/// A `PointEvaluator` that COUNTS, and refuses to answer. It exists so the test below can
/// assert the number that matters — how many points euler evaluated before it gave up — which
/// no assertion about a constructor can reach.
#[derive(Default)]
struct CountingEvaluator {
    calls: std::sync::atomic::AtomicUsize,
}

impl PointEvaluator for CountingEvaluator {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<crate::harness::optimize::Evaluated> {
        self.calls.fetch_add(batch.len(), std::sync::atomic::Ordering::SeqCst);
        batch
            .into_iter()
            .map(|c| crate::harness::optimize::Evaluated {
                row: ParamscanRow {
                    overrides: c,
                    report: None,
                    error: Some("counting evaluator: no backtest".to_string()),
                    score: None,
                },
                score: f64::NAN,
            })
            .collect()
    }

    fn rank_by(&self) -> super::super::sweep::RankBy {
        super::super::sweep::RankBy::Objective("counting".to_string())
    }
}

/// ⚠ A NON-TABLE `strategy.params` ABORTS EULER BEFORE THE FIRST EVALUATION, and the assertion
/// is the COUNT — because "immediately" is a claim about how much work happened, and the
/// previous version of this test could not see it.
///
/// # What it used to be, and why that was not a gate
///
/// The first draft asserted that `StoreEvaluator::new` returns `Err` and that
/// `run_paramscan_euler_exec` reports `strategy.params`. Both are true of the OLD implementation
/// too — the one this seam deletes, which latched the build failure in `build_err`, burned the
/// WHOLE `max_depth` budget evaluating nothing, and re-raised at the end. A test that passes
/// against the behaviour it was written to forbid is not a gate, which three reviewers said
/// independently.
///
/// So this drives the real `optimize` door with a counting evaluator and pins `calls == 0`:
/// the preflight runs before `accepts`, `accepts` before `search`, and `search` never reaches
/// a batch. With `max_depth = 6` the old latch would have counted a full neighbourhood per
/// depth — the difference between 0 and "some hundreds" is the whole point.
#[test]
fn a_non_table_params_profile_aborts_euler_before_any_evaluation() {
    let base = BacktestProfile::from_toml_str(
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
params = 3
[sweep]
size = [1.0, 2.0, 3.0]
"#,
    )
    .expect("a non-table `strategy.params` parses — it is the SWEEP that cannot use it");

    // The space itself is fine: three floats on one axis is exactly what euler searches. The
    // fault is the profile's params, and this line is what keeps the two apart.
    let cfg = EulerConfig::with_depth(6);
    EulerSearch::new(cfg)
        .accepts(&base)
        .expect("a three-value float axis IS a searchable space — the space is not the fault");

    let counting = CountingEvaluator::default();
    match crate::harness::optimize::optimize(&EulerSearch::new(cfg), &base, &counting) {
        Err(HarnessError::Validation(m)) => assert!(m.contains("strategy.params"), "{m}"),
        other => panic!("expected the preflight refusal, got {other:?}"),
    }
    assert_eq!(
        counting.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "euler evaluated points over a profile whose params cannot be overridden — the \
             preflight did not run first, and the depth budget was being burned on a fault that \
             was knowable before the first backtest"
    );
}

#[test]
fn integer_axis_overrides_stay_integers() {
    let base = base_with_paramscan("[sweep]\nn = [1, 5, 9]");
    let axes = numeric_axes(&base).unwrap();
    let (profile, overrides) = profile_for(&base, &axes, &[6.0]).unwrap();
    assert!(profile.paramscan.is_none(), "a searched point is a plain single-run profile");
    assert_eq!(overrides, vec![("n".to_string(), toml::Value::Integer(6))]);
    assert_eq!(profile.strategy.params.get("n").unwrap().as_integer(), Some(6));
}

/// End-to-end over a real store: the search runs, every evaluated point becomes a scored row
/// ranked best-first, and the budget reports strictly fewer evaluations than the
/// equal-resolution grid.
#[cfg(feature = "datafusion-store")]
#[test]
fn euler_sweep_runs_and_reports_its_budget() {
    let (_dir, store) = store_with_bars();
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0]");
    let objective = RankMetric::Sharpe.objective();
    let cfg = EulerConfig::with_depth(3);

    let (report, budget) = run_paramscan_euler(&base, store, &objective, "sharpe", &cfg).unwrap();

    assert!(!report.rows.is_empty());
    assert_eq!(report.rank_by, RankBy::Objective("sharpe".to_string()));
    assert_eq!(budget.evaluated, report.rows.len(), "one row per evaluation");
    assert!(budget.depth_reached <= cfg.max_depth);
    // For THIS shape (one 3-value axis) the bound is structural: the grid at depth d is
    // 2^d*2+1 while the search spends at most 3 + d*2. It is not a universal invariant across
    // axis counts, which is why `MAX_AXES` caps the width rather than the assertion claiming it.
    assert!(
        budget.equivalent_grid >= budget.evaluated,
        "a single-axis refinement must not cost more than the grid at the depth it reached: \
             {budget}"
    );

    // Ranked best-first by score, NaN scores after finite ones.
    let scores: Vec<f64> = report.rows.iter().filter_map(|r| r.score).collect();
    for w in scores.windows(2) {
        if w[0].is_nan() || w[1].is_nan() {
            assert!(!w[0].is_nan(), "a NaN score must not rank above a finite one: {scores:?}");
            continue;
        }
        assert!(w[0] >= w[1], "rows must be ranked best-first: {scores:?}");
    }

    assert!(budget.to_string().contains("grid at the depth reached"));
}

/// The budget must describe the resolution REACHED: a run that stops at depth 0 (nothing new
/// reachable) may not advertise the `max_depth` grid it never approached.
#[cfg(feature = "datafusion-store")]
#[test]
fn budget_reports_the_grid_at_the_depth_actually_reached() {
    let (_dir, store) = store_with_bars();
    // An integral axis pinned at its box edge: the first halving floors at step 1 and every
    // candidate canonicalizes onto an already-evaluated point, so depth_reached stays 0.
    let base = base_with_paramscan("[sweep]\nn = [1, 2, 3]");
    let objective = RankMetric::Sharpe.objective();

    let (_report, budget) =
        run_paramscan_euler(&base, store, &objective, "sharpe", &EulerConfig::with_depth(6))
            .unwrap();
    assert_eq!(budget.max_depth, 6, "the requested depth is still reported as such");
    assert!(
        budget.equivalent_grid
            < equivalent_grid_evaluations(
                &[SearchAxis::integral("n", vec![1.0, 2.0, 3.0])],
                &EulerConfig::with_depth(6)
            ),
        "an early-stopped run must not be credited with the max_depth grid: {budget}"
    );
}

#[test]
fn mixed_int_and_float_axis_is_rejected_pointing_at_grid() {
    let base = base_with_paramscan("[sweep]\nn = [1, 2, 2.5]");
    match numeric_axes(&base).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("mixes integer and float"), "{m}");
            assert!(m.contains(GridSearch.name()), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn too_many_axes_is_rejected() {
    let sweep: String = std::iter::once("[sweep]".to_string())
        .chain((0..MAX_AXES + 1).map(|i| format!("k{i} = [1, 2]")))
        .collect::<Vec<_>>()
        .join("\n");
    let base = base_with_paramscan(&sweep);
    match numeric_axes(&base).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("at most"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// DETERMINISM GATE (the rayon lane): an Euler search evaluated on the parallel pool must
/// produce a report AND budget byte-identical to the sequential one — same evaluated points,
/// same rows in the same order, same scores, same depth. Rayon's order-preserving collect plus
/// the batch fold in `euler_search_batched` is what buys this; nothing may depend on which
/// point finished first.
#[cfg(feature = "datafusion-store")]
#[test]
fn euler_parallel_and_sequential_are_byte_identical() {
    let (_dir, store) = store_with_bars();
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0, 4.0]");
    let objective = RankMetric::Sharpe.objective();
    let cfg = EulerConfig::with_depth(3);

    let run = |exec| {
        run_paramscan_euler_exec(&base, store.clone(), &objective, "sharpe", &cfg, exec).unwrap()
    };
    let (seq, seq_budget) = run(ParamscanExec::Sequential);
    let (par, par_budget) = run(ParamscanExec::Parallel);

    assert_eq!(seq_budget, par_budget, "the budget must not depend on execution strategy");
    assert_eq!(seq.rows.len(), par.rows.len());
    assert_eq!(seq.to_string(), par.to_string(), "the ranked table must be byte-identical");
    assert_eq!(
        serde_json::to_string(&seq).unwrap(),
        serde_json::to_string(&par).unwrap(),
        "--json output must be byte-identical"
    );
}

/// `EulerBudget` derives `Serialize`; pin the field names so the derive is not dead surface.
#[test]
fn budget_serializes_with_stable_field_names() {
    let budget = EulerBudget { evaluated: 7, equivalent_grid: 17, depth_reached: 3, max_depth: 4 };
    let json = serde_json::to_value(budget).unwrap();
    assert_eq!(json["evaluated"], 7);
    assert_eq!(json["equivalent_grid"], 17);
    assert_eq!(json["depth_reached"], 3);
    assert_eq!(json["max_depth"], 4);
}

/// Depth 0 makes the Euler path evaluate exactly the coarse grid — the same POINTS the grid
/// sweep runs, which pins that the search's coarse pass is the grid and nothing else.
#[cfg(feature = "datafusion-store")]
#[test]
fn depth_zero_evaluates_the_same_points_as_the_grid() {
    let (_dir, store) = store_with_bars();
    let base = base_with_paramscan("[sweep]\nsize = [1.0, 2.0, 3.0]");
    let objective = RankMetric::Sharpe.objective();

    let (report, budget) =
        run_paramscan_euler(&base, store, &objective, "sharpe", &EulerConfig::with_depth(0))
            .unwrap();
    assert_eq!(budget.evaluated, 3, "depth 0 is the plain 3-point grid");
    assert_eq!(budget.equivalent_grid, 3);

    let grid = super::super::sweep::expand_paramscan(&base).unwrap();
    let mut euler_sizes: Vec<f64> =
        report.rows.iter().map(|r| r.overrides[0].1.as_float().expect("float axis")).collect();
    let mut grid_sizes: Vec<f64> =
        grid.iter().map(|p| p.overrides[0].1.as_float().expect("float axis")).collect();
    euler_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    grid_sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(euler_sizes, grid_sizes, "depth 0 covers exactly the grid's points");
}

/// CROSS-OPTIMIZER REGRESSION (the shared ordering rule, end-to-end): before
/// `sort_scored_rows`/`eval_scored_point` were shared, each lane hand-rolled the ranking with
/// only a comment claiming it matched `run_paramscan_with` — nothing gated it. On ONE tiny fixed
/// grid (an all-integer `size` axis `BuyHold` genuinely reads, over a losing slice so the
/// three points score strictly distinct total returns and best-first order is unambiguous):
///
/// - euler at depth 0 evaluates exactly the coarse grid, so its ranked override sequence must
///   EQUAL `run_paramscan_with`'s, with bit-identical scores per rank;
/// - TPE over the same integral axis proposes only on-grid values (round + clamp), so its rows
///   re-score the same three backtests: the ranked sequence must follow the grid's rank order
///   (duplicates adjacent, never interleaved out of rank), with bit-identical scores per point.
#[cfg(feature = "datafusion-store")]
#[test]
fn euler_and_tpe_rank_rows_exactly_like_run_sweep_with() {
    use super::super::sweep::run_paramscan_with;
    use super::super::tpe::run_tpe;
    use vike_ml::tpe::TpeConfig;

    let (_dir, store) = store_with_bars();
    let base = base_with_paramscan("[sweep]\nsize = [1, 2, 3]");
    let objective = RankMetric::TotalReturn.objective();

    let grid = run_paramscan_with(&base, store.clone(), &objective, "return").unwrap();
    assert_eq!(grid.rows.len(), 3, "one row per grid point");
    assert!(grid.rows.iter().all(|r| r.report.is_some() && r.score.is_some()));
    let grid_order: Vec<toml::Value> = grid.rows.iter().map(|r| r.overrides[0].1.clone()).collect();
    let grid_scores: Vec<f64> = grid.rows.iter().map(|r| r.score.unwrap()).collect();
    // Strictly distinct scores — otherwise "identical order" would be vacuous under the
    // stable sort's tie handling.
    for w in grid_scores.windows(2) {
        assert!(w[0] > w[1], "fixture must score strictly distinct points: {grid_scores:?}");
    }

    // (a) euler depth 0 == the coarse grid: same ranked sequence, same score bits.
    let (eu, _budget) = run_paramscan_euler(
        &base,
        store.clone(),
        &objective,
        "return",
        &EulerConfig::with_depth(0),
    )
    .unwrap();
    let eu_order: Vec<toml::Value> = eu.rows.iter().map(|r| r.overrides[0].1.clone()).collect();
    assert_eq!(eu_order, grid_order, "euler depth-0 must rank exactly like run_paramscan_with");
    for (g, e) in grid.rows.iter().zip(&eu.rows) {
        assert_eq!(
            g.score.unwrap().to_bits(),
            e.score.unwrap().to_bits(),
            "the same point must score bit-identically across optimizers"
        );
    }

    // (b) TPE rows follow the grid's rank order, and each point scores bit-identically.
    let tpe = run_tpe(&base, store, &objective, "return", &TpeConfig::new(8, 7)).unwrap();
    assert_eq!(tpe.rows.len(), 8, "one row per trial");
    let rank_of = |v: &toml::Value| -> usize {
        grid_order
            .iter()
            .position(|g| g == v)
            .expect("an integral-axis TPE proposal must land on a grid value")
    };
    let ranks: Vec<usize> = tpe.rows.iter().map(|r| rank_of(&r.overrides[0].1)).collect();
    for w in ranks.windows(2) {
        assert!(w[0] <= w[1], "tpe rows must follow run_paramscan_with's rank order: {ranks:?}");
    }
    for row in &tpe.rows {
        assert_eq!(
            row.score.unwrap().to_bits(),
            grid_scores[rank_of(&row.overrides[0].1)].to_bits(),
            "the same point must score bit-identically across optimizers"
        );
    }
}
