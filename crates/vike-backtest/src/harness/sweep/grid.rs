//! The exhaustive grid as an optimizer, the per-point evaluation fold, and the run_paramscan adapters.

use std::sync::Arc;

use vike_analytics::report::BacktestReport;
use vike_data::HistStore;

use super::{
    ParamscanExec, ParamscanReport, ParamscanRow, RankMetric, ReturnBuckets,
    expand_paramscan_overrides,
};
use crate::harness::optimize::{
    Optimized, Optimizer, PointEvaluator, SearchOutcome, StoreEvaluator, optimize,
};
use crate::harness::{BacktestProfile, HarnessError, run_backtest};
use crate::search::objective::Objective;

/// Run ONE point's single-run profile through [`run_backtest`] into its (unscored) report row —
/// the shared row builder EVERY optimizer lane (grid, euler, TPE) folds through. Pure per-point
/// work over shared-by-reference inputs (`&BacktestProfile`, `&Arc<dyn HistStore>`) — nothing here
/// touches state another point can see, which is what makes the points safely parallelizable.
/// A failure is recorded as this row's `error`, never propagated. `score` stays `None`: the
/// classic [`run_paramscan`] never stamps one, and the search lanes stamp theirs on top via
/// [`eval_scored_point`].
///
/// The second half of the answer is the trial's bucketed return vector under `buckets` — `None`
/// under [`ReturnBuckets::DISARMED`], which is every caller that did not opt in.
fn point_row(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
    buckets: ReturnBuckets,
) -> (ParamscanRow, Option<Vec<f64>>) {
    row_from_outcome(base, profile, overrides, run_backtest(profile, store.clone()), buckets)
}

/// [`point_row`] over bars the CALLER already holds — the walk-forward window's per-candidate unit.
///
/// Same row, same report, same failure rule; only the bar source differs. See
/// [`super::run::run_backtest_over_bars`] for why a window cannot use the store-driven spelling:
/// it would reload the profile's whole range once per candidate, and the window's slice is an
/// INDEX range that `crate::walkforward::walk_forward_strategy` cannot express as a store range.
///
/// ⚠ **This path retains NO return vector, and that is a scoping decision rather than an
/// omission.** A walk-forward WINDOW is a fraction of the range, so its buckets cover a different
/// span from the full-range trials a search ranks — pooling the two into one matrix would compare
/// in-sample against out-of-sample block means across columns that do not describe the same
/// interval, which is precisely the comparison `pbo_cscv` exists to make honestly. Walk-forward
/// already answers the out-of-sample question its own way (`overfit::overfit_verdict`'s
/// `wf_consistency` argument), and keeping this signature unchanged is also what leaves
/// `crate::walkforward::runner`'s own window closure, which calls
/// [`eval_scored_point_over_bars`] directly, untouched.
fn point_row_over_bars(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<vike_model::Bar>)>,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> ParamscanRow {
    let outcome = super::run::run_backtest_over_bars(profile, store, bars);
    row_from_outcome(base, profile, overrides, outcome, ReturnBuckets::DISARMED).0
}

/// The ONE row builder both spellings above fold through — the report composition, the
/// annualization and the failure-is-a-row-not-an-error rule live here once. Split out when the
/// bars-in twin landed, precisely so the two sources could never disagree about what a sweep row
/// MEANS; before that this body was `point_row`'s whole match.
///
/// ⚠ **This is the ONE place a trial's equity curve still exists**, which is why the bucketing
/// happens here rather than anywhere a caller might find more convenient: the very next thing this
/// function does is drop `r`, and everything above it sees only [`BacktestReport`]'s scalars. The
/// capture reads [`vike_model::runs::MAX_EQUITY_SAMPLES`]-worth of nothing — see
/// `ReturnBuckets::capture`, which touches `buckets + 1` points of the curve and allocates
/// `buckets` floats.
fn row_from_outcome(
    base: &BacktestProfile,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
    outcome: Result<vike_analytics::BacktestResult, HarnessError>,
    buckets: ReturnBuckets,
) -> (ParamscanRow, Option<Vec<f64>>) {
    match outcome {
        Ok(r) => {
            let returns = buckets.capture(&r.equity_curve);
            (
                ParamscanRow {
                    overrides,
                    report: Some(BacktestReport::from_result(
                        base.name.clone(),
                        &r,
                        super::report::periods_per_year(profile),
                    )),
                    error: None,
                    score: None,
                },
                returns,
            )
        }
        // A failed point has no curve to bucket, and its row is already unrankable by
        // `score_row`'s own rule — so it contributes no column and nothing is fabricated for it.
        Err(e) => (
            ParamscanRow { overrides, report: None, error: Some(e.to_string()), score: None },
            None,
        ),
    }
}

