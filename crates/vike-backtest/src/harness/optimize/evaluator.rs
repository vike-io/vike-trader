//! The two shipped point evaluators: one re-scans the store per point, one scores bars it holds.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use vike_data::HistStore;

use super::progress::{ProgressTracker, observed};
use super::{
    Candidate, CapturedReturns, Evaluated, PointEvaluator, ProgressSink, TradeFloor,
    require_overridable_params,
};
use crate::harness::run::is_cancelled;
use crate::harness::sweep::grid::{eval_scored_point, eval_scored_point_over_bars};
use crate::harness::sweep::threads::install_bounded;
use crate::harness::sweep::{
    ParamscanExec, ParamscanRow, RankBy, RankMetric, ReturnBuckets, profile_with_overrides,
};
use crate::harness::{BacktestProfile, HarnessError};
use crate::search::objective::Objective;
use crate::search::trials::candidate_key;

#[cfg(doc)]
use super::{Optimizer, overfit_stats, report_from_outcome};

/// The objective an evaluator scores with: the caller's, borrowed, or a [`RankMetric`]'s own,
/// constructed here for the classic path. An [`Objective`] is a `Box<dyn Fn>` and is not `Clone`, so
/// the two cases cannot be collapsed into a `Cow`.
enum ObjectiveSource<'a> {
    Borrowed(&'a Objective),
    Owned(Objective),
}

impl ObjectiveSource<'_> {
    fn get(&self) -> &Objective {
        match self {
            ObjectiveSource::Borrowed(o) => o,
            ObjectiveSource::Owned(o) => o,
        }
    }
}

/// Store-driven — what every method does today: each point re-scans the store through
/// [`super::run_backtest`].
///
/// `exec` is a CONSTRUCTOR argument, which is the whole reason [`ParamscanExec`] does not appear on
/// [`Optimizer`]: it is a knob TPE cannot obey (there is no `run_tpe_exec`) and euler honours only
/// at batch granularity. Here it applies uniformly and TPE's serialization falls out of batch WIDTH
/// instead. The `_exec` twins in `sweep`/`euler` exist SOLELY so the determinism gates never mutate
/// process env — that lever survives one level down, so
/// `sweep`'s `parallel_and_sequential_sweeps_are_byte_identical` and euler's
/// `euler_parallel_and_sequential_are_byte_identical` keep working by constructing this type twice.
pub struct StoreEvaluator<'a> {
    base: &'a BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: ObjectiveSource<'a>,
    rank_by: RankBy,
    exec: ParamscanExec,
    /// The statistical-significance floor, DISARMED by default — so an evaluator built by any
    /// existing call site behaves exactly as it did before [`TradeFloor`] existed.
    floor: TradeFloor,
    /// The progress observer, absent by default. `Option` rather than a no-op sink so a search with
    /// progress off costs no counter, no clock read and no virtual call per point.
    progress: Option<ProgressTracker>,
    /// How many buckets to keep per trial, DISARMED by default — so an evaluator built by any
    /// existing call site allocates nothing and behaves exactly as it did before
    /// [`ReturnBuckets`] existed.
    buckets: ReturnBuckets,
    /// The retained vectors, keyed by `crate::search::trials::candidate_key`. See
    /// [`CapturedReturns`] for why this is a side channel rather than a field on [`ParamscanRow`].
    ///
    /// ⚠ **A `Mutex` inside the rayon fan-out, and it is not on the hot path in the sense that
    /// matters.** One lock per POINT, taken after a whole backtest has run — against
    /// `ProgressTracker`, which already locks per point for a much cheaper piece of work. Under
    /// [`ReturnBuckets::DISARMED`] it is never taken at all, because `capture` answered `None`.
    captured: Mutex<HashMap<String, Vec<f64>>>,
    /// The stop flag, absent by default — so an evaluator built by any existing call site never
    /// reads it and behaves exactly as it did before [`StoreEvaluator::with_cancel`] existed.
    cancel: Option<&'a AtomicBool>,
}

