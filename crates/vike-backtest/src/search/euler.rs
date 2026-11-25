//! Euler (successive-halving) parameter search over a `[sweep]` profile — the store-backed wrapper
//! around the pure search core (`crate::search`).
//!
//! ADDITIVE AND OPT-IN. The default sweep path (`run_paramscan`/`run_paramscan_with`, `--optimizer grid`) is
//! untouched: the same cartesian grid, the same comparator, byte-identical output. `--optimizer euler`
//! instead evaluates the coarse grid ONCE and then refines locally around the best point, halving
//! the per-axis step up to [`crate::search::SearchEulerConfig::max_depth`] times — far fewer backtests
//! for the resolution it reaches (see `crate::search`'s module doc for the cost formula and the
//! local-refinement caveat). The reported [`SearchEulerBudget`] compares against the grid matching the
//! depth ACTUALLY reached, never the requested `max_depth`, so an early-stopped run cannot
//! advertise a saving it did not buy.
//!
//! Scoring reuses the existing objective seam verbatim ([`crate::search::objective::SearchObjective`]): a
//! `RankMetric`'s built-in objective or the composite `multi_metric`, same "higher is better,
//! `NaN` = unrankable" convention. Nothing about scoring is reimplemented here.
//!
//! REQUIREMENT: every `[sweep]` axis must be numeric AND type-homogeneous (an all-integer or an
//! all-float TOML array). A string/bool axis has no notion of "halfway between two values"; a MIXED
//! `[1, 2.5]` axis would silently change the TOML type the strategy receives versus the grid path
//! (which clones each raw `ParamValue`), so a strategy reading the param via `as_integer()` would
//! behave differently under `--optimizer euler` than under the grid. Both are a
//! [`HarnessError::Validation`] naming the grid method as the way to search that space.
//!
//! PARALLELISM (additive, result-neutral): a refinement DEPTH is inherently sequential — each one
//! centres on the previous depth's winner — but the points WITHIN one depth's neighbourhood (and
//! within the coarse grid) are independent backtests. [`SearchEuler`] therefore drives
//! [`crate::search::euler_search_batched`] and hands each already-deduped batch to ONE
//! `optimize::SearchEvaluator::evaluate` call, which is where the fan-out lives now — the BOUNDED
//! sweep pool (`paramscan::threads::install_bounded`, NOT rayon's global pool; each concurrent point
//! materializes its own copy of the data slice, so the worker count is a peak-RSS multiplier, capped
//! by `VIKE_SWEEP_THREADS`, see `sweep`'s module doc), collecting in INPUT order. ⚠ The CADENCE is
//! unchanged by that move — one pool per `evaluate` call, and a call is still exactly one depth —
//! which is what keeps the trace, the rows, the budget and the ranking identical to the
//! one-at-a-time path (pinned by `search`'s
//! `batched_search_matches_the_point_wise_search_bit_for_bit` and by
//! `euler_parallel_and_sequential_are_byte_identical` below). [`ParamscanExecution::Sequential`] (or
//! `VIKE_PARAMSCAN_SEQUENTIAL=1`) forces the old loop, now as an evaluator CONSTRUCTOR argument.

use std::sync::Arc;

use vike_data::HistoryStore;

use crate::search::objective::SearchObjective;
use crate::search::{
    SearchAxis, SearchEulerConfig, equivalent_grid_evaluations, euler_search_batched,
};

use crate::HarnessError;
use crate::run::profile::ProfileBacktest;
use crate::run::refusals::ProfileRefusal;
use crate::search::optimize::{
    Search, SearchCandidate, SearchEvaluator, SearchEvaluatorStore, SearchOutcome,
    report_from_outcome,
};
use crate::search::paramscan::{
    ParamscanExecution, ParamscanReport, ParamscanRow, SearchGrid, numeric_sweep_axes,
    paramscan_execution,
};
use vike_model::params::ParamValue;

/// How a `[sweep]` axis's values are rendered back into a [`ParamValue`]: an all-integer coarse
/// axis stays integral (both in the profile override AND in the refinement, which then never
/// proposes a fractional value), anything else is a float axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxisKind {
    Integer,
    Float,
}