/// Evaluate ONE searched point (an already-built single-run `profile` plus its `overrides`) into
/// its SCORED report row and the raw steering score — **the ONE evaluate-a-point block the search
/// lanes (euler, TPE) share** instead of each hand-rolling the `run_backtest` → report →
/// objective fold (they were line-for-line twins). A successful backtest stamps
/// `score: Some(objective(&report))`; a failed one keeps the row's `score` at `None` (the
/// report/JSON shape is unchanged) and returns `NaN` as the steering score — a failed point is
/// unrankable and must never become a refinement centre or steer the model.
///
/// The third element is the trial's bucketed return vector under `buckets` — `None` for a failed
/// point and for every caller that passes [`ReturnBuckets::DISARMED`]. It is returned rather than
/// stamped on the row for the reason [`CapturedReturns`] argues.
pub(crate) fn eval_scored_point(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
    buckets: ReturnBuckets,
) -> (ParamscanRow, f64, Option<Vec<f64>>) {
    let (row, returns) = point_row(base, store, profile, overrides, buckets);
    let (row, score) = score_row(row, objective);
    (row, score, returns)
}

/// [`eval_scored_point`] over bars the CALLER already holds — what a walk-forward window scores its
/// candidates with. Identical scoring, identical failed-point rule; only the bar source differs.
pub(crate) fn eval_scored_point_over_bars(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<vike_model::Bar>)>,
    objective: &Objective,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> (ParamscanRow, f64) {
    score_row(point_row_over_bars(base, store, bars, profile, overrides), objective)
}

/// Stamp a row's steering score — the ONE place the "a failed point is unrankable" rule is
/// written, shared by both `eval_scored_point*` spellings. A successful backtest carries
/// `score: Some(objective(&report))`; a failure keeps `score: None` (the report/JSON shape is
/// unchanged) and steers with `NaN`, which `cmp_scores_desc` sorts LAST — so a failed point can
/// never become a refinement centre, steer a model, or win a walk-forward window.
fn score_row(mut row: ParamscanRow, objective: &Objective) -> (ParamscanRow, f64) {
    match row.report.as_ref() {
        Some(report) => {
            let score = objective(report);
            row.score = Some(score);
            (row, score)
        }
        None => (row, f64::NAN),
    }
}

/// The exhaustive cartesian GRID, as an [`Optimizer`] — the permissive method, and the one that
/// decided every point before running one.
///
/// A one-turn loop is the honest description: `search` expands the whole `[sweep]` space with
/// [`expand_paramscan_overrides`] and submits it as ONE batch, which is exactly what the deleted
/// `run_rows` did. Concurrency is the evaluator's business (see `optimize::PointEvaluator`'s pool
/// rule), so the rayon fan-out and the `N_threads x data-slice` memory cap that used to live in
/// `run_rows` now live one level down and are shared with every other method.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridSearch;

impl Optimizer for GridSearch {
    fn name(&self) -> &'static str {
        "grid"
    }

    // `accepts` is deliberately NOT overridden: the grid is the permissive method — it accepts any
    // non-empty TOML array, string and bool axes included, and validates each one during EXPANSION
    // (`sweep_axis_array`). That is why `search` keeps a `Result`.

    /// The grid's total is EXACT and always has been: it is `|expand|`, the same number `search`
    /// is about to evaluate, so a progress line under this method can promise `n/N` without
    /// qualification.
    ///
    /// ⚠ It EXPANDS the space a second time, and that is cheap on purpose: `expand_paramscan_overrides`
    /// builds `Vec<(String, toml::Value)>` tuples and runs no backtest, against N backtests about
    /// to follow. The alternative — threading the expansion out of `search` so it could be counted
    /// once — would put a grid-shaped value on the [`Optimizer`] seam that no other method has.
    ///
    /// A space this method will REFUSE during expansion answers `None` rather than propagating: a
    /// budget hint is an observer's question, and the refusal belongs to `search`, which raises it a
    /// moment later with its own message.
    fn budget_hint(&self, base: &BacktestProfile) -> Option<u64> {
        expand_paramscan_overrides(base).ok().map(|points| points.len() as u64)
    }

    fn search(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<SearchOutcome, HarnessError> {
        let rows = eval
            .evaluate(expand_paramscan_overrides(base)?)
            .into_iter()
            .map(|e| e.row)
            .collect::<Vec<_>>();
        // No summary: the grid's cost is `|expand|`, derived from the profile, and it has never
        // reported one.
        Ok(SearchOutcome { rows, summary: None })
    }
}