/// The `error` a point carries when an evaluator's `with_cancel` flag
/// ([`StoreEvaluator::with_cancel`], [`BarsEvaluator::with_cancel`]) was set before it ran.
const CANCELLED_POINT: &str = "cancelled: the search was stopped before this point ran";

/// The failed row a point gets instead of a backtest once `is_cancelled` (the harness's one
/// spelling of the check, `crate::harness::run`'s) answered `true` — both evaluators run that check
/// first in `evaluate_one`.
///
/// ⚠ Not observed: a cancelled point was never searched, so it is not a progress tick, and the
/// floor has nothing to judge on a row with no report.
fn cancelled_point(candidate: Candidate) -> Evaluated {
    Evaluated {
        row: ParamscanRow {
            overrides: candidate,
            report: None,
            error: Some(CANCELLED_POINT.to_string()),
            score: None,
        },
        score: f64::NAN,
    }
}

impl<'a> StoreEvaluator<'a> {
    /// Objective path. ⚠ Fallible: calls [`require_overridable_params`], which is what makes
    /// [`PointEvaluator::evaluate`] infallible BY TYPE rather than by promise.
    pub fn new(
        base: &'a BacktestProfile,
        store: Arc<dyn HistStore + Send + Sync>,
        objective: &'a Objective,
        label: impl Into<String>,
        exec: ParamscanExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(StoreEvaluator {
            base,
            store,
            objective: ObjectiveSource::Borrowed(objective),
            rank_by: RankBy::Objective(label.into()),
            exec,
            floor: TradeFloor::DISARMED,
            progress: None,
            buckets: ReturnBuckets::DISARMED,
            captured: Mutex::new(HashMap::new()),
            cancel: None,
        })
    }

    /// Classic path — [`PointEvaluator::rank_by`] answers `RankBy::Metric(m)` and the objective is
    /// `m.objective()`. Exists ONLY so [`super::sweep::run_paramscan_exec`] can delegate without
    /// changing its output (see [`report_from_outcome`]'s Metric arm).
    pub fn classic(
        base: &'a BacktestProfile,
        store: Arc<dyn HistStore + Send + Sync>,
        rank_by: RankMetric,
        exec: ParamscanExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(StoreEvaluator {
            base,
            store,
            objective: ObjectiveSource::Owned(rank_by.objective()),
            rank_by: RankBy::Metric(rank_by),
            exec,
            floor: TradeFloor::DISARMED,
            progress: None,
            buckets: ReturnBuckets::DISARMED,
            captured: Mutex::new(HashMap::new()),
            cancel: None,
        })
    }

    /// Arm the statistical-significance floor. **A CONSUMING builder, not a constructor argument**,
    /// and that is a compatibility decision rather than a style one: a seventh parameter on
    /// [`StoreEvaluator::new`] and an equivalent on [`StoreEvaluator::classic`] would have to be
    /// spelled at every existing call site — the engine binary, the wire arm,
    /// `crate::search::euler`'s and `crate::search::tpe`'s adapters — each of which would then pass
    /// a value meaning "unchanged". A builder that defaults to [`TradeFloor::DISARMED`] makes
    /// "unchanged" the thing nobody has to write, so an unarmed run stays byte-identical BY
    /// CONSTRUCTION rather than by four correct arguments.
    pub fn with_min_trades(mut self, floor: TradeFloor) -> Self {
        self.floor = floor;
        self
    }

    /// Arm progress reporting against `sink`, with `total` from [`Optimizer::budget_hint`].
    ///
    /// ⚠ **The clock starts HERE**, not at the first point, so the first event's elapsed time
    /// includes whatever the caller does between arming and searching — which for the engine's
    /// lane is opening the store. That is deliberate: the operator's question is "how long has this
    /// been going", and the store open is time they have been waiting.
    ///
    /// ⚠ Pass `total: None` rather than a guess. [`Optimizer::budget_hint`] carries the argument for
    /// why a wrong total is worse than no total.
    pub fn with_progress(mut self, sink: Box<dyn ProgressSink>, total: Option<u64>) -> Self {
        self.progress = Some(ProgressTracker::new(sink, total));
        self
    }

