//! The `Optimizer` seam: ONE parameter-search METHOD over a profile's `[sweep]` space, plus the
//! [`PointEvaluator`] half a method drives its loop against.
//!
//! Read OUT of the three searchers that already exist ([`super::sweep`]'s cartesian grid,
//! [`super::euler`]'s successive halving, [`super::tpe`]'s ask/tell Bayesian search) rather than
//! designed ahead of them — `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` is the
//! signed-off design and carries the measurements. Measured, the three share exactly four things:
//! one evaluation unit (`sweep::eval_scored_point` -> `point_row` -> `row_from_outcome` ->
//! `score_row`), one ordering rule (`sweep::sort_scored_rows` over `sweep::cmp_scores_desc`), one
//! profile builder (`sweep::profile_with_overrides`) and one bounded executor
//! (`sweep::install_bounded`). They share NEITHER the loop, NOR the candidate type, NOR the budget,
//! NOR the report shape — three loop shapes, three budget KINDS, three accepted spaces. So **the
//! searcher is the part that differs and the evaluation is the part that does not**: this module
//! abstracts the evaluation and leaves each searcher its own loop.
//!
//! The three implementations are plain data and each lives in its own existing module
//! (`sweep::GridSearch`, `euler::EulerSearch`, `tpe::TpeSearch`) — deliberately, because that needs
//! ZERO visibility widening: `euler`'s `numeric_axes` and `tpe`'s `tpe_space_from_profile` are
//! private and stay private, since the impl that calls them is in the same file. A method's
//! knowledge stays in the method's file; this module holds only the seam.
//!
//! Gated with the rest of `harness/` behind `hist-replay`, and compiled by BOTH that trait-only
//! lane and the `datafusion-store` lane.

use std::sync::Arc;

use rayon::prelude::*;
use vike_data::HistStore;

use crate::objective::Objective;

use super::sweep::{
    PARAMS_NOT_A_TABLE, RankBy, RankMetric, SweepExec, SweepReport, SweepRow, eval_scored_point,
    eval_scored_point_over_bars, install_bounded, profile_with_overrides, sort_scored_rows,
};
use super::{BacktestProfile, HarnessError};

/// One proposed point, in the ONE currency all three searchers already converge on: the
/// `(param name, TOML value)` overrides `super::sweep::profile_with_overrides` takes.
///
/// An ALIAS, not a newtype, so a candidate, a [`SweepRow`]'s `overrides` field and that builder's
/// argument are literally the same value.
///
/// ⚠ NOT [`super::sweep::SweepPoint`], which also bundles a built [`BacktestProfile`] — that field
/// is why `expand_sweep` materializes N full profile clones before running one point, and the other
/// two searchers build theirs per point anyway. ⚠ NOT `Vec<f64>`: a numeric currency would exclude
/// the GRID from its own trait, since `sweep`'s `sweep_axis_array` accepts string and bool axes that
/// its `numeric_sweep_axes` refuses. The numeric methods keep their coordinates PRIVATE and render
/// at this boundary, exactly as `euler`'s `AxisKind::to_toml` and [`super::tpe::ParamDomain`]'s
/// `to_toml` already do — which is also what preserves euler's raw-bit-pattern dedup key.
pub type Candidate = Vec<(String, toml::Value)>;

/// One evaluated point: the row a report will print, and the raw steering score the searcher
/// refines on. Exactly the pair `sweep::eval_scored_point` already returns and
/// [`super::tpe::run_tpe`] already forks (row to `rows`, score to [`super::tpe::TpeOptimizer`]'s
/// `tell`) — naming it is the only change.
///
/// ⚠ A STRUCT rather than a tuple ON PURPOSE: an `elapsed`, a fidelity tag, or an evaluation-cost
/// field can be added without touching a single [`Optimizer`] signature. That is the extension
/// point for cost-aware acquisition, and it is why this is not `(SweepRow, f64)`.
#[derive(Debug, Clone)]
pub struct Evaluated {
    pub row: SweepRow,
    /// Higher is better; `NaN` means UNRANKABLE. See [`Optimizer`]'s score contract.
    pub score: f64,
}

