//! Euler (successive-halving) parameter search over a `[sweep]` profile — the store-backed wrapper
//! around the pure search core (`crate::search`).
//!
//! ADDITIVE AND OPT-IN. The default sweep path (`run_sweep`/`run_sweep_with`, `--search grid`) is
//! untouched: the same cartesian grid, the same comparator, byte-identical output. `--search euler`
//! instead evaluates the coarse grid ONCE and then refines locally around the best point, halving
//! the per-axis step up to [`crate::search::EulerConfig::max_depth`] times — far fewer backtests
//! for the resolution it reaches (see `crate::search`'s module doc for the cost formula and the
//! local-refinement caveat). The reported [`EulerBudget`] compares against the grid matching the
//! depth ACTUALLY reached, never the requested `max_depth`, so an early-stopped run cannot
//! advertise a saving it did not buy.
//!
//! Scoring reuses the existing objective seam verbatim ([`crate::objective::Objective`]): a
//! `RankMetric`'s built-in objective or the composite `multi_metric`, same "higher is better,
//! `NaN` = unrankable" convention. Nothing about scoring is reimplemented here.
//!
//! REQUIREMENT: every `[sweep]` axis must be numeric AND type-homogeneous (an all-integer or an
//! all-float TOML array). A string/bool axis has no notion of "halfway between two values"; a MIXED
//! `[1, 2.5]` axis would silently change the TOML type the strategy receives versus the grid path
//! (which clones each raw `toml::Value`), so a strategy reading the param via `as_integer()` would
//! behave differently under `--search euler` than under `--search grid`. Both are a
//! [`HarnessError::Validation`] telling the caller to use `--search grid`.
//!
//! PARALLELISM (additive, result-neutral): a refinement DEPTH is inherently sequential — each one
//! centres on the previous depth's winner — but the points WITHIN one depth's neighbourhood (and
//! within the coarse grid) are independent backtests. [`run_sweep_euler`] therefore drives
//! [`crate::search::euler_search_batched`] and evaluates each already-deduped batch on the BOUNDED
//! sweep pool (`sweep::install_bounded` — NOT rayon's global pool; each concurrent point
//! materializes its own copy of the data slice, so the worker count is a peak-RSS multiplier, capped
//! by `VIKE_SWEEP_THREADS`, see `sweep`'s module doc), collecting in INPUT order. The trace, the
//! rows, the budget and the ranking are identical to the one-at-a-time path (pinned by `search`'s
//! `batched_search_matches_the_point_wise_search_bit_for_bit` and by
//! `euler_parallel_and_sequential_are_byte_identical` below).
//! [`SweepExec::Sequential`] (or `VIKE_SWEEP_SEQUENTIAL=1`) forces the old loop.

use std::sync::Arc;

use rayon::prelude::*;
use vike_data::HistStore;

use crate::objective::Objective;
use crate::search::{equivalent_grid_evaluations, euler_search_batched, EulerConfig, SearchAxis};

use super::sweep::{
    eval_scored_point, numeric_sweep_axes, profile_with_overrides, sort_scored_rows, RankBy,
    SweepExec, SweepReport, SweepRow,
};
use super::{BacktestProfile, HarnessError};

/// How a `[sweep]` axis's values are rendered back into TOML: an all-integer coarse axis stays
/// integral (both in the profile override AND in the refinement, which then never proposes a
/// fractional value), anything else is a float axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxisKind {
    Integer,
    Float,
}

impl AxisKind {
    fn to_toml(self, x: f64) -> toml::Value {
        match self {
            AxisKind::Integer => toml::Value::Integer(x.round() as i64),
            AxisKind::Float => toml::Value::Float(x),
        }
    }
}

/// Hard ceiling on the number of `[sweep]` axes an Euler search accepts. Each refinement depth
/// evaluates a `3^d` neighbourhood, so the per-depth cost grows FASTER in `d` than the equal-
/// resolution grid does for low-cardinality axes; past this width the "cheaper than the grid"
/// premise stops holding and the caller is better served by `--search grid`. (Clamping at the box
/// corner plus the dedup keep the realised count well under in practice, but that is empirical —
/// this guard is what makes the bound structural.)
const MAX_AXES: usize = 8;

