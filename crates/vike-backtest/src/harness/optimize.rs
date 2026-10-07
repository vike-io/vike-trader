//! The `Optimizer` seam: ONE parameter-search METHOD over a profile's `[paramscan]` space, plus the
//! [`PointEvaluator`] half a method drives its loop against.
//!
//! Read OUT of the three searchers that already exist ([`super::sweep`]'s cartesian grid,
//! [`crate::search::euler`]'s successive halving, [`crate::search::tpe`]'s ask/tell Bayesian
//! search) rather than designed ahead of them —
//! `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` is the signed-off design and
//! carries the measurements. Measured, the three share exactly four things:
//! one evaluation unit (`sweep::grid::eval_scored_point` -> `point_row` -> `row_from_outcome` ->
//! `score_row`), one ordering rule (`sweep::sort_scored_rows` over `sweep::cmp_scores_desc`), one
//! profile builder (`sweep::profile_with_overrides`) and one bounded executor
//! (`sweep::threads::install_bounded`). They share NEITHER the loop, NOR the candidate type, NOR
//! the budget, NOR the report shape — three loop shapes, three budget KINDS, three accepted spaces.
//! So **the searcher is the part that differs and the evaluation is the part that does not**: this
//! module abstracts the evaluation and leaves each searcher its own loop.
//!
//! The three implementations are plain data and each lives in its own existing module
//! (`sweep::GridSearch`, `euler::EulerSearch`, `tpe::TpeSearch`) — deliberately, because that needs
//! ZERO visibility widening: `euler`'s `numeric_axes` and `tpe`'s `tpe_space_from_profile` are
//! private and stay private, since the impl that calls them is in the same file. A method's
//! knowledge stays in the method's file; this module holds only the seam.
//!
//! Compiled unconditionally with the rest of `harness/` (a DEFAULT build since the 2026-09-27
//! feature collapse), and so by BOTH the default trait-only lane and the `datafusion-store` lane.

mod evaluator;
mod overfit;
mod progress;

use super::sweep::{
    self, PARAMS_NOT_A_TABLE, ParamscanReport, ParamscanRow, RankBy, sort_scored_rows,
};
use super::{BacktestProfile, HarnessError};

pub use evaluator::{BarsEvaluator, StoreEvaluator};
pub use overfit::{CapturedReturns, DEFAULT_CSCV_SPLITS, overfit_stats};
pub use progress::{
    ProgressEvent, ProgressMode, ProgressSink, StderrProgress, TradeFloor, apply_trade_floor,
};

#[cfg(doc)]
use super::run_backtest;
#[cfg(doc)]
use super::sweep::RankMetric;
#[cfg(doc)]
use crate::search::objective::Objective;

/// One proposed point, in the ONE currency all three searchers already converge on: the
/// `(param name, TOML value)` overrides `super::sweep::profile_with_overrides` takes.
///
/// An ALIAS, not a newtype, so a candidate, a [`ParamscanRow`]'s `overrides` field and that builder's
/// argument are literally the same value.
///
/// ⚠ NOT [`super::sweep::ParamscanPoint`], which also bundles a built [`BacktestProfile`] — that field
/// is why `expand_paramscan` materializes N full profile clones before running one point, and the other
/// two searchers build theirs per point anyway. ⚠ NOT `Vec<f64>`: a numeric currency would exclude
/// the GRID from its own trait, since `sweep`'s `sweep_axis_array` accepts string and bool axes that
/// its `numeric_sweep_axes` refuses. The numeric methods keep their coordinates PRIVATE and render
/// at this boundary, exactly as `euler`'s `AxisKind::to_toml` and [`vike_ml::tpe::ParamDomain`]'s
/// `to_toml` already do — which is also what preserves euler's raw-bit-pattern dedup key.
pub type Candidate = Vec<(String, toml::Value)>;

/// One evaluated point: the row a report will print, and the raw steering score the searcher
/// refines on. Exactly the pair `sweep::grid::eval_scored_point` already returns and
/// [`crate::search::tpe::run_tpe`] already forks (row to `rows`, score to
/// [`vike_ml::tpe::TpeOptimizer`]'s `tell`) — naming it is the only change.
///
/// ⚠ A STRUCT rather than a tuple ON PURPOSE: an `elapsed`, a fidelity tag, or an evaluation-cost
/// field can be added without touching a single [`Optimizer`] signature. That is the extension
/// point for cost-aware acquisition, and it is why this is not `(ParamscanRow, f64)`.
#[derive(Debug, Clone)]
pub struct Evaluated {
    pub row: ParamscanRow,
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
    /// [`crate::search::tpe::run_tpe`] sorted before returning, so nothing downstream could see the
    /// search path at all.
    pub rows: Vec<ParamscanRow>,
    /// This method's own one-line cost summary, ALREADY RENDERED, for a verb to print on stderr so
    /// a `--json` stdout stays a clean document. `None` for a method with nothing to say.
    ///
    /// A `String`, because a string is what all three ALREADY are at the point of use:
    /// [`crate::search::euler::EulerBudget`] reaches stderr through its own `Display`, TPE's line
    /// is hand-assembled in `crates/vike-backtest/src/backtest_cli.rs` from flags plus the best
    /// row's score, and the grid prints nothing. A typed `Diagnostics` would be euler-shaped and
    /// filled by no one else.
    pub summary: Option<String>,
}