/// What a searcher produced, before ranking.
#[derive(Debug)]
pub struct SearchOutcome {
    /// One row per EVALUATION, in EVALUATION order, UNSORTED. Not per PROPOSAL (euler's dedup means
    /// proposals exceed rows) and not per grid point (only the grid). Ranking is REPORT ASSEMBLY
    /// and happens once, above every method, in [`report_from_outcome`] — so an implementation
    /// cannot invent its own ordering, which is the defect euler's
    /// `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` was written to catch.
    ///
    /// Returning EVALUATION order also preserves the trajectory up to the assembler;
    /// [`super::tpe::run_tpe`] sorted before returning, so nothing downstream could see the search
    /// path at all.
    pub rows: Vec<SweepRow>,
    /// This method's own one-line cost summary, ALREADY RENDERED, for a verb to print on stderr so
    /// a `--json` stdout stays a clean document. `None` for a method with nothing to say.
    ///
    /// A `String`, because a string is what all three ALREADY are at the point of use:
    /// [`super::euler::EulerBudget`] reaches stderr through its own `Display`, TPE's line is
    /// hand-assembled in `crates/vike-backtest/src/backtest_cli.rs` from flags plus the best row's
    /// score, and the grid prints nothing. A typed `Diagnostics` would be euler-shaped and filled by
    /// no one else.
    pub summary: Option<String>,
}

/// The ranked answer. Replaces euler's `(SweepReport, EulerBudget)` tuple as the ONE return shape.
#[derive(Debug)]
pub struct Optimized {
    pub report: SweepReport,
    pub summary: Option<String>,
}

/// ONE parameter-search METHOD over a profile's `[sweep]` space. Grid, euler and TPE are its first
/// three implementations.
///
/// # The searcher owns its loop
///
/// [`Optimizer::search`] receives an evaluator and drives its own iteration to completion. That is
/// how all three are written TODAY — [`super::sweep::GridSearch`] (one turn),
/// `crate::search::euler_search_batched` (one turn per refinement depth),
/// [`super::tpe::TpeSearch`] (one turn per trial) — so this is a WIDENING of existing code rather
/// than a shape imposed on it. `crates/vike-backtest/src/search.rs` needs no change at all, which is
/// the sharpest available evidence that the seam is real.
///
/// # Object safety is load-bearing
///
/// Every method takes concrete types and no generic parameter, so a caller can hold
/// `Box<dyn Optimizer>` and a fourth method costs ONE match arm. This is why nothing here spells
/// `label: impl Into<String>` (all five existing entry points do, and it is not object-safe) and why
/// `sweep`'s `install_bounded` (an `impl FnOnce`) stays a free function implementations CALL.
///
/// `Send + Sync` so a boxed optimizer can be built on one thread and run on another; `&self` so ONE
/// value can serve every walk-forward window without interior mutability. Per-run state is
/// constructed inside `search` — which is exactly where [`super::tpe::TpeOptimizer`] and euler's
/// evaluated/seen/incumbent live today.
///
/// # The score contract — written down here for the first time
///
/// The steering score is a bare `f64`. **Higher is better. `NaN` means UNRANKABLE.**
///
/// Not `Option<f64>` and ⚠ **never `f64::NEG_INFINITY`** — `-inf` is finite-comparable, so it would
/// sort merely last-among-finite, silently changing tie behaviour and the `--json` shape. The
/// convention is written in FOUR places and was stated as a contract in none:
/// [`super::sweep::cmp_scores_desc`] (NaN after finite), `sweep`'s `score_row` (a failed backtest
/// steers NaN), `crate::search`'s `better` (NaN never displaces a finite incumbent, so refinement
/// cannot chase a broken corner), and `sweep`'s `ser_opt_score` (non-finite -> JSON `null`). An
/// implementation reaches all four FOR FREE by scoring through [`PointEvaluator`] and never calling
/// `super::run_backtest` itself.
///
/// A failed BACKTEST is a row, never an `Err`.
pub trait Optimizer: Send + Sync {
    /// This method's name as an operator spells it: `"grid"`, `"euler"`, `"tpe"`.
    ///
    /// Load-bearing, not decoration. `sweep`'s `numeric_sweep_axes(base, search)` already takes a
    /// lane name purely to interpolate into its narrowing errors, and those errors also NAME the
    /// method they send a refused profile to — [`super::sweep::GridSearch`]'s own `name()`, so the
    /// name, the operator-facing spelling and the string in the refusal are ONE fact rather than
    /// three literals. (They used to end `use --search grid`, naming the flag PR 2 then retired.)
    fn name(&self) -> &'static str;

