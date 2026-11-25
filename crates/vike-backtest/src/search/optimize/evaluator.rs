//! The two shipped point evaluators: one re-scans the store per point, one scores bars it holds.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use vike_analytics::TradingRecord;
use vike_data::HistoryStore;

use super::progress::{SearchProgressTracker, observed};
use super::{
    OverfitCapturedReturns, SearchCandidate, SearchEvaluation, SearchEvaluator, SearchProgressSink,
    SearchTradeFloor,
};
use crate::run::is_cancelled;
use crate::run::profile::ProfileBacktest;
use crate::run::tick_replay::tapes::TickTapes;
use crate::search::objective::SearchObjective;
use crate::search::paramscan::grid::{evaluation_scored_point, evaluation_scored_point_over_bars};
use crate::search::paramscan::threads::install_bounded;
use crate::search::paramscan::{
    OverfitReturnBuckets, ParamscanExecution, ParamscanRow, RankBy, RankMetric,
    profile_with_overrides,
};

#[cfg(doc)]
use super::{Search, overfit_stats, report_from_outcome};

/// The objective an evaluator scores with: the caller's, borrowed, or a [`RankMetric`]'s own,
/// constructed here for the classic path. A [`SearchObjective`] is a `Box<dyn Fn>` and is not `Clone`, so
/// the two cases cannot be collapsed into a `Cow`.
enum ObjectiveSource<'a> {
    Borrowed(&'a SearchObjective),
    Owned(SearchObjective),
}

impl ObjectiveSource<'_> {
    fn get(&self) -> &SearchObjective {
        match self {
            ObjectiveSource::Borrowed(o) => o,
            ObjectiveSource::Owned(o) => o,
        }
    }
}

/// Store-driven — what every method does today: each point re-scans the store through
/// [`super::run_backtest`], save a TICK point, which borrows the tape this search prepared once (`tapes`).
///
/// `exec` is a CONSTRUCTOR argument, which is the whole reason [`ParamscanExecution`] does not appear on
/// [`Search`]: it is a knob TPE cannot obey (there is no `run_tpe_exec`) and euler honours only
/// at batch granularity. Here it applies uniformly and TPE's serialization falls out of batch WIDTH
/// instead. The `_execution` twins in `sweep`/`euler` exist SOLELY so the determinism gates never mutate
/// process env — that lever survives one level down, so
/// `sweep`'s `parallel_and_sequential_sweeps_are_byte_identical` and euler's
/// `euler_parallel_and_sequential_are_byte_identical` keep working by constructing this type twice.
pub struct SearchEvaluatorStore<'a> {
    base: &'a ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    objective: ObjectiveSource<'a>,
    rank_by: RankBy,
    exec: ParamscanExecution,
    /// The statistical-significance floor, DISARMED by default — so an evaluator built by any
    /// existing call site behaves exactly as it did before [`SearchTradeFloor`] existed.
    floor: SearchTradeFloor,
    /// The progress observer, absent by default. `Option` rather than a no-op sink so a search with
    /// progress off costs no counter, no clock read and no virtual call per point.
    progress: Option<SearchProgressTracker>,
    /// How many buckets to keep per trial, DISARMED by default — so an evaluator built by any
    /// existing call site allocates nothing and behaves exactly as it did before
    /// [`OverfitReturnBuckets`] existed.
    buckets: OverfitReturnBuckets,
    /// The retained vectors, keyed by the [`SearchCandidate`] itself (`Eq + Hash`). See
    /// [`OverfitCapturedReturns`] for why this is a side channel rather than a field on [`ParamscanRow`].
    ///
    /// ⚠ **A `Mutex` inside the rayon fan-out, and it is not on the hot path in the sense that
    /// matters.** One lock per POINT, taken after a whole backtest has run — against
    /// `SearchProgressTracker`, which already locks per point for a much cheaper piece of work. Under
    /// [`OverfitReturnBuckets::DISARMED`] it is never taken at all, because `capture` answered `None`.
    captured: Mutex<HashMap<SearchCandidate, Vec<f64>>>,
    /// The stop flag, absent by default — so an evaluator built by any existing call site never
    /// reads it and behaves exactly as it did before [`SearchEvaluatorStore::with_cancel`] existed.
    cancel: Option<&'a AtomicBool>,
    /// Every point's WHOLE result, keyed by candidate — `None` (keep nothing) unless
    /// [`SearchEvaluatorStore::with_results`] asked.
    results: Option<Mutex<HashMap<SearchCandidate, TradingRecord>>>,
    /// This search's prepared tick tapes (`crate::run::tick_replay::tapes`): a tick tape is read, merged and
    /// seeded once and every point borrows it, so a search holds one tape where each worker held its own. Held
    /// for the evaluator's life, which is the request's; a bar search never fills it.
    tapes: TickTapes,
}