    /// **Arm the per-trial return capture** — the opt-in that makes [`overfit_stats`] computable at
    /// all. A consuming builder for exactly the reason [`StoreEvaluator::with_min_trades`] is one:
    /// "unchanged" is then the value nobody has to write at any of the existing call sites (the
    /// engine binary, the wire arm, euler's and TPE's adapters), so an unarmed search is
    /// byte-identical BY CONSTRUCTION rather than by four correct arguments.
    ///
    /// ⚠ **The retention is `buckets * 8` bytes per EVALUATED point and it is not freed until the
    /// evaluator is dropped**, which is the honest cost of the feature and the whole reason it is
    /// off by default. At [`ReturnBuckets::DEFAULT_BUCKETS`] that is 4 KB a trial — 2 MB over a
    /// 500-point grid, against ~80 MB of decimated curves, pinned by `super::sweep`'s
    /// `bucketed_returns_cost_a_fortieth_of_a_curve`. It does NOT multiply by
    /// `super::sweep::sweep_threads`: a worker holds one vector for the moment between `capture`
    /// and the map insert, where the DATA SLICE that cap exists for is held for a whole backtest.
    pub fn with_return_buckets(mut self, buckets: ReturnBuckets) -> Self {
        self.buckets = buckets;
        self
    }

    /// **Arm the stop flag**: once `flag` reads `true`, every point not yet started is answered
    /// with a failed row (`error` = `CANCELLED_POINT`, score `NaN`) instead of a backtest, so the
    /// search runs out of work in microseconds whatever its method. The compute daemon's paramscan
    /// arm sets it when the client's socket closes (`crate::compute_server`'s `PeerWatch`).
    ///
    /// ⚠ **A point already running is not interrupted** — the flag is read once, before the
    /// backtest; a running one finishes and its row is kept. The REPORT a cancelled search returns
    /// is therefore a mix of real and cancelled rows and means nothing: the caller that set the flag
    /// must discard it, as the daemon does by answering `Response::Error` instead.
    ///
    /// The cost on a search that never sets it is one relaxed atomic load per point; a consuming
    /// builder for the reason [`StoreEvaluator::with_min_trades`] gives.
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// The vectors this evaluator retained, for [`overfit_stats`] to join against the RANKED rows.
    ///
    /// ⚠ Read AFTER the search, from the caller that still owns the evaluator — the
    /// `crate::search::trials::TrialRecorder` that wraps it borrows `&self`, so this is a second
    /// shared borrow and never a conflict. Empty for every search that never called
    /// [`StoreEvaluator::with_return_buckets`].
    ///
    /// ⚠ **It CLONES rather than taking**, which briefly doubles the retention (4 MB at the
    /// defaults over a 500-point grid) and is the deliberate trade: a `mem::take` would halve that
    /// and make a second call answer EMPTY, turning an idempotent read into a consuming one that
    /// silently reports "no matrix" to whoever asks twice. A transient few megabytes at the end of
    /// a search that already held the whole data slice is not the cost worth optimising.
    pub fn captured_returns(&self) -> CapturedReturns {
        let map = self.captured.lock().expect("captured returns").clone();
        CapturedReturns { buckets: self.buckets.buckets(), by_candidate: map }
    }