/// Read the profile's `[sweep]` table as numeric search axes (axes sorted by key — the same
/// deterministic axis order `expand_sweep` uses). The shared reader
/// (`sweep::numeric_sweep_axes`) owns the array/non-empty/numeric/type-homogeneity validation;
/// this adapter adds the euler-only width guard and renders each axis as a
/// [`SearchAxis`] + [`AxisKind`] pair.
fn numeric_axes(base: &BacktestProfile) -> Result<Vec<(SearchAxis, AxisKind)>, HarnessError> {
    // Width guard FIRST — before per-axis validation, preserving the original error precedence
    // (a too-wide sweep reports the width problem even when an axis is also non-numeric).
    if let Some(sweep) = base.sweep.as_ref().filter(|_| base.is_sweep()) {
        if sweep.len() > MAX_AXES {
            return Err(HarnessError::Validation(format!(
                "euler search supports at most {MAX_AXES} sweep axes, got {} — the 3^d refinement \
                 neighbourhood stops being cheaper than the grid past that; use --search grid",
                sweep.len()
            )));
        }
    }

    Ok(numeric_sweep_axes(base, "euler")?
        .into_iter()
        .map(|axis| {
            let kind = if axis.integral { AxisKind::Integer } else { AxisKind::Float };
            let search = match kind {
                AxisKind::Integer => SearchAxis::integral(axis.key, axis.values),
                AxisKind::Float => SearchAxis::new(axis.key, axis.values),
            };
            (search, kind)
        })
        .collect())
}

/// Build the single-run profile for one coordinate vector: each axis's coordinate rendered back
/// as its TOML type, folded through the shared `sweep::profile_with_overrides` — exactly the
/// shape `expand_sweep` produces, so a searched point runs through the identical `run_backtest`
/// path a grid point does.
fn profile_for(
    base: &BacktestProfile,
    axes: &[(SearchAxis, AxisKind)],
    point: &[f64],
) -> Result<(BacktestProfile, Vec<(String, toml::Value)>), HarnessError> {
    let overrides = axes
        .iter()
        .zip(point)
        .map(|((axis, kind), &x)| (axis.key.clone(), kind.to_toml(x)))
        .collect();
    profile_with_overrides(base, overrides)
}

/// The budget an Euler run would cost against the grid that reaches the same resolution — the
/// tradeoff, reported so a caller can see what the refinement bought.
///
/// `equivalent_grid` is the exact point count of the cartesian grid matching the resolution the run
/// ACTUALLY reached — derived from `depth_reached`, NOT from `max_depth`, so a search that stopped
/// early cannot advertise a saving against a finer grid it never approached. `evaluated` is what the
/// search really spent (only known after the run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct EulerBudget {
    pub evaluated: usize,
    pub equivalent_grid: usize,
    pub depth_reached: u32,
    pub max_depth: u32,
}

impl std::fmt::Display for EulerBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "euler: {} evaluations at depth {}/{} (grid at the depth reached would be {})",
            self.evaluated, self.depth_reached, self.max_depth, self.equivalent_grid
        )
    }
}

/// Run an Euler successive-halving search over `base`'s `[sweep]` axes, scoring every evaluated
/// point with `objective` and returning the SAME [`SweepReport`] shape the grid path returns (rows
/// ranked best-first by score, failures last) plus the [`EulerBudget`] the run cost.
///
/// Only the points the search actually evaluated appear as rows — that IS the saving. A failed
/// backtest scores `NaN` (unrankable), so a broken corner of the space can never become the
/// refinement's centre.
pub fn run_sweep_euler(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    cfg: &EulerConfig,
) -> Result<(SweepReport, EulerBudget), HarnessError> {
    run_sweep_euler_exec(base, store, objective, label, cfg, SweepExec::from_env())
}

/// Evaluate ONE searched coordinate vector into its report row + score. Pure per-point work over
/// shared-by-reference inputs — the unit the batch below hands to rayon. The shared
/// `sweep::eval_scored_point` does the `run_backtest` → report → objective fold (a failed
/// backtest becomes a row with `error` set and scores `NaN` — unrankable, never a refinement
/// centre); a profile-BUILD failure (`strategy.params` is not a table) is an `Err` the caller
/// re-raises after the search.
fn eval_point(
    base: &BacktestProfile,
    axes: &[(SearchAxis, AxisKind)],
    store: &Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    point: &[f64],
) -> Result<(SweepRow, f64), HarnessError> {
    let (profile, overrides) = profile_for(base, axes, point)?;
    Ok(eval_scored_point(base, store, objective, &profile, overrides))
}