/// The `error` a point carries when an evaluator's `with_cancel` flag
/// ([`SearchEvaluatorStore::with_cancel`], [`SearchEvaluatorBars::with_cancel`]) was set before it ran.
const CANCELLED_POINT: &str = "cancelled: the search was stopped before this point ran";

/// The failed row a point gets instead of a backtest once `is_cancelled` (the harness's one
/// spelling of the check, `crate::run`'s) answered `true` — both evaluators run that check
/// first in `evaluate_one`.
///
/// ⚠ Not observed: a cancelled point was never searched, so it is not a progress tick, and the
/// floor has nothing to judge on a row with no report.
fn cancelled_point(candidate: SearchCandidate) -> SearchEvaluation {
    SearchEvaluation {
        row: ParamscanRow {
            overrides: candidate,
            report: None,
            error: Some(CANCELLED_POINT.to_string()),
            score: None,
        },
        score: f64::NAN,
    }
}

impl<'a> SearchEvaluatorStore<'a> {
    /// Objective path.
    pub fn new(
        base: &'a ProfileBacktest,
        store: Arc<dyn HistoryStore + Send + Sync>,
        objective: &'a SearchObjective,
        label: impl Into<String>,
        exec: ParamscanExecution,
    ) -> Self {
        SearchEvaluatorStore {
            base,
            store,
            objective: ObjectiveSource::Borrowed(objective),
            rank_by: RankBy::Objective(label.into()),
            exec,
            floor: SearchTradeFloor::DISARMED,
            progress: None,
            buckets: OverfitReturnBuckets::DISARMED,
            captured: Mutex::new(HashMap::new()),
            cancel: None,
            results: None,
            tapes: TickTapes::default(),
        }
    }

    /// Classic path — [`SearchEvaluator::rank_by`] answers `RankBy::Metric(m)` and the objective is
    /// `m.objective()`. Exists ONLY so [`crate::search::paramscan::run_paramscan_execution`] can delegate
    /// without changing its output (see [`report_from_outcome`]'s Metric arm).
    pub fn classic(
        base: &'a ProfileBacktest,
        store: Arc<dyn HistoryStore + Send + Sync>,
        rank_by: RankMetric,
        exec: ParamscanExecution,
    ) -> Self {
        SearchEvaluatorStore {
            base,
            store,
            objective: ObjectiveSource::Owned(rank_by.objective()),
            rank_by: RankBy::Metric(rank_by),
            exec,
            floor: SearchTradeFloor::DISARMED,
            progress: None,
            buckets: OverfitReturnBuckets::DISARMED,
            captured: Mutex::new(HashMap::new()),
            cancel: None,
            results: None,
            tapes: TickTapes::default(),
        }
    }

    /// Arm the statistical-significance floor. **A CONSUMING builder, not a constructor argument**,
    /// and that is a compatibility decision rather than a style one: a seventh parameter on
    /// [`SearchEvaluatorStore::new`] and an equivalent on [`SearchEvaluatorStore::classic`] would have to be
    /// spelled at every existing call site — the engine binary, the wire arm,
    /// `crate::search::euler`'s and `crate::search::tpe`'s adapters — each of which would then pass
    /// a value meaning "unchanged". A builder that defaults to [`SearchTradeFloor::DISARMED`] makes
    /// "unchanged" the thing nobody has to write, so an unarmed run stays byte-identical BY
    /// CONSTRUCTION rather than by four correct arguments.
    pub fn with_min_trades(mut self, floor: SearchTradeFloor) -> Self {
        self.floor = floor;
        self
    }

    /// Arm progress reporting against `sink`, with `total` from [`Search::budget_hint`].
    ///
    /// ⚠ **The clock starts HERE**, not at the first point, so the first event's elapsed time
    /// includes whatever the caller does between arming and searching — which for the engine's
    /// lane is opening the store. That is deliberate: the operator's question is "how long has this
    /// been going", and the store open is time they have been waiting.
    ///
    /// ⚠ Pass `total: None` rather than a guess. [`Search::budget_hint`] carries the argument for
    /// why a wrong total is worse than no total.
    pub fn with_progress(mut self, sink: Box<dyn SearchProgressSink>, total: Option<u64>) -> Self {
        self.progress = Some(SearchProgressTracker::new(sink, total));
        self
    }