    /// One candidate -> its scored row, through the shared `sweep::grid::eval_scored_point` fold.
    /// The `Err` arm is unreachable by construction (the constructor refused the only profile whose
    /// build can fail) and records a failed ROW rather than panicking, because a panic in a rayon
    /// worker takes the whole batch with it. The candidate CLONE is what buys that arm an honest
    /// `overrides` list; it is a `Vec<(String, toml::Value)>` beside the whole `BacktestProfile`
    /// clone `profile_with_overrides` performs on the happy path, so it is not measurable.
    fn evaluate_one(&self, candidate: Candidate) -> Evaluated {
        // ⚠ BEFORE the profile clone: a cancelled point costs one atomic load and nothing else.
        if is_cancelled(self.cancel) {
            return cancelled_point(candidate);
        }
        match profile_with_overrides(self.base, candidate.clone()) {
            Ok((profile, overrides)) => {
                // ⚠ The key is taken from the candidate BEFORE `eval_scored_point` moves
                // `overrides` — they are the same value (`Candidate` is an alias for that vector),
                // and keying off the moved-in copy afterwards would mean cloning it again.
                let key = self.buckets.is_armed().then(|| candidate_key(&candidate));
                let (row, score, returns) = eval_scored_point(
                    self.base,
                    &self.store,
                    self.objective.get(),
                    &profile,
                    overrides,
                    self.buckets,
                );
                if let (Some(key), Some(returns)) = (key, returns) {
                    // A poisoned lock would mean another worker panicked mid-insert; the
                    // STATISTICS are additive, so losing a column is better than taking the search
                    // down — `crate::trial_ledger`'s "persisting is additive" posture, applied to a
                    // number rather than to a file.
                    if let Ok(mut map) = self.captured.lock() {
                        map.insert(key, returns);
                    }
                }
                observed(Evaluated { row, score }, self.floor, self.progress.as_ref())
            }
            Err(e) => observed(
                Evaluated { row: failed_row(candidate, &e), score: f64::NAN },
                self.floor,
                self.progress.as_ref(),
            ),
        }
    }
}

impl PointEvaluator for StoreEvaluator<'_> {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
        // The pool rule (trait doc): a NEW bounded pool per call is only worth building when there
        // is a neighbourhood to spread it over, so width 1 stays on the calling thread.
        if self.exec == ParamscanExec::Parallel && batch.len() > 1 {
            install_bounded(|| batch.into_par_iter().map(|c| self.evaluate_one(c)).collect())
        } else {
            batch.into_iter().map(|c| self.evaluate_one(c)).collect()
        }
    }

    fn rank_by(&self) -> RankBy {
        self.rank_by.clone()
    }
}

/// Bars the CALLER already holds. The reason [`Optimizer::search`] takes an evaluator at all: an
/// optimizer fixed at `(&BacktestProfile, Arc<dyn HistStore>) -> ParamscanReport` can never host
/// [`crate::walkforward::runner`]'s per-window search, whose slice is an INDEX range
/// `crate::walkforward::walk_forward_strategy` cannot express as a store range.
///
/// ⚠ `sweep::grid::eval_scored_point_over_bars` takes `bars` BY VALUE per call, so this clones per
/// candidate — which is what [`crate::walkforward::runner`]'s own `map_bounded` closure already
/// does with its `train_series.clone()`. Not a new cost; stated so it is not discovered.
pub struct BarsEvaluator<'a> {
    base: &'a BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<vike_model::Bar>)>,
    objective: &'a Objective,
    rank_by: RankBy,
    exec: ParamscanExec,
    /// The statistical-significance floor, DISARMED by default — so an evaluator built by any
    /// existing call site behaves exactly as it did before [`TradeFloor`] existed.
    floor: TradeFloor,
    /// The progress observer, absent by default. `Option` rather than a no-op sink so a search with
    /// progress off costs no counter, no clock read and no virtual call per point.
    progress: Option<ProgressTracker>,
    /// The stop flag, absent by default — the twin of [`StoreEvaluator`]'s, see
    /// [`BarsEvaluator::with_cancel`].
    cancel: Option<&'a AtomicBool>,
}