impl AxisKind {
    fn to_param(self, x: f64) -> ParamValue {
        match self {
            AxisKind::Integer => ParamValue::Int(x.round() as i64),
            AxisKind::Float => ParamValue::Float(x),
        }
    }
}

/// Hard ceiling on the number of `[sweep]` axes an Euler search accepts. Each refinement depth
/// evaluates a `3^d` neighbourhood, so the per-depth cost grows FASTER in `d` than the equal-
/// resolution grid does for low-cardinality axes; past this width the "cheaper than the grid"
/// premise stops holding and the caller is better served by the grid method. (Clamping at the box
/// corner plus the dedup keep the realised count well under in practice, but that is empirical —
/// this guard is what makes the bound structural.)
pub(crate) const MAX_AXES: usize = 8;

/// Read the profile's `[sweep]` table as numeric search axes (axes sorted by key — the same
/// deterministic axis order `expand_paramscan` uses). The shared reader
/// (`paramscan::numeric_sweep_axes`) owns the array/non-empty/numeric/type-homogeneity validation;
/// this adapter adds the euler-only width guard and renders each axis as a
/// [`SearchAxis`] + [`AxisKind`] pair.
fn numeric_axes(base: &ProfileBacktest) -> Result<Vec<(SearchAxis, AxisKind)>, HarnessError> {
    // Width guard FIRST — before per-axis validation, preserving the original error precedence
    // (a too-wide sweep reports the width problem even when an axis is also non-numeric).
    if let Some(sweep) = base.paramscan.as_ref().filter(|_| base.is_paramscan())
        && sweep.len() > MAX_AXES
    {
        return Err(HarnessError::from(ProfileRefusal::ParamscanEulerTooManyAxes {
            axes: sweep.len(),
            // The NAME is read off the method so the refusal cannot drift from the roster; the FLAG
            // is spelled beside it because `--optimizer` is what an operator types. Both come from
            // `SearchGrid.name()`, so the method and the flag's value cannot disagree.
            grid: SearchGrid.name().to_string(),
        }));
    }

    // The lane name this reader interpolates into its own narrowing errors is `SearchEuler`'s own
    // `name()` — one fact, not a second literal; the arity stays as it is because five tests call
    // this function directly.
    Ok(numeric_sweep_axes(base, SearchEuler::NAME)?
        .into_iter()
        .map(|axis| {
            let kind = if axis.integral { AxisKind::Integer } else { AxisKind::Float };
            (axis, kind)
        })
        .collect())
}

/// Render one coordinate vector as an `optimize::SearchCandidate`: each axis's coordinate back in its own
/// kind (`Int` or `Float`). This is the ONLY place euler's private `Vec<f64>` coordinates become
/// the shared candidate currency — which is exactly what lets `crate::search`'s raw-f64 `seen`
/// dedup key stay private and unchanged.
fn overrides_for(axes: &[(SearchAxis, AxisKind)], point: &[f64]) -> SearchCandidate {
    axes.iter().zip(point).map(|((axis, kind), &x)| (axis.key.clone(), kind.to_param(x))).collect()
}

/// [`overrides_for`] plus the profile it produces — TEST-ONLY since the optimizer seam landed.
///
/// The build half moved to `optimize::SearchEvaluatorStore`, so nothing on the search path builds a
/// profile any more; this spelling survives because
/// `integer_axis_overrides_stay_integers` pins the whole chain (an integral axis renders as a TOML
/// integer AND lands in `strategy.params` as one), which is the property a renderer alone cannot
/// show.
#[cfg(test)]
fn profile_for(
    base: &ProfileBacktest,
    axes: &[(SearchAxis, AxisKind)],
    point: &[f64],
) -> (ProfileBacktest, SearchCandidate) {
    super::paramscan::profile_with_overrides(base, overrides_for(axes, point))
}

/// The budget an Euler run would cost against the grid that reaches the same resolution — the
/// tradeoff, reported so a caller can see what the refinement bought.
///
/// `equivalent_grid` is the exact point count of the cartesian grid matching the resolution the run
/// ACTUALLY reached — derived from `depth_reached`, NOT from `max_depth`, so a search that stopped
/// early cannot advertise a saving against a finer grid it never approached. `evaluated` is what the
/// search really spent (only known after the run).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct SearchEulerBudget {
    pub evaluated: usize,
    pub equivalent_grid: usize,
    pub depth_reached: u32,
    pub max_depth: u32,
}