    /// **Arm the per-trial return capture** — the opt-in that makes [`overfit_stats`] computable at
    /// all. A consuming builder for exactly the reason [`SearchEvaluatorStore::with_min_trades`] is one:
    /// "unchanged" is then the value nobody has to write at any of the existing call sites (the
    /// engine binary, the wire arm, euler's and TPE's adapters), so an unarmed search is
    /// byte-identical BY CONSTRUCTION rather than by four correct arguments.
    ///
    /// ⚠ **The retention is `buckets * 8` bytes per EVALUATED point and it is not freed until the
    /// evaluator is dropped**, which is the honest cost of the feature and the whole reason it is
    /// off by default. At [`OverfitReturnBuckets::DEFAULT_BUCKETS`] that is 4 KB a trial — 2 MB over a
    /// 500-point grid, against ~80 MB of decimated curves, pinned by `super::paramscan`'s
    /// `bucketed_returns_cost_a_fortieth_of_a_curve`. It does NOT multiply by
    /// `super::paramscan::paramscan_threads`: a worker holds one vector for the moment between `capture`
    /// and the map insert, where the DATA SLICE that cap exists for is held for a whole backtest.
    pub fn with_return_buckets(mut self, buckets: OverfitReturnBuckets) -> Self {
        self.buckets = buckets;
        self
    }

    /// **Arm the stop flag**: once `flag` reads `true`, every point not yet started is answered
    /// with a failed row (`error` = `CANCELLED_POINT`, score `NaN`) instead of a backtest, so the
    /// search runs out of work in microseconds whatever its method. The backtesthub's paramscan
    /// arm sets it when the client's socket closes (`vike_backtesthub::backtesthub_server`'s `PeerWatch`).
    ///
    /// ⚠ **A point already running is not interrupted** — the flag is read once, before the
    /// backtest; a running one finishes and its row is kept. The REPORT a cancelled search returns
    /// is therefore a mix of real and cancelled rows and means nothing: the caller that set the flag
    /// must discard it, as the daemon does by answering `Response::Error` instead.
    ///
    /// The cost on a search that never sets it is one relaxed atomic load per point; a consuming
    /// builder for the reason [`SearchEvaluatorStore::with_min_trades`] gives.
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// **Keep every point's whole result** — its curve, trades and counters, which a row's report
    /// does not carry — for a caller that answers with them (the Studio's sweep over its wire,
    /// `vike_backtesthub::studio_run::run_paramscan_slice`). A consuming builder for
    /// [`SearchEvaluatorStore::with_min_trades`]'s reason. ⚠ It holds every curve until
    /// [`SearchEvaluatorStore::take_results`], the memory a caller that keeps them all pays anyway.
    pub fn with_results(mut self) -> Self {
        self.results = Some(Mutex::default());
        self
    }

    /// The results [`SearchEvaluatorStore::with_results`] kept, keyed by candidate, handed over: a second
    /// call answers empty. A point that failed, or was cancelled, has none.
    pub fn take_results(&self) -> HashMap<SearchCandidate, TradingRecord> {
        self.results.as_ref().map_or_else(HashMap::new, |kept| {
            std::mem::take(&mut *kept.lock().unwrap_or_else(|e| e.into_inner()))
        })
    }

    /// The vectors this evaluator retained, for [`overfit_stats`] to join against the RANKED rows.
    ///
    /// ⚠ Read AFTER the search, from the caller that still owns the evaluator — the
    /// `crate::search::trials::TrialRecorder` that wraps it borrows `&self`, so this is a second
    /// shared borrow and never a conflict. Empty for every search that never called
    /// [`SearchEvaluatorStore::with_return_buckets`].
    ///
    /// ⚠ **It CLONES rather than taking**, which briefly doubles the retention (4 MB at the
    /// defaults over a 500-point grid) and is the deliberate trade: a `mem::take` would halve that
    /// and make a second call answer EMPTY, turning an idempotent read into a consuming one that
    /// silently reports "no matrix" to whoever asks twice. A transient few megabytes at the end of
    /// a search that already held the whole data slice is not the cost worth optimising.
    pub fn captured_returns(&self) -> OverfitCapturedReturns {
        let map = self.captured.lock().expect("captured returns").clone();
        OverfitCapturedReturns { buckets: self.buckets.buckets(), by_candidate: map }
    }

