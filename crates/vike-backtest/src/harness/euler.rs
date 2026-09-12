//! Euler (successive-halving) parameter search over a `[sweep]` profile — the store-backed wrapper
//! around the pure search core (`crate::search`).
//!
//! ADDITIVE AND OPT-IN. The default sweep path (`run_sweep`/`run_sweep_with`, `--optimizer grid`) is
//! untouched: the same cartesian grid, the same comparator, byte-identical output. `--optimizer euler`
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
//! behave differently under `--optimizer euler` than under the grid. Both are a
//! [`HarnessError::Validation`] naming the grid method as the way to search that space.
//!
//! PARALLELISM (additive, result-neutral): a refinement DEPTH is inherently sequential — each one
//! centres on the previous depth's winner — but the points WITHIN one depth's neighbourhood (and
//! within the coarse grid) are independent backtests. [`EulerSearch`] therefore drives
//! [`crate::search::euler_search_batched`] and hands each already-deduped batch to ONE
//! `optimize::PointEvaluator::evaluate` call, which is where the fan-out lives now — the BOUNDED
//! sweep pool (`sweep::install_bounded`, NOT rayon's global pool; each concurrent point
//! materializes its own copy of the data slice, so the worker count is a peak-RSS multiplier, capped
//! by `VIKE_SWEEP_THREADS`, see `sweep`'s module doc), collecting in INPUT order. ⚠ The CADENCE is
//! unchanged by that move — one pool per `evaluate` call, and a call is still exactly one depth —
//! which is what keeps the trace, the rows, the budget and the ranking identical to the
//! one-at-a-time path (pinned by `search`'s
//! `batched_search_matches_the_point_wise_search_bit_for_bit` and by
//! `euler_parallel_and_sequential_are_byte_identical` below). [`SweepExec::Sequential`] (or
//! `VIKE_SWEEP_SEQUENTIAL=1`) forces the old loop, now as an evaluator CONSTRUCTOR argument.

use std::sync::Arc;

use vike_data::HistStore;

use crate::objective::Objective;
use crate::search::{EulerConfig, SearchAxis, equivalent_grid_evaluations, euler_search_batched};