impl std::fmt::Display for SearchEulerBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "euler: {} evaluations at depth {}/{} (grid at the depth reached would be {})",
            self.evaluated, self.depth_reached, self.max_depth, self.equivalent_grid
        )
    }
}

/// Run an Euler successive-halving search over `base`'s `[sweep]` axes, scoring every evaluated
/// point with `objective` and returning the SAME [`ParamscanReport`] shape the grid path returns (rows
/// ranked best-first by score, failures last) plus the [`SearchEulerBudget`] the run cost.
///
/// Only the points the search actually evaluated appear as rows — that IS the saving. A failed
/// backtest scores `NaN` (unrankable), so a broken corner of the space can never become the
/// refinement's centre.
pub fn run_paramscan_euler(
    base: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    objective: &SearchObjective,
    label: impl Into<String>,
    config: &SearchEulerConfig,
) -> Result<(ParamscanReport, SearchEulerBudget), HarnessError> {
    run_paramscan_euler_execution(base, store, objective, label, config, paramscan_execution())
}

/// The Euler successive-halving refinement, as a [`Search`].
///
/// Its `search` is the old `run_paramscan_euler_execution` body with two things REMOVED and nothing added:
///
/// * the rayon fan-out — concurrency belongs to `optimize::SearchEvaluator` now, and the CADENCE is
///   unchanged (one `evaluate` call per refinement DEPTH, which is one bounded pool per depth,
///   exactly as before);
/// * the build-failure LATCH — a point build cannot fail (`strategy.params` is a
///   `vike_model::params::Params` table, and a non-table one never loads), which is why `evaluate`
///   is infallible.
///
/// What stays is the LOOP, and that is the whole argument for the seam:
/// `crates/vike-backtest/src/search.rs` does not change by one line — no state promotion, no control
/// inversion — so its 22 pure tests gate unmoved code and depth accounting is untouched
/// (`equivalent_grid_evaluations` still receives `trace.depth_reached`, never `config.max_depth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchEuler {
    pub config: SearchEulerConfig,
}

impl SearchEuler {
    /// This method's operator-facing name, as a const because [`numeric_axes`] interpolates it into
    /// the shared reader's narrowing errors and that function keeps its arity (five tests call it
    /// directly). One fact, two readers — never two literals.
    const NAME: &'static str = "euler";

    /// A method value over `config`. `SearchEulerConfig` is `Copy`, so this holds it rather than borrowing:
    /// the trait's `&self` is what lets ONE value serve many runs.
    pub fn new(config: SearchEulerConfig) -> Self {
        SearchEuler { config }
    }

    /// The search, keeping the TYPED [`SearchEulerBudget`] rather than only its rendered line.
    /// [`Search::search`] renders it into `SearchOutcome`'s `summary`; [`run_paramscan_euler_execution`]
    /// takes it whole, because that adapter's `(ParamscanReport, SearchEulerBudget)` return is a public
    /// signature this seam does not remove.
    fn search_with_budget(
        &self,
        base: &ProfileBacktest,
        evaluation: &dyn SearchEvaluator,
    ) -> Result<(Vec<ParamscanRow>, SearchEulerBudget), HarnessError> {
        let axes = numeric_axes(base)?;
        let search_axes: Vec<SearchAxis> = axes.iter().map(|(a, _)| a.clone()).collect();

        // Row-per-evaluation, appended in EVALUATION order (= batch input order, which the
        // evaluator's order-preserving collect guarantees) inside the batch closure. UNSORTED:
        // `optimize::report_from_outcome` is the one place rows get ordered.
        let mut rows: Vec<ParamscanRow> = Vec::new();

        let trace = euler_search_batched(&search_axes, &self.config, |batch| {
            // ONE `evaluate` call per depth — the already-deduped neighbourhood, handed over whole.
            let candidates: Vec<SearchCandidate> =
                batch.iter().map(|p| overrides_for(&axes, p.as_slice())).collect();
            let mut scores = Vec::with_capacity(candidates.len());
            for e in evaluation.evaluate(candidates) {
                scores.push(e.score);
                rows.push(e.row);
            }
            scores
        });

        let budget = SearchEulerBudget {
            evaluated: trace.n_evaluated(),
            // The grid matching the resolution ACTUALLY attained — `depth_reached`, not `max_depth`.
            equivalent_grid: equivalent_grid_evaluations(
                &search_axes,
                &SearchEulerConfig::with_depth(trace.depth_reached),
            ),
            depth_reached: trace.depth_reached,
            max_depth: self.config.max_depth,
        };

        Ok((rows, budget))
    }
}