    /// One candidate -> its scored row, through the shared `paramscan::grid::evaluation_scored_point` fold.
    /// The build (`profile_with_overrides`) is infallible by type; a failed BACKTEST is a row.
    fn evaluate_one(&self, candidate: SearchCandidate) -> SearchEvaluation {
        // ⚠ BEFORE the profile clone: a cancelled point costs one atomic load and nothing else.
        if is_cancelled(self.cancel) {
            return cancelled_point(candidate);
        }
        // ⚠ The key is taken from the candidate BEFORE `evaluation_scored_point` moves it — and only
        // when a capture is armed, so an unarmed search clones nothing extra.
        let key = (self.buckets.is_armed() || self.results.is_some()).then(|| candidate.clone());
        let (profile, overrides) = profile_with_overrides(self.base, candidate);
        let (row, score, returns, result) = evaluation_scored_point(
            self.base,
            &self.store,
            &self.tapes,
            self.objective.get(),
            &profile,
            overrides,
            self.buckets,
        );
        if let (Some(key), Some(returns)) = (key.clone(), returns) {
            // A poisoned lock would mean another worker panicked mid-insert; the STATISTICS are
            // additive, so losing a column is better than taking the search down —
            // `crate::search::trial_ledger`'s "persisting is additive" posture, applied to a number rather
            // than to a file.
            if let Ok(mut map) = self.captured.lock() {
                map.insert(key, returns);
            }
        }
        if let (Some(key), Some(result), Some(kept)) = (key, result, self.results.as_ref()) {
            kept.lock().unwrap_or_else(|e| e.into_inner()).insert(key, result);
        }
        observed(SearchEvaluation { row, score }, self.floor, self.progress.as_ref())
    }
}

/// Both evaluators' `evaluate`: `one` over `batch` under the pool rule ([`SearchEvaluator`]'s doc) — a
/// NEW bounded pool per call is only worth building when there is a neighbourhood to spread it
/// over, so width 1 stays on the calling thread.
pub(super) fn evaluate_batch(
    exec: ParamscanExecution,
    batch: Vec<SearchCandidate>,
    one: impl Fn(SearchCandidate) -> SearchEvaluation + Sync,
) -> Vec<SearchEvaluation> {
    if exec == ParamscanExecution::Parallel && batch.len() > 1 {
        install_bounded(|| batch.into_par_iter().map(&one).collect())
    } else {
        batch.into_iter().map(one).collect()
    }
}

impl SearchEvaluator for SearchEvaluatorStore<'_> {
    fn evaluate(&self, batch: Vec<SearchCandidate>) -> Vec<SearchEvaluation> {
        evaluate_batch(self.exec, batch, |c| self.evaluate_one(c))
    }

    fn rank_by(&self) -> RankBy {
        self.rank_by.clone()
    }
}

/// Bars the CALLER already holds. The reason [`Search::search`] takes an evaluator at all: an
/// optimizer fixed at `(&ProfileBacktest, Arc<dyn HistoryStore>) -> ParamscanReport` can never host
/// [`crate::walkforward::runner`]'s per-window search, whose slice is an INDEX range
/// `crate::walkforward::walk_forward_strategy` cannot express as a store range.
///
/// `paramscan::grid::evaluation_scored_point_over_bars` takes `bars` BY VALUE per call, so this clones the
/// views per candidate (`vike_simulator::BarSeriesShared`): a count bumped, no bar copied.
pub struct SearchEvaluatorBars<'a> {
    base: &'a ProfileBacktest,
    store: Arc<dyn HistoryStore + Send + Sync>,
    bars: Vec<(String, vike_simulator::BarSeriesShared)>,
    /// The funding schedule that came with `bars` ([`SearchEvaluatorBars::with_funding`]), empty by default.
    funding: crate::run::FundingSchedule,
    /// The conversion pairs that came with `bars` ([`SearchEvaluatorBars::with_conversions`]), empty by default.
    conversions: Vec<vike_simulator::ConversionSeries>,
    objective: &'a SearchObjective,
    rank_by: RankBy,
    exec: ParamscanExecution,
    /// The statistical-significance floor, DISARMED by default — so an evaluator built by any
    /// existing call site behaves exactly as it did before [`SearchTradeFloor`] existed.
    floor: SearchTradeFloor,
    /// The progress observer, absent by default. `Option` rather than a no-op sink so a search with
    /// progress off costs no counter, no clock read and no virtual call per point.
    progress: Option<SearchProgressTracker>,
    /// The stop flag, absent by default — the twin of [`SearchEvaluatorStore`]'s, see
    /// [`SearchEvaluatorBars::with_cancel`].
    cancel: Option<&'a AtomicBool>,
}