/// [`run_sweep_euler`] with the execution strategy passed explicitly instead of read from the env —
/// the determinism gate's lever. Both [`SweepExec`] variants return byte-identical reports.
pub fn run_sweep_euler_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    cfg: &EulerConfig,
    exec: SweepExec,
) -> Result<(SweepReport, EulerBudget), HarnessError> {
    let axes = numeric_axes(base)?;
    let search_axes: Vec<SearchAxis> = axes.iter().map(|(a, _)| a.clone()).collect();

    // Row-per-evaluation, appended in EVALUATION order (= batch input order, which rayon preserves)
    // inside the batch closure; a profile-build failure is latched and re-raised after the search
    // rather than panicking mid-closure.
    let mut rows: Vec<SweepRow> = Vec::new();
    let mut build_err: Option<HarnessError> = None;

    let trace = euler_search_batched(&search_axes, cfg, |batch| {
        let evaluated: Vec<Result<(SweepRow, f64), HarnessError>> = if exec == SweepExec::Sequential
        {
            batch.iter().map(|p| eval_point(base, &axes, &store, objective, p.as_slice())).collect()
        } else {
            // BOUNDED pool, never rayon's global one — each concurrent point materializes its
            // own copy of the data slice, so worker count IS the peak-RSS multiplier (see
            // `sweep`'s module doc). One pool per batch; a batch is one refinement DEPTH, so
            // the spawn cost is amortized over a whole neighbourhood of backtests.
            super::sweep::install_bounded(|| -> Vec<Result<(SweepRow, f64), HarnessError>> {
                batch
                    .par_iter()
                    .map(|p| eval_point(base, &axes, &store, objective, p.as_slice()))
                    .collect()
            })
        };

        let mut scores = Vec::with_capacity(evaluated.len());
        for outcome in evaluated {
            match outcome {
                Ok((row, score)) => {
                    rows.push(row);
                    scores.push(score);
                }
                Err(e) => {
                    // Latch the FIRST build failure (it is a property of `base`, so every point in
                    // the batch fails identically) and score the point unrankable so the search
                    // never refines around it; `run_sweep_euler_exec` re-raises it below.
                    if build_err.is_none() {
                        build_err = Some(e);
                    }
                    scores.push(f64::NAN);
                }
            }
        }
        scores
    });

    if let Some(e) = build_err {
        return Err(e);
    }

    // THE shared ordering rule (`sweep::sort_scored_rows`, the same call `run_sweep_with` makes):
    // score descending, NaN after finite, failures last.
    sort_scored_rows(&mut rows);

    let budget = EulerBudget {
        evaluated: trace.n_evaluated(),
        // The grid matching the resolution ACTUALLY attained — `depth_reached`, not `max_depth`.
        equivalent_grid: equivalent_grid_evaluations(
            &search_axes,
            &EulerConfig::with_depth(trace.depth_reached),
        ),
        depth_reached: trace.depth_reached,
        max_depth: cfg.max_depth,
    };

    Ok((SweepReport { rows, rank_by: RankBy::Objective(label.into()) }, budget))
}

#[cfg(test)]
mod tests {
    use super::*;
    // `RankMetric` and the concrete `DataFusionHist` are used ONLY by the store-backed tests below
    // (the pure axis/budget tests need neither), so both are gated behind `datafusion-store`; the
    // `HistStore` trait those tests call `append_bars` through is already in scope via `super::*`.
    #[cfg(feature = "datafusion-store")]
    use crate::harness::sweep::RankMetric;
    #[cfg(feature = "datafusion-store")]
    use vike_data::DataFusionHist;

