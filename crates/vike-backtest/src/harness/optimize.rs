//! The `Optimizer` seam: ONE parameter-search METHOD over a profile's `[paramscan]` space, plus the
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
//! Compiled unconditionally with the rest of `harness/` (a DEFAULT build since the 2026-09-27
//! feature collapse), and so by BOTH the default trait-only lane and the `datafusion-store` lane.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use vike_data::HistStore;

use crate::objective::Objective;

use super::sweep::{
    PARAMS_NOT_A_TABLE, ParamscanExec, ParamscanReport, ParamscanRow, RankBy, RankMetric,
    ReturnBuckets, eval_scored_point, eval_scored_point_over_bars, install_bounded,
    profile_with_overrides, sort_scored_rows,
};
use super::trials::candidate_key;
use super::{BacktestProfile, HarnessError};

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
/// refines on. Exactly the pair `sweep::eval_scored_point` already returns and
/// [`super::tpe::run_tpe`] already forks (row to `rows`, score to [`vike_ml::tpe::TpeOptimizer`]'s
/// `tell`) — naming it is the only change.
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
    /// [`super::tpe::run_tpe`] sorted before returning, so nothing downstream could see the search
    /// path at all.
    pub rows: Vec<ParamscanRow>,
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

/// **The per-trial return vectors one search retained, keyed by candidate** — the side channel
/// [`StoreEvaluator`] fills and [`overfit_stats`] joins against the ranked rows.
///
/// ⚠ **A SIDE CHANNEL rather than a field on [`ParamscanRow`], and the arithmetic behind that is
/// already written down one module over.** `super::trials`'s module doc measured the alternatives
/// when the trial LEDGER needed per-evaluation information: "a field on [`SearchOutcome`] would be
/// 6 struct-literal edits, a field on [`ParamscanRow`] 19". Both counts still hold, and a row
/// field would also cost something a decorator cannot: [`ParamscanReport`] CROSSES THE DATAHUB WIRE
/// (`vike_datahub_client::proto::Response::ParamscanReport`), so a `#[serde(skip)]` 4 KB field
/// would sit on the wire type and
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` would then rest on an ATTRIBUTE being right
/// rather than on the type carrying nothing new at all.
///
/// ⚠ **Keyed by `super::trials::candidate_key`, which is exact and total** — a [`Candidate`] is
/// `Vec<(String, toml::Value)>` and cannot be a map key on its own (no `Eq`, no `Hash`); that
/// function keys floats by BIT PATTERN and escapes its own separators, so two distinct candidates
/// cannot collide. A candidate evaluated TWICE inside one search (a resume whose warm cache missed
/// a ledger line it holds) overwrites its own entry with an identical vector, because the same
/// candidate over the same data produces the same curve — a repeat is not a conflict.
#[derive(Debug, Clone, Default)]
pub struct CapturedReturns {
    buckets: usize,
    by_candidate: HashMap<String, Vec<f64>>,
}

impl CapturedReturns {
    /// The bucket count that was REQUESTED, which is not the same as a column's LENGTH:
    /// `ReturnBuckets::capture` shortens a column over a range holding fewer returns than
    /// buckets. `crate::trial_ledger::OverfitStats`'s `buckets` reports the length actually used.
    pub fn buckets(&self) -> usize {
        self.buckets
    }

    /// How many trials retained a vector.
    pub fn len(&self) -> usize {
        self.by_candidate.len()
    }

    /// Whether nothing was retained — the state of every search that did not opt in.
    pub fn is_empty(&self) -> bool {
        self.by_candidate.is_empty()
    }

    /// This candidate's retained vector, if it has one. A row whose point FAILED, was answered
    /// from a warm CACHE, or produced a non-finite block return has none.
    pub fn get(&self, overrides: &Candidate) -> Option<&[f64]> {
        self.by_candidate.get(&candidate_key(overrides)).map(Vec::as_slice)
    }
}

/// **The CSCV split count `overfit_stats` uses**, and the reason a `T` of 512 is sized the way it
/// is.
///
/// Sixteen is López de Prado's own setting for CSCV and the one `pbo_cscv`'s doc is written
/// against. It is fixed rather than a knob because the number is not free: `pbo_cscv` iterates
/// `C(S, S/2)` combinations — `C(16, 8)` = 12 870 — and each one takes two column-mean passes over
/// half the matrix, so the whole statistic is `~C(S, S/2) * T * N` multiply-adds. At `S = 16`,
/// `T = 512`, `N = 500` that is a few billion, i.e. seconds, paid ONCE at the end of a search that
/// already took hours. `S = 20` would be four times that for no extra resolution, and `S = 8`
/// (`C(8,4)` = 70) gives a PBO estimated from seventy logits.
///
/// ⚠ `pbo_cscv` PANICS on an odd count — the CSCV construction has no meaning without an even
/// split — so this constant being even is load-bearing, not cosmetic.
pub const DEFAULT_CSCV_SPLITS: usize = 16;

/// **The anti-overfitting statistics of a whole search, computed once over the ranked rows** —
/// PBO via CSCV, the correlation-corrected effective trial count, and the deflated Sharpe
/// benchmarked against it.
///
/// `None` when there is nothing to compute — no search opted in, every trial failed, or the range
/// was too short to give `T >= splits`. A `None` here is why
/// `crate::trial_ledger::TrialsDocument::overfit` is an `Option`: a document that carried a zeroed
/// block would read as "measured, and clean".
///
/// # What the numbers are computed over, stated because it is easy to assume otherwise
///
/// * **The matrix is `T` observations x `N` trials, TRANSPOSED here.** Each trial's retained vector
///   is `T` long and `pbo_cscv` indexes `matrix[t][j]`, so the columns are built by transposition;
///   that costs one more `T * N` allocation (2 MB at the defaults) and is the whole reason
///   `effective_n_trials`, which wants the PER-TRIAL series, is fed `series` instead.
/// * **Column ORDER is the ranked row order**, so the statistic is deterministic even though the
///   evaluator filled its map from rayon workers in completion order. That matters: `pbo_cscv`'s
///   first-argmax tie-break reads column index, so a map-iteration order would make the PBO of a
///   search with tied points depend on scheduling.
/// * **`observed_sharpe` is the WINNER's**, i.e. ranked row 0 — the configuration a search actually
///   hands onward (`--export-params`, a walk-forward's chosen parameters). Deflating the mean or
///   the median would answer a question nobody asks of a search.
/// * **`n_obs` is `T`, not the bar count**, and that is not a loss of power:
///   [`ReturnBuckets`]'s own doc carries the invariance argument (`sr_per_obs * sqrt(n - 1)` is
///   preserved by compounding).
/// * **A row with no retained vector is EXCLUDED and counted**, never zero-filled. The reachable
///   causes are a failed point, a resume's reused trial (whose answer is a score, not a report —
///   `super::trials::WarmTrial` says why), and a curve that touched exactly zero. `excluded` is
///   what makes a thin matrix visible instead of merely small.
///
/// ⚠ **Every statistic is reported as `None` rather than as a number when it is uncomputable.**
/// `pbo_cscv` answers `NaN` for a degenerate matrix, which `overfit::overfit_verdict` reads as
/// "not assessed" — a `0.0` would read as "no overfit", the exact opposite. The verdict is carried
/// alongside so a reader does not have to re-derive the thresholds.
pub fn overfit_stats(
    rows: &[ParamscanRow],
    captured: &CapturedReturns,
    splits: usize,
) -> Option<crate::trial_ledger::OverfitStats> {
    use vike_analytics::overfit;

    if captured.is_empty() || splits < 2 {
        return None;
    }
    // Per-trial series in RANKED row order — see the ordering note above.
    let mut series: Vec<Vec<f64>> = Vec::new();
    let mut excluded = 0usize;
    let mut t = 0usize;
    for row in rows {
        match captured.get(&row.overrides) {
            // The first admitted column fixes `T`. A column of another length cannot be
            // time-aligned with it (bucket `b` would cover a different fraction of the range), so
            // it is excluded rather than truncated. Unreachable while every trial of a search runs
            // one `[data]` range, which is why this is a belt and not a documented mode.
            Some(col) if t == 0 || col.len() == t => {
                t = col.len();
                series.push(col.to_vec());
            }
            _ => excluded += 1,
        }
    }
    if series.is_empty() || t < splits {
        return None;
    }

    // T x N, the shape `pbo_cscv` indexes.
    let matrix: Vec<Vec<f64>> = (0..t).map(|i| series.iter().map(|col| col[i]).collect()).collect();
    let pbo = overfit::pbo_cscv(&matrix, splits);

    // Each trial's PER-OBSERVATION moments, derived through the ONE home for the two unit
    // conventions (`overfit::sharpe_moments`) rather than re-spelled here: a pseudo-curve whose
    // successive ratios ARE the bucket returns is what that function wants, and
    // `vike_analytics::metrics::returns` recovers them from it.
    let moments: Vec<overfit::SharpeMoments> =
        series.iter().map(|col| overfit::sharpe_moments(&curve_from_returns(col))).collect();
    let trial_sharpes: Vec<f64> = moments.iter().map(|m| m.sr_per_obs).collect();
    let best = moments[0];
    let effective_n = overfit::effective_n_trials(&series);
    let deflated = overfit::deflated_sharpe_with_effective_n(
        best.sr_per_obs,
        &trial_sharpes,
        &series,
        best.n_obs,
        best.skew,
        best.kurt,
    );
    // ⚠ `pbo` is handed on as-is, NaN included: `overfit_verdict`'s first branch is the
    // "not assessed" one and reads that NaN deliberately. Collapsing it to a number first would
    // delete the only signal that the matrix was degenerate.
    let verdict = overfit::overfit_verdict(pbo, deflated, None);

    Some(crate::trial_ledger::OverfitStats {
        buckets: t,
        requested_buckets: captured.buckets(),
        trials: series.len(),
        excluded,
        splits,
        pbo: finite(pbo),
        effective_n: finite(effective_n),
        deflated_sharpe: finite(deflated),
        observed_sharpe: finite(best.sr_per_obs),
        observations: best.n_obs,
        verdict: match verdict.level {
            overfit::OverfitLevel::Low => "low",
            overfit::OverfitLevel::Medium => "medium",
            overfit::OverfitLevel::High => "high",
        }
        .to_string(),
    })
}

/// A non-finite statistic becomes `None`, so the document says "uncomputable" in one spelling.
///
/// ⚠ Not cosmetic: `crates/vike-cli/src/cmd/runs/jsondoc.rs`'s `number_at` already filters a
/// non-finite leaf to `None`, and `serde_json::Number` cannot hold one — so a `NaN` written into a
/// document either serializes as `null` or, on a format that refuses it, fails the whole write.
/// Making the ABSENCE explicit in the type means `backtest gate --fail-if` reports "carries no
/// finite number under `overfit.pbo`" rather than a missing key, which names the right problem.
fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

/// A pseudo equity curve whose successive RATIOS are `returns` — the shape
/// `overfit::sharpe_moments` takes.
///
/// ⚠ **This exists so the two unit conventions have ONE home.** `sharpe_moments`'s doc is emphatic
/// that `sr_per_obs` must be per-period (an annualized Sharpe saturates PSR to ~1.0 and makes the
/// significance test vacuous) and that `kurt` must be rebased to non-excess, and it derives both
/// from an equity CURVE. Computing mean/std/skew/kurtosis over the bucket returns directly would
/// be a second spelling of `vike_analytics::metrics`' conventions, which is exactly the
/// duplication `crates/vike-ops/tests/docs/one_authority_gate.rs` exists to refuse.
///
/// Starts at `1.0`, so the curve is a growth index and no cash scale is invented. The roundtrip is
/// not bit-exact — `metrics::returns` recomputes `c[i]/c[i-1] - 1` from a product — but the
/// statistic is over hundreds of samples and the error is last-bit; a bit-exact path would mean
/// duplicating the moment formulas, which is the thing being avoided.
fn curve_from_returns(returns: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(returns.len() + 1);
    let mut level = 1.0f64;
    out.push(level);
    for r in returns {
        level *= 1.0 + r;
        out.push(level);
    }
    out
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
    /// The retained vectors, keyed by `super::trials::candidate_key`. See [`CapturedReturns`] for
    /// why this is a side channel rather than a field on [`ParamscanRow`].
    ///
    /// ⚠ **A `Mutex` inside the rayon fan-out, and it is not on the hot path in the sense that
    /// matters.** One lock per POINT, taken after a whole backtest has run — against
    /// `ProgressTracker`, which already locks per point for a much cheaper piece of work. Under
    /// [`ReturnBuckets::DISARMED`] it is never taken at all, because `capture` answered `None`.
    captured: Mutex<HashMap<String, Vec<f64>>>,
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
        })
    }

    /// Arm the statistical-significance floor. **A CONSUMING builder, not a constructor argument**,
    /// and that is a compatibility decision rather than a style one: a seventh parameter on
    /// [`StoreEvaluator::new`] and an equivalent on [`StoreEvaluator::classic`] would have to be
    /// spelled at every existing call site — the engine binary, the wire arm, `super::euler`'s and
    /// `super::tpe`'s adapters — each of which would then pass a value meaning "unchanged". A
    /// builder that defaults to [`TradeFloor::DISARMED`] makes "unchanged" the thing nobody has to
    /// write, so an unarmed run stays byte-identical BY CONSTRUCTION rather than by four correct
    /// arguments.
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

    /// The vectors this evaluator retained, for [`overfit_stats`] to join against the RANKED rows.
    ///
    /// ⚠ Read AFTER the search, from the caller that still owns the evaluator — the
    /// `super::trials::TrialRecorder` that wraps it borrows `&self`, so this is a second shared
    /// borrow and never a conflict. Empty for every search that never called
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

    /// One candidate -> its scored row, through the shared `sweep::eval_scored_point` fold. The
    /// `Err` arm is unreachable by construction (the constructor refused the only profile whose
    /// build can fail) and records a failed ROW rather than panicking, because a panic in a rayon
    /// worker takes the whole batch with it. The candidate CLONE is what buys that arm an honest
    /// `overrides` list; it is a `Vec<(String, toml::Value)>` beside the whole `BacktestProfile`
    /// clone `profile_with_overrides` performs on the happy path, so it is not measurable.
    fn evaluate_one(&self, candidate: Candidate) -> Evaluated {
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
    exec: ParamscanExec,
    /// The statistical-significance floor, DISARMED by default — so an evaluator built by any
    /// existing call site behaves exactly as it did before [`TradeFloor`] existed.
    floor: TradeFloor,
    /// The progress observer, absent by default. `Option` rather than a no-op sink so a search with
    /// progress off costs no counter, no clock read and no virtual call per point.
    progress: Option<ProgressTracker>,
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
    /// cross-window total here: `super::walkforward` owns the window list and is the only place
    /// that could add one up.
    pub fn with_progress(mut self, sink: Box<dyn ProgressSink>, total: Option<u64>) -> Self {
        self.progress = Some(ProgressTracker::new(sink, total));
        self
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

/// The statistical-significance FLOOR: the trade count below which a point is not ranked at all.
///
/// ⚠ **This exists because a lucky pair of trades otherwise wins a 500-point grid**, and nothing in
/// the ranking can tell the difference. Every [`crate::objective::Objective`] in this crate is a
/// function of a [`vike_analytics::report::BacktestReport`] alone, and a report over two trades that both
/// went the right way carries a Sharpe, a total return and a profit factor that are arithmetically
/// enormous and statistically empty. The grid then crowns it, `super::trials`'s ledger records it
/// as the winner, and whatever reads the #1 row downstream — a walk-forward window's chosen
/// parameters, an exported params file — inherits a configuration fitted to two bars.
///
/// ⚠ **It is NOT `crate::objective::trade_count_penalty`, and the two do different jobs.** That
/// penalty is a SOFT multiplicative term inside `crate::objective::multi_metric` only: it is
/// floored at `penalty_floor` (default `0.1`, deliberately, so a thin-but-promising point stays
/// VISIBLE), and it is not applied at all under the four classic `--rank-by` metrics — which
/// includes the default `sharpe` and therefore the invocation nearly every operator actually
/// types. A floor that can be out-earned is not a floor. This one replaces the score outright and
/// applies under every ranking and every method, because it answers a different question: not "how
/// much should thinness cost" but "is this sample admissible evidence at all".
///
/// ⚠ **`0` is DISARMED and is byte-identical to the knob never existing** — that is what makes an
/// explicitly written `--min-trades 0` honest rather than a silent no-op, and it is why
/// [`apply_trade_floor`] returns its argument untouched rather than re-stamping an equal value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TradeFloor(usize);

impl TradeFloor {
    /// No floor. Every point is ranked on its own score, exactly as before this type existed.
    pub const DISARMED: TradeFloor = TradeFloor(0);

    /// A floor of `min_trades`. `0` builds [`TradeFloor::DISARMED`].
    pub fn new(min_trades: usize) -> Self {
        TradeFloor(min_trades)
    }

    /// The count itself — what a progress line or a summary reports.
    pub fn min_trades(self) -> usize {
        self.0
    }

    /// Whether this floor does anything at all.
    pub fn is_armed(self) -> bool {
        self.0 > 0
    }

    /// Whether a run with `n_trades` trades is admissible evidence. A DISARMED floor admits
    /// everything, including a zero-trade run — which is the pre-existing behaviour and must stay
    /// reachable, because a zero-trade point is legitimate information about a parameter region.
    pub fn admits(self, n_trades: usize) -> bool {
        n_trades >= self.0
    }
}

/// Apply a [`TradeFloor`] to one evaluated point — **the ONE place thinness becomes unrankability**,
/// so the four methods, both bar sources and both evaluator constructions cannot answer differently.
///
/// ⚠ **The floored score is `NaN`, and `NaN` is not a shortcut for "very low".** It is this crate's
/// written contract for UNRANKABLE ([`Optimizer`]'s score contract), and choosing it buys four
/// behaviours that already exist rather than four that would have to be written: `super::sweep`'s
/// `cmp_scores_desc` sorts it after every finite score; `crate::search`'s `better` refuses to let it
/// displace a finite incumbent, so euler cannot refine INTO a thin region; `super::tpe`'s
/// observation fold maps it to `f64::NEG_INFINITY` before telling the model, so TPE learns the
/// region is worthless instead of dropping the trial; and `super::genetic` treats an absent score as
/// the worst fitness. `f64::NEG_INFINITY` was the other candidate and is refused by that same
/// contract — it is finite-comparable, so it would sort merely last-among-finite and change tie
/// behaviour for every method at once.
///
/// ⚠ **The stamped `row.score` is updated too, and it has to be**: `sort_scored_rows` reads the ROW,
/// not the steering score, so flooring only the steering value would move the refinement centre
/// while leaving the report's #1 row the thin point. `Option::map` rather than an assignment, so a
/// row that carried no stamped score does not GAIN one — that would add a `"score": null` key to a
/// document that had none.
///
/// ⚠ **A FAILED point is left exactly as it is.** Its report is `None`, so it has no trade count to
/// judge, and it is already unrankable by `score_row`'s own rule. Overwriting it here would be the
/// same value written twice — stated so nobody "fixes" the early return into a stamp.
///
/// The row's `report` is deliberately UNTOUCHED: `n_trades` stays readable in the `--json` document,
/// which is the only way an operator can see WHY a row fell to the bottom. `ParamscanRow`'s
/// report-XOR-error rule forbids writing an explanation into `error`.
pub fn apply_trade_floor(mut ev: Evaluated, floor: TradeFloor) -> Evaluated {
    if !floor.is_armed() {
        return ev;
    }
    let admissible = match ev.row.report.as_ref() {
        Some(report) => floor.admits(report.n_trades),
        // No report: a failed point, already unrankable. Nothing to judge and nothing to change.
        None => return ev,
    };
    if admissible {
        return ev;
    }
    ev.row.score = ev.row.score.map(|_| f64::NAN);
    ev.score = f64::NAN;
    ev
}

/// One progress observation: where a search is, how long it has taken, and what it has found.
///
/// ⚠ **Observational ONLY. Nothing here reaches the report**, and that is the property that makes a
/// clock and a counter admissible inside a seam whose determinism gates compare bytes:
/// `super::sweep`'s `parallel_and_sequential_sweeps_are_byte_identical` and euler's twin compare
/// [`ParamscanReport`]s, which no field of this struct enters. What IS non-deterministic is the
/// EVENT STREAM — under `ParamscanExec::Parallel` events arrive in completion order, so two runs of
/// the same search emit the same events in different orders with different elapsed times. Never
/// assert on it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgressEvent {
    /// Points evaluated so far, INCLUDING this one. Always `>= 1`.
    pub completed: u64,
    /// [`Optimizer::budget_hint`]'s answer — `None` for a method that cannot promise a total.
    pub total: Option<u64>,
    /// Wall time since the observer was armed, which is the evaluator's construction and therefore
    /// a hair before the first point rather than at it.
    pub elapsed: Duration,
    /// The best score seen so far under `super::sweep::cmp_scores_desc`, AFTER the trade floor —
    /// so a progress line can never advertise a leader the report will bury. `None` before anything
    /// rankable has been seen, and it STAYS `None` through a run where nothing is rankable.
    pub best: Option<f64>,
}

impl ProgressEvent {
    /// Time remaining, extrapolated from the mean cost of the points already done.
    ///
    /// ⚠ `None` without a `total`, and `None` on a run whose elapsed time is still zero — a mean of
    /// zero would render an ETA of zero seconds on a search about to take an hour, which is worse
    /// than printing nothing. Linear extrapolation is the honest model here and nothing more is
    /// claimed for it: every point runs one backtest over the same range, so the per-point cost is
    /// near-constant and the error is the variance of the data loader, not of the algorithm.
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total?;
        let done = self.completed.min(total);
        let remaining = total.checked_sub(done)?;
        if remaining == 0 {
            return Some(Duration::ZERO);
        }
        let per_point = self.elapsed.checked_div(u32::try_from(done).ok()?)?;
        if per_point.is_zero() {
            return None;
        }
        per_point.checked_mul(u32::try_from(remaining).ok()?)
    }

    /// The human line, for STDERR. No trailing newline — the sink owns the line ending, because a
    /// terminal sink may one day want a carriage return instead.
    ///
    /// ⚠ **It renders nothing a machine is meant to parse**, deliberately: the shape is free to
    /// change, and the framed alternative is [`ProgressEvent::render_json`]. A test that greps this
    /// string for a number is pinning the wrong surface.
    pub fn render_human(&self) -> String {
        let of = match self.total {
            Some(total) => format!("{}/{}", self.completed, total),
            None => format!("{}", self.completed),
        };
        let best = match self.best {
            Some(s) => format!("{s:.4}"),
            None => "n/a".to_string(),
        };
        let eta = match self.eta() {
            Some(d) => format!(", eta {}", render_secs(d)),
            None => String::new(),
        };
        format!(
            "backtest: searched {of} points in {}{eta} — best {best}",
            render_secs(self.elapsed)
        )
    }

    /// The MACHINE line: one self-contained JSON object, no trailing newline, for a caller that
    /// writes newline-delimited JSON to STDERR.
    ///
    /// ⚠ **Hand-assembled rather than serde-derived, and that is a dependency decision.** This
    /// crate's harness tree carries no `serde_json` edge — [`ParamscanReport`] derives `Serialize`
    /// and lets the BINARY choose a serializer — and adding one so a progress line can be printed
    /// would put a serializer in the search's build graph for five scalar fields. None of them can
    /// contain a character needing an escape, so there is nothing for a serializer to get right that
    /// this does not.
    ///
    /// ⚠ **A non-finite `best` renders `null`**, matching `super::sweep`'s `ser_opt_score` rule
    /// exactly — `null` means unrankable on both surfaces. A bare `NaN` token would make the line
    /// unparseable, which on the one output shape whose entire purpose is being parsed is the worst
    /// available failure.
    pub fn render_json(&self) -> String {
        let total = match self.total {
            Some(t) => t.to_string(),
            None => "null".to_string(),
        };
        let best = match self.best {
            Some(s) if s.is_finite() => format!("{s}"),
            _ => "null".to_string(),
        };
        let eta = match self.eta() {
            Some(d) => format!("{:.3}", d.as_secs_f64()),
            None => "null".to_string(),
        };
        format!(
            "{{\"event\":\"search_progress\",\"completed\":{},\"total\":{total},\"elapsed_s\":{:.3},\"eta_s\":{eta},\"best\":{best}}}",
            self.completed,
            self.elapsed.as_secs_f64()
        )
    }
}

/// `1h02m03s` / `2m03s` / `3.4s` — a duration a human reads at a glance, with no dependency behind
/// it. Seconds get a decimal only below a minute, where the difference between 3s and 3.4s is the
/// difference between "fast" and "this is going to take a while".
fn render_secs(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Where a [`ProgressEvent`] goes.
///
/// ⚠ **A TRAIT rather than a hard-wired `eprintln!`, because a harness module writing to a stream is
/// the thing this tree has deliberately never done.** Every other cost line a method produces
/// ([`SearchOutcome`]'s `summary`) is returned to the caller ALREADY RENDERED and printed by the
/// binary, precisely so a library cannot decide what an operator's terminal shows. Progress cannot
/// use that shape — it is per-point and the run has not returned yet — so the caller supplies the
/// destination instead, and [`StderrProgress`] is the one this crate ships for it.
///
/// `Send + Sync` is not decorative: [`StoreEvaluator`]'s `evaluate` fans a batch out with
/// `into_par_iter()` over `&self`, so every rayon worker calls [`ProgressSink::point`] concurrently.
/// An implementation that buffers must synchronise; `eprintln!` is line-atomic under the `Stderr`
/// lock, which is why the shipped sink needs nothing.
pub trait ProgressSink: Send + Sync {
    /// One point finished. MUST NOT write to stdout — see [`StderrProgress`]'s own argument.
    fn point(&self, ev: &ProgressEvent);
}

/// What `--progress` selected.
///
/// ⚠ `Auto` is not "on": it is "on IF a human is watching". A non-terminal stderr means a log file,
/// a CI transcript or a captured pipe, and appending one line per backtest to a captured log that
/// nobody will read is how a log directory reached 341 GB elsewhere in this workspace.
/// [`StderrProgress::for_mode`] performs the decision once, at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressMode {
    /// Human-readable, throttled, ONLY when stderr is a terminal. The default.
    #[default]
    Auto,
    /// Silent. Nothing is constructed and nothing is counted. ⚠ Spelled `none` on the command line
    /// and `Off` here, deliberately: a variant literally named `None` sits beside `Option::None` in
    /// every match in this file, and a reader cannot tell which one an arm means at a glance.
    Off,
    /// One newline-delimited JSON event per point on stderr, terminal or not, UNTHROTTLED — a
    /// machine consuming a stream wants every event, and dropping some to save a line is a
    /// disservice to the only consumer that can use them all.
    Json,
}

impl ProgressMode {
    /// The spellings, in the order a usage line should print them. The ONE roster — a refusal
    /// RENDERS it rather than restating it, so a fourth mode cannot be accepted by the parser and
    /// missing from the message.
    pub const NAMES: [&'static str; 3] = ["auto", "none", "json"];

    /// Case-insensitive parse, `None` for anything else — the same shape
    /// `super::sweep::RankMetric::from_str_ci` has, so the caller owns the refusal SENTENCE and this
    /// owns only the grammar.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Some(ProgressMode::Auto),
            "none" => Some(ProgressMode::Off),
            "json" => Some(ProgressMode::Json),
            _ => None,
        }
    }
}

/// The shipped [`ProgressSink`]: **STDERR, and structurally never stdout.**
///
/// ⚠ **The stdout rule is the load-bearing one on this plane, and here it is the type's shape rather
/// than a convention.** `vike-cli backtest path` exists to print one line and nothing else, and a
/// `--json` report must stay parseable — so a single progress line on stdout corrupts a document a
/// script is piping. This type writes through `eprintln!` at both of its two call sites and holds no
/// handle a caller could redirect, so there is no configuration under which it can reach the other
/// stream.
pub struct StderrProgress {
    json: bool,
    /// `0` = emit every event. Milliseconds of elapsed time between human lines.
    min_interval_ms: u64,
    /// Elapsed-millis of the last emitted line, or `u64::MAX` for "nothing emitted yet". An atomic
    /// rather than an `Instant` because [`ProgressEvent`] already carries the elapsed time, so the
    /// throttle needs no clock of its own and no lock to be poisoned in a rayon worker.
    last_ms: AtomicU64,
}

impl StderrProgress {
    /// Twice a second. Fast enough that a search feels live, slow enough that a 4000-point grid
    /// costs a terminal a few hundred lines instead of four thousand.
    pub const HUMAN_MIN_INTERVAL_MS: u64 = 500;

    /// The sink `mode` implies, or `None` for "emit nothing" — which is both [`ProgressMode::Off`]
    /// and [`ProgressMode::Auto`] with a non-terminal stderr.
    ///
    /// ⚠ The terminal probe happens HERE, once, rather than per event: `IsTerminal` is a syscall,
    /// and asking it once per backtest would be a syscall per point for an answer that cannot change
    /// mid-process.
    pub fn for_mode(mode: ProgressMode) -> Option<Self> {
        match mode {
            ProgressMode::Off => None,
            ProgressMode::Json => Some(Self::json()),
            ProgressMode::Auto => std::io::stderr().is_terminal().then(Self::human),
        }
    }

    /// Newline-delimited JSON, every event.
    pub fn json() -> Self {
        StderrProgress { json: true, min_interval_ms: 0, last_ms: AtomicU64::new(u64::MAX) }
    }

    /// The throttled human line.
    pub fn human() -> Self {
        StderrProgress {
            json: false,
            min_interval_ms: Self::HUMAN_MIN_INTERVAL_MS,
            last_ms: AtomicU64::new(u64::MAX),
        }
    }

    /// Whether this event is due. **The FINAL event of a run with a known total always passes**, so
    /// the last line a human sees reports the finished count rather than whatever the throttle let
    /// through last — a progress line frozen at `61/64` above a printed report reads as a search
    /// that stopped early.
    fn due(&self, ev: &ProgressEvent) -> bool {
        if self.min_interval_ms == 0 {
            return true;
        }
        if ev.total.is_some_and(|t| ev.completed >= t) {
            return true;
        }
        let now = u64::try_from(ev.elapsed.as_millis()).unwrap_or(u64::MAX);
        let last = self.last_ms.load(Ordering::Relaxed);
        if last != u64::MAX && now.saturating_sub(last) < self.min_interval_ms {
            return false;
        }
        // ⚠ A plain store, not a compare-exchange: two workers finishing inside the same window can
        // both pass. That is a DUPLICATE LINE, which is the right failure for a progress display —
        // the alternative is a CAS loop in the path of every backtest to protect a cosmetic
        // property. Under `ParamscanExec::Sequential` (the determinism lever) it cannot happen at
        // all.
        self.last_ms.store(now, Ordering::Relaxed);
        true
    }
}

impl ProgressSink for StderrProgress {
    fn point(&self, ev: &ProgressEvent) {
        if !self.due(ev) {
            return;
        }
        if self.json {
            eprintln!("{}", ev.render_json());
        } else {
            eprintln!("{}", ev.render_human());
        }
    }
}

/// The counter, the clock and the running best behind one [`ProgressSink`] — armed on an evaluator,
/// never on an [`Optimizer`].
///
/// ⚠ **The evaluator is the only place this works for all four methods at once.** A hook on
/// `Optimizer::search` would have to be implemented four times, in four loops with four different
/// shapes, and a fifth method would ship with no progress until somebody remembered.
/// [`PointEvaluator::evaluate`] is the one funnel every candidate of every method passes through, in
/// both bar sources — so one arming site covers grid, euler, tpe, genetic and whatever comes fifth,
/// by construction.
struct ProgressTracker {
    sink: Box<dyn ProgressSink>,
    total: Option<u64>,
    started: Instant,
    completed: AtomicU64,
    /// The running best as raw bits, seeded with `NaN` = nothing rankable yet. Bits rather than a
    /// `Mutex<f64>` so the fold is lock-free in a rayon worker and there is no poisoning to handle
    /// when a backtest panics.
    best_bits: AtomicU64,
}

impl ProgressTracker {
    fn new(sink: Box<dyn ProgressSink>, total: Option<u64>) -> Self {
        ProgressTracker {
            sink,
            total,
            started: Instant::now(),
            completed: AtomicU64::new(0),
            best_bits: AtomicU64::new(f64::NAN.to_bits()),
        }
    }

    /// Fold one finished point in and emit.
    ///
    /// ⚠ `score` must ALREADY have been through [`apply_trade_floor`] — see [`observed`], the only
    /// caller, which enforces the order. A progress line built from the raw score would advertise a
    /// running best the floor has already disqualified, i.e. name a leader the printed report
    /// buries.
    fn observe(&self, score: f64) {
        let completed = self.completed.fetch_add(1, Ordering::Relaxed) + 1;
        // ⚠ The ONE ordering rule (`super::sweep::cmp_scores_desc`), never `f64::max`: `max`
        // returns the non-NaN operand, so it would let an unrankable score be treated as an
        // improvement on nothing and, worse, quietly install it as the incumbent best.
        let _ = self.best_bits.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bits| {
            match super::sweep::cmp_scores_desc(score, f64::from_bits(bits)) {
                std::cmp::Ordering::Less => Some(score.to_bits()),
                _ => None,
            }
        });
        let best = f64::from_bits(self.best_bits.load(Ordering::Relaxed));
        self.sink.point(&ProgressEvent {
            completed,
            total: self.total,
            elapsed: self.started.elapsed(),
            best: if best.is_nan() { None } else { Some(best) },
        });
    }
}

/// Floor, THEN observe — **the ONE post-evaluation site both shipped evaluators fold through**, so
/// the two cannot drift about the order.
///
/// ⚠ The order is the whole reason this is a function rather than two lines written twice. Observing
/// first would publish a running best the floor then disqualifies, and an operator watching a search
/// would see a leader that is absent from the report they are handed ninety seconds later.
fn observed(ev: Evaluated, floor: TradeFloor, progress: Option<&ProgressTracker>) -> Evaluated {
    let ev = apply_trade_floor(ev, floor);
    if let Some(p) = progress {
        p.observe(ev.score);
    }
    ev
}

#[path = "optimize_tests.rs"]
#[cfg(test)]
mod optimize_tests;