use super::optimize::{
    Candidate, Optimizer, PointEvaluator, SearchOutcome, StoreEvaluator, report_from_outcome,
};
use super::sweep::{GridSearch, SweepExec, SweepReport, SweepRow, numeric_sweep_axes};
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
/// premise stops holding and the caller is better served by the grid method. (Clamping at the box
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
    if let Some(sweep) = base.sweep.as_ref().filter(|_| base.is_sweep())
        && sweep.len() > MAX_AXES
    {
        return Err(HarnessError::Validation(format!(
            "euler search supports at most {MAX_AXES} sweep axes, got {} — the 3^d refinement \
                 neighbourhood stops being cheaper than the grid past that; use the {} optimizer (--optimizer {})",
            sweep.len(),
            // The NAME is read off the method so the refusal cannot drift from the roster; the FLAG
            // is spelled beside it because `--optimizer` is what an operator types. Both come from
            // `GridSearch.name()`, so the method and the flag's value cannot disagree.
            GridSearch.name(),
            GridSearch.name()
        )));
    }

    // The lane name this reader interpolates into its own narrowing errors is `EulerSearch`'s own
    // `name()` — one fact, not a second literal; the arity stays as it is because five tests call
    // this function directly.
    Ok(numeric_sweep_axes(base, EulerSearch::NAME)?
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

/// Render one coordinate vector as an `optimize::Candidate`: each axis's coordinate back in its own
/// TOML type. This is the ONLY place euler's private `Vec<f64>` coordinates become the shared
/// candidate currency — which is exactly what lets `crate::search`'s raw-f64 `seen` dedup key stay
/// private and unchanged.
fn overrides_for(axes: &[(SearchAxis, AxisKind)], point: &[f64]) -> Candidate {
    axes.iter().zip(point).map(|((axis, kind), &x)| (axis.key.clone(), kind.to_toml(x))).collect()
}

/// [`overrides_for`] plus the profile it produces — TEST-ONLY since the optimizer seam landed.
///
/// The build half moved to `optimize::StoreEvaluator`, so nothing on the search path builds a
/// profile any more; this spelling survives because
/// `integer_axis_overrides_stay_integers` pins the whole chain (an integral axis renders as a TOML
/// integer AND lands in `strategy.params` as one), which is the property a renderer alone cannot
/// show.
#[cfg(test)]
fn profile_for(
    base: &BacktestProfile,
    axes: &[(SearchAxis, AxisKind)],
    point: &[f64],
) -> Result<(BacktestProfile, Candidate), HarnessError> {
    super::sweep::profile_with_overrides(base, overrides_for(axes, point))
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

/// The Euler successive-halving refinement, as an [`Optimizer`].
///
/// Its `search` is the old `run_sweep_euler_exec` body with two things REMOVED and nothing added:
///
/// * the rayon fan-out — concurrency belongs to `optimize::PointEvaluator` now, and the CADENCE is
///   unchanged (one `evaluate` call per refinement DEPTH, which is one bounded pool per depth,
///   exactly as before);
/// * the build-failure LATCH — `optimize::require_overridable_params` runs before the first depth
///   (it is every evaluator's constructor AND `optimize::optimize`'s first act), so the one error a
///   point build could raise is a property of the BASE profile that can no longer reach this loop.
///   That collapses three fault timings for one error into one and is why `evaluate` is infallible.
///
/// What stays is the LOOP, and that is the whole argument for the seam:
/// `crates/vike-backtest/src/search.rs` does not change by one line — no state promotion, no control
/// inversion — so its 22 pure tests gate unmoved code and depth accounting is untouched
/// (`equivalent_grid_evaluations` still receives `trace.depth_reached`, never `cfg.max_depth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EulerSearch {
    pub cfg: EulerConfig,
}

impl EulerSearch {
    /// This method's operator-facing name, as a const because [`numeric_axes`] interpolates it into
    /// the shared reader's narrowing errors and that function keeps its arity (five tests call it
    /// directly). One fact, two readers — never two literals.
    const NAME: &'static str = "euler";

    /// A method value over `cfg`. `EulerConfig` is `Copy`, so this holds it rather than borrowing:
    /// the trait's `&self` is what lets ONE value serve many runs.
    pub fn new(cfg: EulerConfig) -> Self {
        EulerSearch { cfg }
    }

    /// The search, keeping the TYPED [`EulerBudget`] rather than only its rendered line.
    /// [`Optimizer::search`] renders it into `SearchOutcome`'s `summary`; [`run_sweep_euler_exec`]
    /// takes it whole, because that adapter's `(SweepReport, EulerBudget)` return is a public
    /// signature this seam does not remove.
    fn search_with_budget(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<(Vec<SweepRow>, EulerBudget), HarnessError> {
        let axes = numeric_axes(base)?;
        let search_axes: Vec<SearchAxis> = axes.iter().map(|(a, _)| a.clone()).collect();

        // Row-per-evaluation, appended in EVALUATION order (= batch input order, which the
        // evaluator's order-preserving collect guarantees) inside the batch closure. UNSORTED:
        // `optimize::report_from_outcome` is the one place rows get ordered.
        let mut rows: Vec<SweepRow> = Vec::new();

        let trace = euler_search_batched(&search_axes, &self.cfg, |batch| {
            // ONE `evaluate` call per depth — the already-deduped neighbourhood, handed over whole.
            let candidates: Vec<Candidate> =
                batch.iter().map(|p| overrides_for(&axes, p.as_slice())).collect();
            let mut scores = Vec::with_capacity(candidates.len());
            for e in eval.evaluate(candidates) {
                scores.push(e.score);
                rows.push(e.row);
            }
            scores
        });

        let budget = EulerBudget {
            evaluated: trace.n_evaluated(),
            // The grid matching the resolution ACTUALLY attained — `depth_reached`, not `max_depth`.
            equivalent_grid: equivalent_grid_evaluations(
                &search_axes,
                &EulerConfig::with_depth(trace.depth_reached),
            ),
            depth_reached: trace.depth_reached,
            max_depth: self.cfg.max_depth,
        };

        Ok((rows, budget))
    }
}

impl Optimizer for EulerSearch {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The euler-only narrowing: numeric, type-homogeneous, at most `MAX_AXES` axes — with the
    /// WIDTH guard first, so a too-wide sweep reports its width even when an axis is also
    /// non-numeric. Pure and profile-only, so a space failure is now caught BEFORE a store opens.
    fn accepts(&self, base: &BacktestProfile) -> Result<(), HarnessError> {
        numeric_axes(base).map(|_| ())
    }

    fn search(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let (rows, budget) = self.search_with_budget(base, eval)?;
        // ALREADY RENDERED — the tradeoff line a verb prints on stderr so `--json` stdout stays a
        // clean document. `EulerBudget`'s own `Display`, unchanged.
        Ok(SearchOutcome { rows, summary: Some(budget.to_string()) })
    }
}

/// [`run_sweep_euler`] with the execution strategy passed explicitly instead of read from the env —
/// the determinism gate's lever. Both [`SweepExec`] variants return byte-identical reports.
///
/// An ADAPTER over the optimizer seam. It spells `optimize::optimize`'s three steps out (preflight,
/// then `accepts`, then the search) instead of calling it, for ONE reason: it must return the TYPED
/// [`EulerBudget`], and `SearchOutcome` carries only the rendered line. The preflight is the
/// evaluator's CONSTRUCTOR — `StoreEvaluator::new` calls `require_overridable_params` — so the order
/// is the same one `optimize` enforces, and a base profile whose `strategy.params` is not a table is
/// refused before the first depth rather than after the whole budget.
pub fn run_sweep_euler_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    cfg: &EulerConfig,
    exec: SweepExec,
) -> Result<(SweepReport, EulerBudget), HarnessError> {
    let eval = StoreEvaluator::new(base, store, objective, label, exec)?;
    let search = EulerSearch::new(*cfg);
    search.accepts(base)?;
    let (rows, budget) = search.search_with_budget(base, &eval)?;
    // The summary is dropped here on purpose: this entry point returns the budget TYPED, and
    // `crates/vike-backtest/src/backtest_cli.rs` prints it through the same `Display`.
    let report = report_from_outcome(SearchOutcome { rows, summary: None }, eval.rank_by()).report;
    Ok((report, budget))
}

#[cfg(test)]
mod tests {
    use super::*;
    // `RankMetric` and the concrete `DataFusionHist` are used ONLY by the store-backed tests below
    // (the pure axis/budget tests need neither), so both are gated behind `datafusion-store`; the
    // `HistStore` trait those tests call `append_bars` through is already in scope via `super::*`.
    #[cfg(feature = "datafusion-store")]
    use crate::harness::sweep::{RankBy, RankMetric};
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
            // The refusal names the METHOD an operator would reach for, read off `GridSearch`'s
            // own `name()` — never a flag spelling this assertion would then be pinning.
            HarnessError::Validation(m) => assert!(m.contains(GridSearch.name()), "{m}"),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn non_sweep_profile_is_rejected() {
        let base = base_with_sweep("");
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
                    row: SweepRow {
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
    /// `run_sweep_euler_exec` reports `strategy.params`. Both are true of the OLD implementation
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
        use super::super::tpe::{TpeConfig, run_tpe};

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