    fn base_with_sweep(sweep: &str) -> BacktestProfile {
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

    #[cfg(feature = "datafusion-store")]
    fn bar(ts: i64, price: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// The tempdir is returned alongside the store: dropping it would delete the parquet tree the
    /// store still reads from, so the caller must keep it alive for the whole test.
    #[cfg(feature = "datafusion-store")]
    fn store_with_bars() -> (tempfile::TempDir, Arc<DataFusionHist>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();
        (dir, store)
    }

    #[test]
    fn float_and_integer_axes_are_classified() {
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0]\nn = [1, 5, 9]");
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
        let base = base_with_sweep("[sweep]\nmode = [\"a\", \"b\"]");
        let err = numeric_axes(&base).unwrap_err();
        match err {
            HarnessError::Validation(m) => assert!(m.contains("--search grid"), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn non_sweep_profile_is_rejected() {
        let base = base_with_sweep("");
        assert!(matches!(numeric_axes(&base), Err(HarnessError::Validation(_))));
    }

    #[test]
    fn integer_axis_overrides_stay_integers() {
        let base = base_with_sweep("[sweep]\nn = [1, 5, 9]");
        let axes = numeric_axes(&base).unwrap();
        let (profile, overrides) = profile_for(&base, &axes, &[6.0]).unwrap();
        assert!(profile.sweep.is_none(), "a searched point is a plain single-run profile");
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
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0, 3.0]");
        let objective = RankMetric::Sharpe.objective();
        let cfg = EulerConfig::with_depth(3);

        let (report, budget) = run_sweep_euler(&base, store, &objective, "sharpe", &cfg).unwrap();

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
        let base = base_with_sweep("[sweep]\nn = [1, 2, 3]");
        let objective = RankMetric::Sharpe.objective();

        let (_report, budget) =
            run_sweep_euler(&base, store, &objective, "sharpe", &EulerConfig::with_depth(6))
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
        let base = base_with_sweep("[sweep]\nn = [1, 2, 2.5]");
        match numeric_axes(&base).unwrap_err() {
            HarnessError::Validation(m) => {
                assert!(m.contains("mixes integer and float"), "{m}");
                assert!(m.contains("--search grid"), "{m}");
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
        let base = base_with_sweep(&sweep);
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
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0, 3.0, 4.0]");
        let objective = RankMetric::Sharpe.objective();
        let cfg = EulerConfig::with_depth(3);

        let run = |exec| {
            run_sweep_euler_exec(&base, store.clone(), &objective, "sharpe", &cfg, exec).unwrap()
        };
        let (seq, seq_budget) = run(SweepExec::Sequential);
        let (par, par_budget) = run(SweepExec::Parallel);

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
        let budget =
            EulerBudget { evaluated: 7, equivalent_grid: 17, depth_reached: 3, max_depth: 4 };
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
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0, 3.0]");
        let objective = RankMetric::Sharpe.objective();

        let (report, budget) =
            run_sweep_euler(&base, store, &objective, "sharpe", &EulerConfig::with_depth(0))
                .unwrap();
        assert_eq!(budget.evaluated, 3, "depth 0 is the plain 3-point grid");
        assert_eq!(budget.equivalent_grid, 3);

        let grid = super::super::sweep::expand_sweep(&base).unwrap();
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
    /// only a comment claiming it matched `run_sweep_with` — nothing gated it. On ONE tiny fixed
    /// grid (an all-integer `size` axis `BuyHold` genuinely reads, over a losing slice so the
    /// three points score strictly distinct total returns and best-first order is unambiguous):
    ///
    /// - euler at depth 0 evaluates exactly the coarse grid, so its ranked override sequence must
    ///   EQUAL `run_sweep_with`'s, with bit-identical scores per rank;
    /// - TPE over the same integral axis proposes only on-grid values (round + clamp), so its rows
    ///   re-score the same three backtests: the ranked sequence must follow the grid's rank order
    ///   (duplicates adjacent, never interleaved out of rank), with bit-identical scores per point.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn euler_and_tpe_rank_rows_exactly_like_run_sweep_with() {
        use super::super::sweep::run_sweep_with;
        use super::super::tpe::{run_tpe, TpeConfig};

        let (_dir, store) = store_with_bars();
        let base = base_with_sweep("[sweep]\nsize = [1, 2, 3]");
        let objective = RankMetric::TotalReturn.objective();

        let grid = run_sweep_with(&base, store.clone(), &objective, "return").unwrap();
        assert_eq!(grid.rows.len(), 3, "one row per grid point");
        assert!(grid.rows.iter().all(|r| r.report.is_some() && r.score.is_some()));
        let grid_order: Vec<toml::Value> =
            grid.rows.iter().map(|r| r.overrides[0].1.clone()).collect();
        let grid_scores: Vec<f64> = grid.rows.iter().map(|r| r.score.unwrap()).collect();
        // Strictly distinct scores — otherwise "identical order" would be vacuous under the
        // stable sort's tie handling.
        for w in grid_scores.windows(2) {
            assert!(w[0] > w[1], "fixture must score strictly distinct points: {grid_scores:?}");
        }

        // (a) euler depth 0 == the coarse grid: same ranked sequence, same score bits.
        let (eu, _budget) = run_sweep_euler(
            &base,
            store.clone(),
            &objective,
            "return",
            &EulerConfig::with_depth(0),
        )
        .unwrap();
        let eu_order: Vec<toml::Value> = eu.rows.iter().map(|r| r.overrides[0].1.clone()).collect();
        assert_eq!(eu_order, grid_order, "euler depth-0 must rank exactly like run_sweep_with");
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
            assert!(w[0] <= w[1], "tpe rows must follow run_sweep_with's rank order: {ranks:?}");
        }
        for row in &tpe.rows {
            assert_eq!(
                row.score.unwrap().to_bits(),
                grid_scores[rank_of(&row.overrides[0].1)].to_bits(),
                "the same point must score bit-identically across optimizers"
            );
        }
    }
}