    /// Can THIS method search THIS profile's space at all? Pure, profile-only, no store, no I/O.
    /// Called by [`optimize`] BEFORE a `HistStore` is opened.
    ///
    /// This is the one capability beyond bare dispatch the seam keeps, because the three genuinely
    /// accept DIFFERENT input spaces and a shared validator would flatten a deliberate precedence:
    /// euler's `numeric_axes` checks its axis-count ceiling FIRST, ahead of per-axis validation,
    /// with a comment saying so, so that a too-wide sweep reports its WIDTH even when an axis is
    /// also non-numeric.
    ///
    /// Default `Ok(())` — the permissive grid, which validates during expansion anyway. ⚠ `Ok(())`
    /// is not a promise the run will SUCCEED, only that the SPACE is acceptable; the grid's own
    /// per-axis check (`sweep`'s `sweep_axis_array`) still lives inside expansion, which is why
    /// [`Optimizer::search`] keeps a `Result`.
    fn accepts(&self, base: &BacktestProfile) -> Result<(), HarnessError> {
        let _ = base;
        Ok(())
    }

    /// Run the search to completion, driving `eval` as many times as this method's algorithm calls
    /// for.
    ///
    /// **The searcher submits WHOLE BATCHES, of whatever width it likes.** Batch width is an
    /// ALGORITHMIC property, never a machine one: the grid submits its entire expansion, euler one
    /// already-deduped refinement neighbourhood per depth, TPE one candidate because each proposal
    /// reads the whole history. Nothing here caps a batch — CONCURRENCY is the evaluator's business
    /// and is a different question.
    ///
    /// **The evaluator answers in INPUT ORDER, one [`Evaluated`] per candidate.** The same hard
    /// contract `crate::search::euler_search_batched` already `assert_eq!`s on its own closure, and
    /// the same property that makes the parallel and sequential paths byte-identical (rayon's
    /// `into_par_iter().map().collect()` reassembles by index, never by completion).
    ///
    /// `Err` is reserved for a condition [`Optimizer::accepts`] could not have seen — for the grid,
    /// a non-array or empty axis discovered during expansion. It means NO usable report.
    fn search(
        &self,
        base: &BacktestProfile,
        eval: &dyn PointEvaluator,
    ) -> Result<SearchOutcome, HarnessError>;
}

/// The seam a searcher drives its loop AGAINST — and the half that actually carries the
/// abstraction. It owns everything a searcher must not: the profile build, the store, the objective,
/// the bounded rayon pool, the bar source, and the report's rank label.
///
/// Two implementations ship with it, corresponding exactly to the matched pair `sweep` already has:
/// [`StoreEvaluator`] (re-scan the store per point — `sweep::eval_scored_point`) and
/// [`BarsEvaluator`] (bars the caller already holds — `sweep::eval_scored_point_over_bars`, which is
/// what a walk-forward window needs because its slice is an INDEX range
/// `crate::walkforward::walk_forward_strategy` cannot express as a store range).
///
/// `Sync` is required, not decorative: [`StoreEvaluator`] fans a batch out with
/// `into_par_iter().map(..)` over `&self`.
pub trait PointEvaluator: Sync {
    /// Evaluate `batch`, returning one [`Evaluated`] per candidate IN INPUT ORDER.
    ///
    /// # ⚠ Infallible by CONSTRUCTION, not by decree
    ///
    /// The only fallible step in the point-build path is `sweep::profile_with_overrides`, whose
    /// single error (`sweep::PARAMS_NOT_A_TABLE`) is a property of the BASE profile — so every candidate
    /// fails identically and there is nothing per-point to report. Both shipped evaluators refuse
    /// that profile in their CONSTRUCTOR ([`require_overridable_params`]), so an evaluator cannot
    /// exist over a profile whose build can fail. [`optimize`] calls the same preflight as its first
    /// act, so the refusal also happens before any store is opened.
    ///
    /// This is what deletes euler's build-failure latch and collapses THREE fault timings for one
    /// error into one. A defence-in-depth implementation that somehow meets a build failure anyway
    /// MUST record it as a failed row with a `NaN` score — never panic, because a panic in a rayon
    /// worker takes the whole batch.
    ///
    /// # ⚠ The pool rule — a contract clause, not an optimization
    ///
    /// An implementation enters `sweep`'s `install_bounded` **only when
    /// `exec == SweepExec::Parallel` AND `batch.len() > 1`**. `install_bounded` constructs a NEW
    /// `rayon::ThreadPoolBuilder` on every call, and euler's own comment justified per-batch
    /// construction by WIDTH ("a batch is one refinement DEPTH, so the spawn cost is amortized over
    /// a whole neighbourhood"). At width 1 there is no neighbourhood: without this clause TPE, which
    /// enters rayon ZERO times today, would build [`super::tpe::TpeConfig`]'s `DEFAULT_TRIALS` = 64
    /// four-thread pools to run 64 single backtests. With it, TPE's "no fan-out" is EXACT rather
    /// than a documented no-op parameter.
    ///
    /// ⚠ An evaluator is the ONLY place this workspace enters rayon inside a search. A caller that
    /// is ALREADY inside a bounded pool must construct its evaluator with `SweepExec::Sequential`,
    /// or the `min(4, cores)` memory bound composes MULTIPLICATIVELY. Nothing nests today —
    /// [`super::walkforward`]'s window loop is sequential — but hosting a searcher per window is the
    /// obvious next step and this is where that bites.
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated>;