impl<'a> SearchEvaluatorBars<'a> {
    pub fn new(
        base: &'a ProfileBacktest,
        store: Arc<dyn HistoryStore + Send + Sync>,
        bars: Vec<(String, impl Into<vike_simulator::BarSeriesShared>)>,
        objective: &'a SearchObjective,
        label: impl Into<String>,
        exec: ParamscanExecution,
    ) -> Self {
        SearchEvaluatorBars {
            base,
            store,
            bars: bars.into_iter().map(|(symbol, series)| (symbol, series.into())).collect(),
            funding: Vec::new(),
            conversions: Vec::new(),
            objective,
            rank_by: RankBy::Objective(label.into()),
            exec,
            floor: SearchTradeFloor::DISARMED,
            progress: None,
            cancel: None,
        }
    }

    /// Arm the statistical-significance floor — the twin of [`SearchEvaluatorStore::with_min_trades`],
    /// which carries the argument for why both are consuming builders rather than constructor
    /// arguments.
    ///
    /// ⚠ A floor on a WALK-FORWARD window is a stricter thing to ask for than a floor on a whole
    /// run, and the count is not rescaled here: a window is a fraction of the range, so a
    /// `--min-trades` that a full-range search clears easily can disqualify every candidate in
    /// every window and leave the walk with nothing rankable. That is the honest reading of the
    /// knob — thin evidence is thin evidence — but it is a foot-gun worth naming where the seam is,
    /// because the operator typed one number and the window sees a shorter range.
    pub fn with_min_trades(mut self, floor: SearchTradeFloor) -> Self {
        self.floor = floor;
        self
    }

    /// Hand every candidate the funding schedule its `bars` were loaded with (the second half of
    /// `crate::run::load_profile_bars`'s answer), so a held perpetual is charged as in a plain
    /// run of the same profile. Without it the bars' `Bar.funding` charges at the close, the rule of a
    /// symbol with no schedule (`vike_simulator::StrategyEngineConfig::funding_schedule`).
    pub fn with_funding(mut self, funding: crate::run::FundingSchedule) -> Self {
        self.funding = funding;
        self
    }

    /// Hand every candidate the conversion pairs its `bars` were loaded with (`crate::run::data_plan::load_conversions`
    /// and `book_conversions` over the range they were cut from), so a symbol quoted in another currency
    /// than the ledger is booked as in a plain run of the same profile (decision 0243). Without them a
    /// profile that names a pair is refused, never run at par.
    pub fn with_conversions(mut self, conversions: Vec<vike_simulator::ConversionSeries>) -> Self {
        self.conversions = conversions;
        self
    }

    /// Arm progress reporting — the twin of [`SearchEvaluatorStore::with_progress`].
    ///
    /// ⚠ One tracker per EVALUATOR, so a walk-forward run that builds an evaluator per window
    /// counts per window and its `completed` restarts at `1` on each. There is deliberately no
    /// cross-window total here: `crate::walkforward::runner` owns the window list and is the only
    /// place that could add one up.
    pub fn with_progress(mut self, sink: Box<dyn SearchProgressSink>, total: Option<u64>) -> Self {
        self.progress = Some(SearchProgressTracker::new(sink, total));
        self
    }

    /// Arm the stop flag — the twin of [`SearchEvaluatorStore::with_cancel`], which carries the contract:
    /// once `flag` reads `true` every point not yet started is a failed row (`error` =
    /// `CANCELLED_POINT`, score `NaN`), a running one finishes, and the report of a cancelled
    /// search is the caller's to discard. One relaxed atomic load per point when armed.
    pub fn with_cancel(mut self, flag: &'a AtomicBool) -> Self {
        self.cancel = Some(flag);
        self
    }

    /// See [`SearchEvaluatorStore`]'s `evaluate_one` — same cancel check, same infallible build, same
    /// fold; only the bar source differs.
    fn evaluate_one(&self, candidate: SearchCandidate) -> SearchEvaluation {
        if is_cancelled(self.cancel) {
            return cancelled_point(candidate);
        }
        let (profile, overrides) = profile_with_overrides(self.base, candidate);
        let (row, score) = evaluation_scored_point_over_bars(
            self.base,
            &self.store,
            self.bars.clone(),
            &self.funding,
            &self.conversions,
            self.objective,
            &profile,
            overrides,
            // Per point: this evaluator keeps every point's row, so a failed read stays visible
            // on that row.
            &Arc::default(),
        );
        observed(SearchEvaluation { row, score }, self.floor, self.progress.as_ref())
    }
}

impl SearchEvaluator for SearchEvaluatorBars<'_> {
    fn evaluate(&self, batch: Vec<SearchCandidate>) -> Vec<SearchEvaluation> {
        evaluate_batch(self.exec, batch, |c| self.evaluate_one(c))
    }

    fn rank_by(&self) -> RankBy {
        self.rank_by.clone()
    }
}