impl<'a> BarsEvaluator<'a> {
    /// ⚠ Fallible for the same reason [`StoreEvaluator::new`] is: the preflight is what makes
    /// [`PointEvaluator::evaluate`] infallible by type.
    pub fn new(
        base: &'a BacktestProfile,
        store: Arc<dyn HistStore + Send + Sync>,
        bars: Vec<(String, Vec<vike_model::Bar>)>,
        objective: &'a Objective,
        label: impl Into<String>,
        exec: ParamscanExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(BarsEvaluator {
            base,
            store,
            bars,
            objective,
            rank_by: RankBy::Objective(label.into()),
            exec,
            floor: TradeFloor::DISARMED,
            progress: None,
            cancel: None,
        })
    }

    /// Arm the statistical-significance floor — the twin of [`StoreEvaluator::with_min_trades`],
    /// which carries the argument for why both are consuming builders rather than constructor
    /// arguments.
    ///
    /// ⚠ A floor on a WALK-FORWARD window is a stricter thing to ask for than a floor on a whole
    /// run, and the count is not rescaled here: a window is a fraction of the range, so a
    /// `--min-trades` that a full-range search clears easily can disqualify every candidate in
    /// every window and leave the walk with nothing rankable. That is the honest reading of the
    /// knob — thin evidence is thin evidence — but it is a foot-gun worth naming where the seam is,
    /// because the operator typed one number and the window sees a shorter range.
    pub fn with_min_trades(mut self, floor: TradeFloor) -> Self {
        self.floor = floor;
        self
    }

    /// Arm progress reporting — the twin of [`StoreEvaluator::with_progress`].
    ///
    /// ⚠ One tracker per EVALUATOR, so a walk-forward run that builds an evaluator per window
    /// counts per window and its `completed` restarts at `1` on each. There is deliberately no
    /// cross-window total here: `crate::walkforward::runner` owns the window list and is the only
    /// place that could add one up.
    pub fn with_progress(mut self, sink: Box<dyn ProgressSink>, total: Option<u64>) -> Self {
        self.progress = Some(ProgressTracker::new(sink, total));
        self
    }

    /// Arm the stop flag — the twin of [`StoreEvaluator::with_cancel`], which carries the contract:
    /// once `flag` reads `true` every point not yet started is a failed row (`error` =
    /// `CANCELLED_POINT`, score `NaN`), a running one finishes, and the report of a cancelled
    /// search is the caller's to discard. One relaxed atomic load per point when armed.
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// See [`StoreEvaluator`]'s `evaluate_one` — same cancel check, same fold, same
    /// unreachable-by-construction `Err` arm; only the bar source differs.
    fn evaluate_one(&self, candidate: Candidate) -> Evaluated {
        if is_cancelled(self.cancel) {
            return cancelled_point(candidate);
        }
        match profile_with_overrides(self.base, candidate.clone()) {
            Ok((profile, overrides)) => {
                let (row, score) = eval_scored_point_over_bars(
                    self.base,
                    &self.store,
                    self.bars.clone(),
                    self.objective,
                    &profile,
                    overrides,
                );
                observed(Evaluated { row, score }, self.floor, self.progress.as_ref())
            }
            Err(e) => observed(
                Evaluated { row: failed_row(candidate, &e), score: f64::NAN },
                self.floor,
                self.progress.as_ref(),
            ),
        }
    }
}

impl PointEvaluator for BarsEvaluator<'_> {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
        if self.exec == ParamscanExec::Parallel && batch.len() > 1 {
            install_bounded(|| batch.into_par_iter().map(|c| self.evaluate_one(c)).collect())
        } else {
            batch.into_iter().map(|c| self.evaluate_one(c)).collect()
        }
    }

    fn rank_by(&self) -> RankBy {
        self.rank_by.clone()
    }
}

/// A row for a point that could not even be BUILT — the defence-in-depth shape both evaluators owe
/// the trait: report-XOR-error with no score, exactly what `sweep`'s `row_from_outcome` produces for
/// a failed backtest, so `--json` grows no third row kind.
fn failed_row(overrides: Candidate, e: &HarnessError) -> ParamscanRow {
    ParamscanRow { overrides, report: None, error: Some(e.to_string()), score: None }
}