    /// How the report should label its ranking. Lives HERE, beside the [`Objective`] it names,
    /// because `crate::objective::Objective` is a `Box<dyn Fn>` with no name of its own — the
    /// separate label exists only for that reason, and pairing them means they cannot disagree.
    fn rank_by(&self) -> RankBy;
}

/// The single preflight, and the reason [`PointEvaluator::evaluate`] can be infallible. Checks the
/// one thing `sweep::profile_with_overrides` fails on, with the IDENTICAL message (one constant,
/// `sweep::PARAMS_NOT_A_TABLE`, not two literals) so no operator sees a new string.
pub fn require_overridable_params(base: &BacktestProfile) -> Result<(), HarnessError> {
    match base.strategy.params.as_table() {
        Some(_) => Ok(()),
        None => Err(HarnessError::Validation(PARAMS_NOT_A_TABLE.into())),
    }
}

/// The ONE door a dispatching caller goes through. Nothing else calls [`Optimizer::search`]
/// directly: the search's infallibility rests on this preflight having happened.
pub fn optimize(
    opt: &dyn Optimizer,
    base: &BacktestProfile,
    eval: &dyn PointEvaluator,
) -> Result<Optimized, HarnessError> {
    // ⚠ HERE, not in a doc comment on `accepts`: this is what makes `PointEvaluator::evaluate`
    // infallible, so it must run before any method's loop starts — not after its budget is spent.
    require_overridable_params(base)?;
    opt.accepts(base)?;
    let outcome = opt.search(base, eval)?;
    Ok(report_from_outcome(outcome, eval.rank_by()))
}

/// Turn a method-agnostic [`SearchOutcome`] into the [`SweepReport`] every caller already prints —
/// THE one place rows get ordered, and the one place the classic/objective output split is decided.
///
/// Sorting is `sweep`'s `sort_scored_rows` over [`super::sweep::cmp_scores_desc`]: score descending,
/// `NaN` after finite, `score: None` (a failed point) last, STABLE so ties keep evaluation order.
///
/// ⚠ The [`RankBy::Metric`] arm additionally CLEARS every row's `score` AFTER sorting. That is not
/// tidiness — it is what lets [`super::sweep::run_sweep_exec`] become an adapter over this seam
/// while its table and its `--json` stay byte-identical (`SweepRow`'s `score` is
/// `skip_serializing_if = "Option::is_none"`, and the Display table grows a `score=` column only
/// when one is stamped). The ORDERING is provably unchanged: [`RankMetric::objective`] folds
/// direction in, and `sweep`'s `metric_objectives_rank_identically_to_cmp_reports` pins that it
/// produces the same index order as the retired `RankMetric::cmp_reports` over ties, negatives and a
/// NaN, for all four metrics. Failed rows land last under both. KEEP that test as the PROOF of the
/// retirement rather than as the guarantee it was.
pub fn report_from_outcome(outcome: SearchOutcome, rank_by: RankBy) -> Optimized {
    let SearchOutcome { mut rows, summary } = outcome;
    sort_scored_rows(&mut rows);
    if matches!(rank_by, RankBy::Metric(_)) {
        for row in &mut rows {
            row.score = None;
        }
    }
    Optimized { report: SweepReport { rows, rank_by }, summary }
}

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
/// `exec` is a CONSTRUCTOR argument, which is the whole reason [`SweepExec`] does not appear on
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
    exec: SweepExec,
}