impl Search for SearchEuler {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    /// The euler-only narrowing: numeric, type-homogeneous, at most `MAX_AXES` axes — with the
    /// WIDTH guard first, so a too-wide sweep reports its width even when an axis is also
    /// non-numeric. Pure and profile-only, so a space failure is now caught BEFORE a store opens.
    fn accepts(&self, base: &ProfileBacktest) -> Result<(), HarnessError> {
        numeric_axes(base).map(|_| ())
    }

    /// **`None`, and euler is the method that makes [`Search::budget_hint`] an `Option`.**
    ///
    /// The cost model in `crate::search`'s module doc is an UPPER BOUND — `N + D*(3^d - 1)` at most,
    /// "usually far less: every candidate already seen is skipped" — and the search additionally
    /// stops the moment a depth produces no unseen candidate. Its own worked example is 25 coarse
    /// points plus at most 24 refinements; the measured run is routinely a fraction of that. So a
    /// total published here would be a number the run beats, a denominator that makes `12/49` read
    /// as a quarter done when the search is about to finish, and an ETA counting down to a time
    /// that never arrives.
    ///
    /// The coarse grid size is tempting and is worse: it is a LOWER bound, so it would be exceeded
    /// and the progress line would print `31/25`.
    ///
    /// What euler reports instead is the truth AFTERWARDS — [`SearchEulerBudget`], which carries the
    /// evaluations actually spent and the depth actually reached, and reaches stderr through
    /// `SearchOutcome`'s already-rendered `summary`.
    fn budget_hint(&self, _base: &ProfileBacktest) -> Option<u64> {
        None
    }

    fn search(
        &self,
        base: &ProfileBacktest,
        evaluation: &dyn SearchEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let (rows, budget) = self.search_with_budget(base, evaluation)?;
        // ALREADY RENDERED — the tradeoff line a verb prints on stderr so `--json` stdout stays a
        // clean document. `SearchEulerBudget`'s own `Display`, unchanged.
        Ok(SearchOutcome { rows, summary: Some(budget.to_string()) })
    }
}

/// [`run_paramscan_euler`] with the execution strategy passed explicitly instead of the root-installed default —
/// the determinism gate's lever. Both [`ParamscanExecution`] variants return byte-identical reports.
///
/// An ADAPTER over the optimizer seam. It spells `optimize::optimize`'s two steps out (`accepts`,
/// then the search) instead of calling it, for ONE reason: it must return the TYPED
/// [`SearchEulerBudget`], and `SearchOutcome` carries only the rendered line. The order is the same one
/// `optimize` enforces.
pub fn run_paramscan_euler_execution(
    base: &ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    objective: &SearchObjective,
    label: impl Into<String>,
    config: &SearchEulerConfig,
    exec: ParamscanExecution,
) -> Result<(ParamscanReport, SearchEulerBudget), HarnessError> {
    let evaluation = SearchEvaluatorStore::new(base, store, objective, label, exec);
    let search = SearchEuler::new(*config);
    search.accepts(base)?;
    let (rows, budget) = search.search_with_budget(base, &evaluation)?;
    // The summary is dropped here on purpose: this entry point returns the budget TYPED, and
    // `crates/vike-backtesthub/src/backtest_cli.rs` prints it through the same `Display`.
    let report =
        report_from_outcome(SearchOutcome { rows, summary: None }, evaluation.rank_by()).report;
    Ok((report, budget))
}

#[path = "euler_tests.rs"]
#[cfg(test)]
mod euler_tests;