/// Expand `base`'s `[sweep]` table and run every resulting point through [`run_backtest`],
/// ranking the successful rows by `rank_by` (best first) with failed rows sorted last.
///
/// The classic `--rank-by` path, output byte-identical. Its comparator LEFT the runtime path when
/// the optimizer seam landed — ranking is now [`RankMetric::objective`] through
/// `optimize::report_from_outcome`, whose Metric arm clears the stamped scores again so neither the
/// table nor `--json` can tell. For a custom objective use [`run_paramscan_with`].
pub fn run_paramscan(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    rank_by: RankMetric,
) -> Result<ParamscanReport, HarnessError> {
    run_paramscan_exec(base, store, rank_by, ParamscanExec::from_env())
}

/// [`run_paramscan`] with the execution strategy passed explicitly instead of read from the env — the
/// determinism gate's lever. Both [`ParamscanExec`] variants return byte-identical reports.
///
/// An ADAPTER over the optimizer seam since the seam landed: a classic [`StoreEvaluator`] (which
/// answers `RankBy::Metric` and scores with `rank_by.objective()`) driven by [`GridSearch`] through
/// `optimize::optimize`. Output is byte-identical — `optimize::report_from_outcome`'s Metric arm
/// clears every row's `score` after sorting, and `metric_objectives_rank_identically_to_cmp_reports`
/// (below) is the PROOF that the objective ordering equals the retired
/// `RankMetric::cmp_reports` one over ties, negatives and a NaN.
pub fn run_paramscan_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    rank_by: RankMetric,
    exec: ParamscanExec,
) -> Result<ParamscanReport, HarnessError> {
    let eval = StoreEvaluator::classic(base, store, rank_by, exec)?;
    let Optimized { report, .. } = optimize(&GridSearch, base, &eval)?;
    Ok(report)
}

/// **The ONE best-first ranking comparator over raw f64 scores** — every score-ranked sort in and
/// out of this crate (objective sweeps, TPE, euler, vike-studio-core's Sharpe rank) goes through
/// it rather than hand-rolling `partial_cmp(..).unwrap_or(Equal)`, the shape that lets a NaN
/// compare Equal to everything and keep whatever position it started in. Higher is better; a NaN
/// ("unrankable") score sorts LAST among successes — the objective-path twin of
/// `RankMetric::cmp_reports`'s NaN rule, so a degenerate point can never rank first here either.
pub fn cmp_scores_desc(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater, // a unrankable → a is worse → sorts last
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => b.partial_cmp(&a).expect("non-NaN f64s compare totally"),
    }
}

/// [`run_paramscan`], ranked by an arbitrary [`Objective`] instead of a [`RankMetric`]: every
/// successful row's report is scored (stamped into [`ParamscanRow::score`]) and rows sort by score
/// descending (higher is better), NaN scores after finite ones, failed rows last. `label` names
/// the objective in the report header/JSON (`rank_by`), e.g. `"multi"`.
pub fn run_paramscan_with(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
) -> Result<ParamscanReport, HarnessError> {
    run_paramscan_with_exec(base, store, objective, label, ParamscanExec::from_env())
}

/// [`run_paramscan_with`] with the execution strategy passed explicitly instead of read from the env.
/// Both [`ParamscanExec`] variants return byte-identical reports.
///
/// The objective-path twin of [`run_paramscan_exec`]'s adapter: an objective [`StoreEvaluator`] driven
/// by [`GridSearch`]. Scores are stamped by the SHARED `score_row` fold now rather than re-derived
/// here, which is the point — a row's score and the searcher's steering score are one number.
pub fn run_paramscan_with_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    exec: ParamscanExec,
) -> Result<ParamscanReport, HarnessError> {
    let eval = StoreEvaluator::new(base, store, objective, label, exec)?;
    let Optimized { report, .. } = optimize(&GridSearch, base, &eval)?;
    Ok(report)
}