impl<'a> StoreEvaluator<'a> {
    /// Objective path. ⚠ Fallible: calls [`require_overridable_params`], which is what makes
    /// [`PointEvaluator::evaluate`] infallible BY TYPE rather than by promise.
    pub fn new(
        base: &'a BacktestProfile,
        store: Arc<dyn HistStore + Send + Sync>,
        objective: &'a Objective,
        label: impl Into<String>,
        exec: SweepExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(StoreEvaluator {
            base,
            store,
            objective: ObjectiveSource::Borrowed(objective),
            rank_by: RankBy::Objective(label.into()),
            exec,
        })
    }

    /// Classic path — [`PointEvaluator::rank_by`] answers `RankBy::Metric(m)` and the objective is
    /// `m.objective()`. Exists ONLY so [`super::sweep::run_sweep_exec`] can delegate without
    /// changing its output (see [`report_from_outcome`]'s Metric arm).
    pub fn classic(
        base: &'a BacktestProfile,
        store: Arc<dyn HistStore + Send + Sync>,
        rank_by: RankMetric,
        exec: SweepExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(StoreEvaluator {
            base,
            store,
            objective: ObjectiveSource::Owned(rank_by.objective()),
            rank_by: RankBy::Metric(rank_by),
            exec,
        })
    }

    /// One candidate -> its scored row, through the shared `sweep::eval_scored_point` fold. The
    /// `Err` arm is unreachable by construction (the constructor refused the only profile whose
    /// build can fail) and records a failed ROW rather than panicking, because a panic in a rayon
    /// worker takes the whole batch with it. The candidate CLONE is what buys that arm an honest
    /// `overrides` list; it is a `Vec<(String, toml::Value)>` beside the whole `BacktestProfile`
    /// clone `profile_with_overrides` performs on the happy path, so it is not measurable.
    fn evaluate_one(&self, candidate: Candidate) -> Evaluated {
        match profile_with_overrides(self.base, candidate.clone()) {
            Ok((profile, overrides)) => {
                let (row, score) = eval_scored_point(
                    self.base,
                    &self.store,
                    self.objective.get(),
                    &profile,
                    overrides,
                );
                Evaluated { row, score }
            }
            Err(e) => Evaluated { row: failed_row(candidate, &e), score: f64::NAN },
        }
    }
}

impl PointEvaluator for StoreEvaluator<'_> {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
        // The pool rule (trait doc): a NEW bounded pool per call is only worth building when there
        // is a neighbourhood to spread it over, so width 1 stays on the calling thread.
        if self.exec == SweepExec::Parallel && batch.len() > 1 {
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
/// optimizer fixed at `(&BacktestProfile, Arc<dyn HistStore>) -> SweepReport` can never host
/// [`super::walkforward`]'s per-window search, whose slice is an INDEX range
/// `crate::walkforward::walk_forward_strategy` cannot express as a store range.
///
/// ⚠ `sweep::eval_scored_point_over_bars` takes `bars` BY VALUE per call, so this clones per
/// candidate — which is what [`super::walkforward`]'s own `map_bounded` closure already does with
/// its `train_series.clone()`. Not a new cost; stated so it is not discovered.
pub struct BarsEvaluator<'a> {
    base: &'a BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<vike_model::Bar>)>,
    objective: &'a Objective,
    rank_by: RankBy,
    exec: SweepExec,
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
        exec: SweepExec,
    ) -> Result<Self, HarnessError> {
        require_overridable_params(base)?;
        Ok(BarsEvaluator {
            base,
            store,
            bars,
            objective,
            rank_by: RankBy::Objective(label.into()),
            exec,
        })
    }

    /// See [`StoreEvaluator`]'s `evaluate_one` — same fold, same unreachable-by-construction `Err`
    /// arm; only the bar source differs.
    fn evaluate_one(&self, candidate: Candidate) -> Evaluated {
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
                Evaluated { row, score }
            }
            Err(e) => Evaluated { row: failed_row(candidate, &e), score: f64::NAN },
        }
    }
}