/// The ranked answer. Replaces euler's `(ParamscanReport, EulerBudget)` tuple as the ONE return shape.
#[derive(Debug)]
pub struct Optimized {
    pub report: ParamscanReport,
    pub summary: Option<String>,
}

/// ONE parameter-search METHOD over a profile's `[paramscan]` space. Grid, euler and TPE are its first
/// three implementations.
///
/// # The searcher owns its loop
///
/// [`Optimizer::search`] receives an evaluator and drives its own iteration to completion. That is
/// how all three are written TODAY — [`super::sweep::GridSearch`] (one turn),
/// `crate::search::euler_search_batched` (one turn per refinement depth),
/// [`crate::search::tpe::TpeSearch`] (one turn per trial) — so this is a WIDENING of existing code
/// rather than a shape imposed on it. `crates/vike-backtest/src/search.rs` needs no change at all,
/// which is the sharpest available evidence that the seam is real.
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
/// constructed inside `search` — which is exactly where [`vike_ml::tpe::TpeOptimizer`] and euler's
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

    /// How many evaluations this method will spend on `base`, when that is a number the method can
    /// stand behind BEFORE it starts — `None` when it is not.
    ///
    /// ⚠ **`None` is a real answer and euler returns it**, which is the whole reason this is an
    /// `Option` rather than a `u64` with a sentinel. A progress line reading `17/49` is a promise,
    /// and euler cannot make it: its cost is `N + D*(3^d - 1)` AT MOST and usually far less (every
    /// candidate already seen is skipped), and it stops early the moment a depth produces no unseen
    /// candidate. Publishing that bound would put a total on screen that the run then beats by 5x,
    /// and an ETA derived from it would count down to a time the search never reaches. A missing
    /// total costs a progress line its ETA; a WRONG total costs the operator their trust in every
    /// other number on it.
    ///
    /// Default `None`, so a fifth method reports no total until it has one to report — the safe
    /// direction, and the same reason [`Optimizer::accepts`] defaults permissive.
    ///
    /// ⚠ NOT a budget the seam ENFORCES. Nothing reads this to stop a search; it exists so an
    /// observer can divide. A method that overshoots its own hint is reporting badly, not
    /// misbehaving.
    fn budget_hint(&self, base: &BacktestProfile) -> Option<u64> {
        let _ = base;
        None
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
/// [`StoreEvaluator`] (re-scan the store per point — `sweep::grid::eval_scored_point`) and
/// [`BarsEvaluator`] (bars the caller already holds — `sweep::grid::eval_scored_point_over_bars`,
/// which is what a walk-forward window needs because its slice is an INDEX range
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
    /// `exec == ParamscanExec::Parallel` AND `batch.len() > 1`**. `install_bounded` constructs a NEW
    /// `rayon::ThreadPoolBuilder` on every call, and euler's own comment justified per-batch
    /// construction by WIDTH ("a batch is one refinement DEPTH, so the spawn cost is amortized over
    /// a whole neighbourhood"). At width 1 there is no neighbourhood: without this clause TPE, which
    /// enters rayon ZERO times today, would build [`vike_ml::tpe::TpeConfig`]'s `DEFAULT_TRIALS` = 64
    /// four-thread pools to run 64 single backtests. With it, TPE's "no fan-out" is EXACT rather
    /// than a documented no-op parameter.
    ///
    /// ⚠ An evaluator is the ONLY place this workspace enters rayon inside a search. A caller that
    /// is ALREADY inside a bounded pool must construct its evaluator with `ParamscanExec::Sequential`,
    /// or the `min(4, cores)` memory bound composes MULTIPLICATIVELY. Nothing nests today —
    /// [`crate::walkforward::runner`]'s window loop is sequential — but hosting a searcher per
    /// window is the obvious next step and this is where that bites.
    fn evaluate(&self, batch: Vec<Candidate>) -> Vec<Evaluated>;

    /// How the report should label its ranking. Lives HERE, beside the [`Objective`] it names,
    /// because `crate::search::objective::Objective` is a `Box<dyn Fn>` with no name of its own —
    /// the separate label exists only for that reason, and pairing them means they cannot disagree.
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

/// Turn a method-agnostic [`SearchOutcome`] into the [`ParamscanReport`] every caller already prints —
/// THE one place rows get ordered, and the one place the classic/objective output split is decided.
///
/// Sorting is `sweep`'s `sort_scored_rows` over [`super::sweep::cmp_scores_desc`]: score descending,
/// `NaN` after finite, `score: None` (a failed point) last, STABLE so ties keep evaluation order.
///
/// ⚠ The [`RankBy::Metric`] arm additionally CLEARS every row's `score` AFTER sorting. That is not
/// tidiness — it is what lets [`super::sweep::run_paramscan_exec`] become an adapter over this seam
/// while its table and its `--json` stay byte-identical (`ParamscanRow`'s `score` is
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
    Optimized { report: ParamscanReport { rows, rank_by, summary: summary.clone() }, summary }
}

#[cfg(test)]
use super::sweep::ReturnBuckets;
#[cfg(test)]
use crate::search::trials::candidate_key;
#[cfg(test)]
use overfit::curve_from_returns;
#[cfg(test)]
use progress::ProgressTracker;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

#[path = "optimize_tests.rs"]
#[cfg(test)]
mod optimize_tests;