impl PointEvaluator for BarsEvaluator<'_> {
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
        if self.exec == SweepExec::Parallel && batch.len() > 1 {
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
fn failed_row(overrides: Candidate, e: &HarnessError) -> SweepRow {
    SweepRow { overrides, report: None, error: Some(e.to_string()), score: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(params: &str) -> BacktestProfile {
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
{params}
"#
        ))
        .unwrap()
    }

    /// A [`PointEvaluator`] with NO backtest behind it: it scores a candidate off its `size`
    /// override alone and records the width of every batch it was handed. Enough to drive dispatch
    /// end to end, which is the whole point — the seam must be exercisable without a store.
    #[derive(Default)]
    struct StubEvaluator {
        batches: std::sync::Mutex<Vec<usize>>,
    }

    impl PointEvaluator for StubEvaluator {
        fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated> {
            self.batches.lock().expect("no test panics while holding this").push(batch.len());
            batch
                .into_iter()
                .map(|c| {
                    let score = c
                        .iter()
                        .find(|(k, _)| k == "size")
                        .and_then(|(_, v)| v.as_float())
                        .unwrap_or(f64::NAN);
                    Evaluated {
                        row: SweepRow {
                            overrides: c,
                            report: None,
                            // No backtest ran, so there is no report to carry — the `score` is the
                            // stub's own, which is all the ranking below reads.
                            error: Some("stub evaluator: no backtest".to_string()),
                            score: Some(score),
                        },
                        score,
                    }
                })
                .collect()
        }

        fn rank_by(&self) -> RankBy {
            RankBy::Objective("stub".to_string())
        }
    }

    /// An [`Optimizer`] with no algorithm: it submits one fixed batch and returns what came back.
    /// It exists to prove `Box<dyn Optimizer>` DISPATCH works — that the trait is object-safe, that
    /// [`optimize`] runs the preflight, [`Optimizer::accepts`] and [`Optimizer::search`] in that
    /// order, and that [`report_from_outcome`] is what ranks — with no backtest, no store and no
    /// `datafusion-store` feature anywhere near it. A fourth real method costs one match arm at a
    /// dispatching call site and nothing here.
    struct StubOptimizer {
        sizes: Vec<f64>,
    }

    impl Optimizer for StubOptimizer {
        fn name(&self) -> &'static str {
            "stub"
        }

        fn search(
            &self,
            _base: &BacktestProfile,
            eval: &dyn PointEvaluator,
        ) -> Result<SearchOutcome, HarnessError> {
            let batch: Vec<Candidate> = self
                .sizes
                .iter()
                .map(|&s| vec![("size".to_string(), toml::Value::Float(s))])
                .collect();
            let rows = eval.evaluate(batch).into_iter().map(|e| e.row).collect();
            Ok(SearchOutcome { rows, summary: Some("stub: 1 batch".to_string()) })
        }
    }

    #[test]
    fn a_boxed_optimizer_dispatches_and_its_outcome_is_ranked() {
        let base = profile("[strategy.params]\nsize = 1.0");
        let eval = StubEvaluator::default();
        // The whole point of object safety: the method is chosen at RUNTIME, behind a box.
        let opt: Box<dyn Optimizer> = Box::new(StubOptimizer { sizes: vec![2.0, 5.0, 3.0] });
        assert_eq!(opt.name(), "stub");

        let out = optimize(opt.as_ref(), &base, &eval).expect("the stub search runs");

        assert_eq!(
            *eval.batches.lock().unwrap(),
            vec![3],
            "one batch of three, exactly as submitted — nothing above the searcher re-batches it"
        );
        assert_eq!(out.summary.as_deref(), Some("stub: 1 batch"), "the summary rides through");
        assert_eq!(out.report.rank_by, RankBy::Objective("stub".to_string()));
        // Submitted 2, 5, 3 (evaluation order); the ASSEMBLER is what puts them best-first.
        let ranked: Vec<f64> = out
            .report
            .rows
            .iter()
            .map(|r| r.overrides[0].1.as_float().expect("a float axis"))
            .collect();
        assert_eq!(ranked, vec![5.0, 3.0, 2.0], "the assembler ranks, not the searcher");
    }

    /// The preflight is the FIRST act, so a base profile whose `strategy.params` is not a table is
    /// refused before any candidate is evaluated — not after a method has spent its whole budget
    /// scoring `NaN`. `search` is never entered at all, which is what makes
    /// [`PointEvaluator::evaluate`] infallible by construction rather than by promise.
    #[test]
    fn a_non_table_params_profile_is_refused_before_any_evaluation() {
        let base = profile("params = 3");
        let eval = StubEvaluator::default();
        let opt = StubOptimizer { sizes: vec![1.0, 2.0] };

        match optimize(&opt, &base, &eval) {
            Err(HarnessError::Validation(m)) => assert_eq!(m, PARAMS_NOT_A_TABLE),
            other => panic!("expected the preflight refusal, got {other:?}"),
        }
        assert!(
            eval.batches.lock().unwrap().is_empty(),
            "the refusal must land before the searcher's loop, not after it"
        );
    }
}
