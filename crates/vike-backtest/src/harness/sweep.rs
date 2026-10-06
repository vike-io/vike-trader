//! Parameter-sweep expansion (Task 1 of the sweep harness): turns a [`BacktestProfile`] with a
//! `[paramscan]` table into the cartesian product of single-run profiles, each with `strategy.params`
//! overridden per point and its own `paramscan` field cleared (so every expanded point is itself a
//! plain, runnable single-backtest profile). A profile with no `[paramscan]` table (or an empty one)
//! expands to exactly one point with no overrides — the harness's non-sweep path is unchanged.
//!
//! `run_paramscan`/[`ParamscanReport`] (Task 2): runs every [`ParamscanPoint`] from [`expand_paramscan`] through
//! [`run_backtest`], collects a [`BacktestReport`] (or the error string) per point, and ranks the
//! successful rows by a [`RankMetric`] — the ranked table/JSON the `backtest --sweep` bin (Task
//! 3, not implemented here) prints.
//!
//! Pluggable ranking (the objective seam): [`run_paramscan_with`] ranks the same rows by ANY
//! [`Objective`] (`crate::objective` — a higher-is-better scalar over each report, NaN sorts
//! last), stamping each successful row's `score`. The [`RankMetric`] variants are exposed as
//! built-in objective constructors ([`RankMetric::objective`], direction folded in) and are
//! ranking-equivalent to the classic path (regression-gated below); [`run_paramscan`] itself keeps
//! its original comparator untouched, so the default `--rank-by` ordering is byte-identical.
//!
//! **Parallel execution ([`ParamscanExec`]).** Every expanded [`ParamscanPoint`] is a fully independent
//! single-run profile over one shared `Arc<dyn HistStore>`, so the points are run on a rayon pool
//! by default. DETERMINISM IS NOT NEGOTIABLE HERE and does not rest on scheduling: rayon's
//! `par_iter().map().collect::<Vec<_>>()` is ORDER-PRESERVING, so a batch comes back in
//! `expand_paramscan` order regardless of which point finished first, and the ranking sort that follows
//! is `slice::sort_by` (stable) over that same input order — the identical bytes the one-at-a-time
//! loop produced (pinned by `parallel_and_sequential_sweeps_are_byte_identical`). ⚠ The fan-out
//! itself now lives one level down, in `optimize::PointEvaluator` (see the "Shared optimizer glue"
//! paragraph): every method inherits the same bound and the same order-preserving collect, and this
//! module keeps only [`map_bounded`], the cross-crate front door.
//!
//! **MEMORY: parallelism multiplies the data slice, and is therefore BOUNDED.** The shared
//! `Arc<dyn HistStore>` shares the store HANDLE, not the loaded data: every point calls
//! `run_backtest`, which independently re-scans and fully materializes its own `Vec<Bar>`/
//! `Vec<Tick>` slice. Peak RSS and store I/O are therefore `N_threads x slice`, NOT `1 x slice` —
//! on a large recorded tick slice an uncapped one-worker-per-logical-core sweep is an OOM shape,
//! and these sweeps run on boxes that also host live trading. So the points are NOT run on rayon's
//! GLOBAL pool: [`install_bounded`] builds a pool of [`sweep_threads`] workers (default
//! `min(4, available_parallelism())`, overridable with `VIKE_SWEEP_THREADS`). Raising that var
//! raises the multiplier — size it against the slice, not against the core count.
//!
//! Escape hatch: [`ParamscanExec::Sequential`] forces the old one-at-a-time loop. Every public entry
//! point has an `_exec` twin taking it explicitly (what the determinism tests use, so they never
//! mutate process env), and the plain entry points default to [`ParamscanExec::from_env`] —
//! `VIKE_SWEEP_SEQUENTIAL=1` (the EXACT string `"1"`, the repo's env-gate idiom) pins the whole
//! process back to sequential. Below the `_exec` twins it is now an `optimize::StoreEvaluator`
//! CONSTRUCTOR argument, which is what let the determinism gates keep their lever.
//!
//! **Shared optimizer glue.** This module also owns the per-point plumbing the SEARCH lanes
//! (`harness::euler`, `harness::tpe`) fold through instead of re-rolling it: `sweep_axis_array` /
//! `numeric_sweep_axes` (the ONE `[sweep]`-table reader — array/non-empty for the grid, plus the
//! numeric/type-homogeneity narrowing for the searches), `profile_with_overrides` (base +
//! overrides → the single-run profile every point becomes), `eval_scored_point` (point → scored
//! [`ParamscanRow`] + the raw steering score), and `sort_scored_rows` (THE best-first ordering rule
//! over [`cmp_scores_desc`]). One implementation means the three optimizers cannot drift apart on
//! validation, evaluation, or ranking (regression-gated by
//! `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` in `harness::euler`).
//!
//! **The trial MATRIX, and why it is 4 KB a trial rather than 160.** [`ReturnBuckets`] is the
//! opt-in (DISARMED by default, so nobody who did not ask pays anything) that makes
//! `row_from_outcome` keep a FIXED-SIZE bucketed return vector out of the equity curve it is
//! about to destroy, which is what `vike_analytics::overfit::pbo_cscv` and
//! `deflated_sharpe_with_effective_n` need and what `--keep-trials series` used to be refused for
//! wanting. The vectors travel out through `optimize::CapturedReturns` — a side channel on
//! `optimize::StoreEvaluator`, NOT a field on [`ParamscanRow`], because that row crosses the
//! datahub wire; that type's doc carries the argument, and
//! `bucketed_returns_cost_a_fortieth_of_a_curve` pins the cost arithmetic.
//!
//! ⚠ Since the optimizer seam landed, that glue is REACHED through `harness::optimize` rather than
//! called lane by lane: `optimize::StoreEvaluator` folds `profile_with_overrides` +
//! `eval_scored_point` into one infallible `evaluate` and owns the bounded pool, and
//! `optimize::report_from_outcome` owns `sort_scored_rows`. [`GridSearch`] is this module's own
//! method; the four public entry points below are ADAPTERS over that seam, and their output is
//! byte-identical (the existing suite is the equivalence gate).

use std::fmt;
use std::sync::Arc;

use rayon::prelude::*;
use vike_data::HistStore;

use crate::objective::Objective;

use super::optimize::{
    Candidate, Optimized, Optimizer, PointEvaluator, SearchOutcome, StoreEvaluator, optimize,
};
use super::{BacktestProfile, HarnessError, run_backtest};
use vike_analytics::report::BacktestReport;

/// How a sweep's independent points are executed. Purely an execution-strategy knob: BOTH variants
/// produce byte-identical reports (see the module doc) — this exists as an escape hatch and as the
/// determinism tests' lever, never as a behavior switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamscanExec {
    /// Run the points on the BOUNDED sweep pool ([`install_bounded`] / [`sweep_threads`] — never
    /// rayon's global pool), collecting results in input order.
    #[default]
    Parallel,
    /// Run the points one at a time on the calling thread (the pre-parallel path).
    Sequential,
}

/// The env var that pins a process back to [`ParamscanExec::Sequential`].
pub const SWEEP_SEQUENTIAL_ENV: &str = "VIKE_SWEEP_SEQUENTIAL";

/// The env var that caps how many sweep points run CONCURRENTLY (see [`sweep_threads`]).
pub const SWEEP_THREADS_ENV: &str = "VIKE_SWEEP_THREADS";

/// The default concurrency cap when [`SWEEP_THREADS_ENV`] is unset or unparsable. SMALL ON
/// PURPOSE, and deliberately NOT the core count: each concurrent point materializes its OWN copy of
/// the data slice (see the module doc's memory note), so this number IS the peak-RSS multiplier.
// ⚠ `pub(crate)` rather than private since stage 5: `crates/vike-backtest/src/backtest_cli.rs`'s
// `parse_keep_trials` REFUSES `--keep-trials series` citing this cap, and a refusal that typed the
// number instead would be a second spelling of it two files away — the shape
// `crates/vike-ops/tests/docs/one_authority_gate.rs` exists to stop. Not `pub`: the value is this
// crate's own spending decision and no consumer has any business reading it.
pub(crate) const DEFAULT_SWEEP_THREADS: usize = 4;

/// **The CALLER-OWNED cap** — the process-wide handle [`install_sweep_threads`] fills and
/// [`sweep_threads`] reads first. `OnceLock`, so it is written once by a composition root and never
/// changes under a running sweep.
static INSTALLED_SWEEP_THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// **Hand this process's sweep pool the cap its settings resolved** — the seam that makes
/// `preferences.sweep_threads` a setting rather than a declaration.
///
/// ⚠ **Why this shape and not a parameter.** The pool is entered from three front doors in two
/// crates (`optimize.rs`'s fan-out, `walkforward.rs`, and the public [`map_bounded`]
/// vike-studio-core takes), none of which carries a settings value and none of which a
/// composition root calls directly — so threading the number would mean a parameter on every
/// public sweep entry point plus the private glue under it. `vike_ops::settings`' module doc files
/// this under its family-2 STEP-2 items and names `init(spec)` + `OnceLock` as the shape; this is
/// that shape, for the one knob, with the root owning the read.
///
/// **`n` is the RESOLVED preference**, i.e. `vike_config::Preferences::sweep_threads` after
/// `apply_env` — which already folds `VIKE_SWEEP_THREADS` over the file value. That is why
/// [`sweep_threads`] consults this BEFORE the environment rather than after: consulting the
/// variable again would be a second authority over a value the loader has already decided, and on
/// a box that sets both it would answer the same thing twice or, after a future precedence change,
/// differently. `None` installs nothing, so a process whose file and environment both say nothing
/// keeps the compiled-in fallback.
///
/// Returns whether THIS call installed the value. A second call with a different number is a
/// composition-root bug (two roots in one process), so it is refused rather than honoured —
/// silently changing a bound under a pool that may already be built is the worse failure.
pub fn install_sweep_threads(n: Option<usize>) -> bool {
    match n {
        Some(n) if n > 0 => INSTALLED_SWEEP_THREADS.set(n).is_ok(),
        // A zero is refused by `vike_config::Preferences::apply` long before it reaches here (it
        // would install a pool that never runs a point); treating it as "install nothing" keeps
        // this function total for a caller that built its own value.
        _ => false,
    }
}

/// How many sweep points run concurrently: the cap a composition root
/// [`install_sweep_threads`]-ed, else `VIKE_SWEEP_THREADS` when it parses to a POSITIVE integer,
/// else `min(DEFAULT_SWEEP_THREADS, available_parallelism())`.
///
/// ⚠ The installed value wins over the environment, and that is not a precedence inversion: what a
/// root installs IS the resolved `env > file > default` answer (see [`install_sweep_threads`]).
/// The direct read below is what keeps a process that installs NOTHING — a one-shot `backtest` run
/// loads no settings at all — working exactly as it did before this seam existed.
///
/// Never returns zero — rayon reads `num_threads(0)` as "use the default", i.e. one worker per
/// logical core, which is exactly the unbounded shape this cap exists to prevent.
pub fn sweep_threads() -> usize {
    if let Some(n) = INSTALLED_SWEEP_THREADS.get() {
        return *n;
    }
    match std::env::var(SWEEP_THREADS_ENV).ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => default_sweep_threads(),
    }
}

/// The compiled-in fallback — `min(DEFAULT_SWEEP_THREADS, available_parallelism())`, with neither
/// lever consulted. Split out of [`sweep_threads`] so the cap can be asserted independently of the
/// process-wide handle: `install_sweep_threads` writes a `OnceLock`, test order is undefined, and a
/// default-cap test reading [`sweep_threads`] would otherwise pass or fail by scheduling.
fn default_sweep_threads() -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    DEFAULT_SWEEP_THREADS.min(cores)
}

/// Run `op` on a sweep-sized rayon pool ([`sweep_threads`] workers) rather than the GLOBAL pool —
/// the ONE place the sweep/euler lanes enter rayon, so the memory multiplier stays bounded.
///
/// Pool construction can fail (the OS refusing a thread spawn). That is a resource condition, not a
/// correctness one, so it is logged and `op` runs on the caller's current pool instead of aborting
/// the sweep: results are identical either way (order-preserving collect) — only the concurrency
/// bound is lost. It is never `unwrap`ped.
///
/// [`map_bounded`] is the public front door for callers OUTSIDE this crate (vike-studio-core's own
/// sweep) — they get the same bounded pool and the same order-preserving collect without taking a
/// rayon dependency of their own, so rayon is entered from exactly one place in the workspace.
pub(crate) fn install_bounded<T: Send>(op: impl FnOnce() -> T + Send) -> T {
    match rayon::ThreadPoolBuilder::new().num_threads(sweep_threads()).build() {
        Ok(pool) => pool.install(op),
        Err(e) => {
            tracing::warn!(
                threads = sweep_threads(),
                error = %e,
                "sweep thread pool build failed; falling back to the ambient rayon pool"
            );
            op()
        }
    }
}

/// Map `items` through `f` on the BOUNDED sweep pool ([`install_bounded`] / [`sweep_threads`]),
/// preserving INPUT order — the cross-crate entry point into this module's parallel lane.
///
/// Determinism is the whole point (module doc): `into_par_iter().map().collect::<Vec<_>>()`
/// reassembles by index, never by completion, so `map_bounded(xs, f)[i] == f(xs[i])` for every `i`
/// regardless of scheduling. A caller that then sorts stably over this output gets byte-identical
/// results from the parallel and the one-at-a-time paths.
///
/// Exists so a downstream crate whose sweep points are ALSO `N_threads x data-slice` in memory
/// (vike-studio-core's `run_paramscan_slice`) inherits the same cap without depending on rayon itself
/// — never use rayon's global pool for backtest points.
pub fn map_bounded<T, U>(items: Vec<T>, f: impl Fn(T) -> U + Send + Sync) -> Vec<U>
where
    T: Send,
    U: Send,
{
    install_bounded(move || items.into_par_iter().map(f).collect())
}

impl ParamscanExec {
    /// The process default: [`ParamscanExec::Parallel`] unless `VIKE_SWEEP_SEQUENTIAL` is the EXACT
    /// string `"1"` (the repo's env-gate idiom — never a fuzzy truthy parse).
    pub fn from_env() -> Self {
        match std::env::var(SWEEP_SEQUENTIAL_ENV) {
            Ok(v) if v == "1" => ParamscanExec::Sequential,
            _ => ParamscanExec::Parallel,
        }
    }
}

/// One point in a parameter sweep: the `(param name, value)` overrides applied on top of the
/// base profile's `strategy.params`, plus the resulting single-run [`BacktestProfile`] (its
/// `sweep` field is always `None` — it is ready to hand straight to the single-backtest runner).
#[derive(Debug, Clone)]
pub struct ParamscanPoint {
    pub overrides: Vec<(String, toml::Value)>,
    pub profile: BacktestProfile,
}

/// Expand `base`'s `[sweep]` table (if any) into the cartesian product of single-run profiles.
///
/// - No `[sweep]` table (or an empty one): one point, no overrides, `sweep` cleared.
/// - Otherwise: every `[sweep]` entry's value must be a non-empty TOML array (each array is one
///   axis of the grid); the cartesian product over all axes is returned in deterministic order
///   (axes sorted by key, values in their array order). Each point overrides the corresponding
///   `strategy.params` keys and clears `sweep`.
// ⚠ THE EARLY RETURN IS LOAD-BEARING AND IT IS NOT A SHORTCUT. A profile with no `[sweep]` table
// expands to ONE point that overrides nothing, and building it must not go through
// `profile_with_overrides` — which refuses a non-table `strategy.params`. Folding the two paths
// (the first draft of the seam did) makes this PUBLIC function REFUSE a profile it used to run,
// and `expand_paramscan` has out-of-crate consumers (`crates/vike-studio-core/src/wire_run.rs`,
// `crates/vike-backtest/tests/walkforward_optimize.rs`) plus `harness/walkforward.rs`'s control
// path. Three reviewers found it independently. The SEARCH path still refuses such a profile —
// `optimize`'s preflight is where that belongs — but expanding a one-point non-sweep does not.
pub fn expand_paramscan(base: &BacktestProfile) -> Result<Vec<ParamscanPoint>, HarnessError> {
    if !base.is_paramscan() {
        let mut profile = base.clone();
        profile.paramscan = None;
        return Ok(vec![ParamscanPoint { overrides: Vec::new(), profile }]);
    }
    expand_paramscan_overrides(base)?
        .into_iter()
        .map(|point| {
            let (profile, overrides) = profile_with_overrides(base, point)?;
            Ok(ParamscanPoint { overrides, profile })
        })
        .collect()
}

/// [`expand_paramscan`]'s ODOMETER half: the same cartesian product in the same deterministic order,
/// as bare [`Candidate`]s, WITHOUT the per-point [`BacktestProfile`] clone.
///
/// This is what [`GridSearch`] searches with, and it is why the optimizer seam did not have to
/// touch [`expand_paramscan`] or [`ParamscanPoint`] at all: those two are consumed OUTSIDE this module
/// (`crates/vike-backtest/tests/walkforward_optimize.rs`, `crates/vike-studio-core/src/wire_run.rs`),
/// and [`super::walkforward`]'s window closure passes BOTH a point's `profile` and its `overrides`
/// — so dropping `ParamscanPoint`'s profile field would turn N profile builds done ONCE into
/// N x `n_splits`. Strictly additive: `expand_paramscan` is now this plus `profile_with_overrides` per
/// point, which is the fold every searched point already went through anyway.
pub fn expand_paramscan_overrides(base: &BacktestProfile) -> Result<Vec<Candidate>, HarnessError> {
    if !base.is_paramscan() {
        return Ok(vec![Vec::new()]);
    }

    // Safe: is_paramscan() is true, so `paramscan` is Some.
    let sweep = base.paramscan.as_ref().expect("is_paramscan() guarantees Some");

    let mut keys: Vec<&String> = sweep.keys().collect();
    keys.sort();

    let mut axes: Vec<(String, Vec<toml::Value>)> = Vec::with_capacity(keys.len());
    for key in keys {
        axes.push((key.clone(), sweep_axis_array(key, &sweep[key])?.clone()));
    }

    let total: usize = axes.iter().map(|(_, vs)| vs.len()).product();
    let mut points = Vec::with_capacity(total);

    // Index odometer over the axes: idx[i] is the current index into axes[i].1.
    let mut idx = vec![0usize; axes.len()];
    for _ in 0..total {
        points.push(
            axes.iter()
                .zip(idx.iter())
                .map(|((k, values), &i)| (k.clone(), values[i].clone()))
                .collect::<Candidate>(),
        );

        // Advance the odometer (least-significant axis first).
        for i in (0..axes.len()).rev() {
            idx[i] += 1;
            if idx[i] < axes[i].1.len() {
                break;
            }
            idx[i] = 0;
        }
    }

    Ok(points)
}

/// Validate ONE `[sweep]` entry as a non-empty array of values — the array/non-empty rule EVERY
/// optimizer lane shares (previously copied into each lane's reader with diverging error strings).
/// The grid path keeps the raw `toml::Value`s it returns; `numeric_sweep_axes` narrows them
/// further for the search lanes.
fn sweep_axis_array<'a>(
    key: &str,
    value: &'a toml::Value,
) -> Result<&'a Vec<toml::Value>, HarnessError> {
    let arr = value.as_array().ok_or_else(|| {
        HarnessError::Validation(format!(
            "sweep.{key} must be an array of values to sweep over, got {value:?}"
        ))
    })?;
    if arr.is_empty() {
        return Err(HarnessError::Validation(format!("sweep.{key} must be a non-empty array")));
    }
    Ok(arr)
}

/// One `[sweep]` axis read as NUMERIC search coordinates — the shape the search lanes (euler, TPE)
/// consume, where the grid lane ([`expand_paramscan`]) keeps raw `toml::Value`s. `values` are the
/// axis's grid values in array order; `integral` records that the axis was all-integer TOML, so a
/// searched coordinate renders back as a TOML integer — never silently changing the type the
/// strategy receives versus the grid path.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NumericAxis {
    pub key: String,
    pub values: Vec<f64>,
    pub integral: bool,
}

/// Read `base`'s `[sweep]` table as numeric search axes, sorted by key — the SAME deterministic
/// axis order [`expand_paramscan`] uses, and the ONE array/non-empty/numeric/type-homogeneity
/// validation loop the search lanes share (previously triplicated across the euler and TPE
/// readers with diverging error strings). `search` names the lane (`"euler"`/`"tpe"`) in the
/// error messages.
///
/// Rules (each a [`HarnessError::Validation`]):
/// - a missing or empty `[sweep]` table: there is nothing to search;
/// - every entry must be a non-empty TOML array (`sweep_axis_array`, the grid lane's own rule);
/// - every value must be numeric — a string/bool axis has no notion of "between two values", so
///   the error names the GRID method ([`GridSearch`]'s own `name`) as the way to search it;
/// - each axis must be type-HOMOGENEOUS (all-integer or all-float): a MIXED `[1, 2.5]` axis has
///   no single TOML type to render coordinates back as, and picking one would make a searched
///   point deserialize differently than on the grid path (which clones each raw value) — a
///   strategy reading the param via `as_integer()` would behave differently under a search than
///   under the grid. Reject rather than silently diverge.
pub(crate) fn numeric_sweep_axes(
    base: &BacktestProfile,
    search: &str,
) -> Result<Vec<NumericAxis>, HarnessError> {
    // The METHOD an operator is sent to when this narrowing refuses their space. Read off
    // `GridSearch::name` rather than spelled here, so the refusal, the method and its
    // operator-facing name cannot drift into three literals (it used to name the retired `--search`
    // FLAG rather than a method).
    let grid = GridSearch.name();
    let Some(sweep) = base.paramscan.as_ref().filter(|_| base.is_paramscan()) else {
        return Err(HarnessError::Validation(format!(
            "{search} search needs a [sweep] table (a single-point profile has nothing to search)"
        )));
    };

    let mut keys: Vec<&String> = sweep.keys().collect();
    keys.sort();

    let mut axes = Vec::with_capacity(keys.len());
    for key in keys {
        let arr = sweep_axis_array(key, &sweep[key])?;

        let all_int = arr.iter().all(|v| v.as_integer().is_some());
        let all_float = arr.iter().all(|v| v.as_float().is_some());
        if !all_int
            && !all_float
            && arr.iter().all(|v| v.as_integer().is_some() || v.as_float().is_some())
        {
            return Err(HarnessError::Validation(format!(
                "sweep.{key} mixes integer and float values; {search} search must render every \
                 coordinate as one TOML type, which would change what the strategy receives — \
                 make the axis all-integer or all-float, or use the {grid} optimizer (--optimizer {grid})"
            )));
        }

        let mut values = Vec::with_capacity(arr.len());
        for v in arr {
            let x = v.as_integer().map(|i| i as f64).or_else(|| v.as_float()).ok_or_else(|| {
                HarnessError::Validation(format!(
                    "{search} search needs numeric sweep axes, but sweep.{key} contains {v:?} — \
                     use the {grid} optimizer (--optimizer {grid}) for non-numeric axes"
                ))
            })?;
            values.push(x);
        }

        axes.push(NumericAxis { key: key.clone(), values, integral: all_int });
    }

    Ok(axes)
}

/// The ONE spelling of the only fault [`profile_with_overrides`] can raise — a property of the BASE
/// profile, so every candidate over that profile fails identically and there is nothing per-point to
/// report. `optimize::require_overridable_params` refuses the same profile UP FRONT (which is what
/// makes `optimize::PointEvaluator::evaluate` infallible by type), so the two must never drift into
/// two literals an operator could see as two different messages.
pub(crate) const PARAMS_NOT_A_TABLE: &str = "strategy.params must be a table";

/// Build the single-run profile for one set of param `overrides`: `base` with `strategy.params`
/// overridden per `(key, value)` pair (in the given order) and its own `sweep` cleared — exactly
/// the shape every expanded/searched point takes, so it runs through the identical
/// [`run_backtest`] path regardless of which optimizer proposed it. The ONE profile builder the
/// grid expander and the search lanes' `profile_for` adapters all fold through; the overrides come
/// back untouched, paired with the profile, for the caller's [`ParamscanRow`].
pub(crate) fn profile_with_overrides(
    base: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> Result<(BacktestProfile, Vec<(String, toml::Value)>), HarnessError> {
    let mut profile = base.clone();
    profile.paramscan = None;
    let params = profile
        .strategy
        .params
        .as_table_mut()
        .ok_or_else(|| HarnessError::Validation(PARAMS_NOT_A_TABLE.into()))?;
    for (k, v) in &overrides {
        params.insert(k.clone(), v.clone());
    }
    Ok((profile, overrides))
}

/// Which [`BacktestReport`] metric ranks a sweep's rows, and in which direction "better" sorts.
/// Sharpe/`TotalReturn`/`FinalEquity` rank descending (bigger is better); `MaxDrawdown` ranks
/// ascending (a smaller drawdown is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RankMetric {
    #[default]
    Sharpe,
    TotalReturn,
    MaxDrawdown,
    FinalEquity,
}

impl RankMetric {
    /// Case-insensitive CLI parse: `"sharpe"|"return"|"max_dd"|"equity"`. Returns `None` for
    /// anything else — the caller (the sweep bin) turns that into a usage error.
    pub fn from_str_ci(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "sharpe" => Some(RankMetric::Sharpe),
            "return" => Some(RankMetric::TotalReturn),
            "max_dd" => Some(RankMetric::MaxDrawdown),
            "equity" => Some(RankMetric::FinalEquity),
            _ => None,
        }
    }

    /// The metric value a [`BacktestReport`] contributes for this ranking — the raw number,
    /// direction handled separately by `sort_key` (and, on the runtime path, folded into
    /// [`RankMetric::objective`] instead).
    fn value(self, report: &BacktestReport) -> f64 {
        match self {
            RankMetric::Sharpe => report.sharpe,
            RankMetric::TotalReturn => report.total_return,
            RankMetric::MaxDrawdown => report.max_drawdown,
            RankMetric::FinalEquity => report.final_equity,
        }
    }

    /// A sort key where SMALLER is better, for every metric — descending metrics get negated,
    /// `MaxDrawdown` (smaller is better) passes through as-is. A NaN key (e.g. a Sharpe over a
    /// zero-variance curve) is ordered LAST among successes by `cmp_reports`, so a degenerate point
    /// can never rank first.
    ///
    /// ⚠ TEST-ONLY since the optimizer seam landed. The runtime path ranks through
    /// [`RankMetric::objective`] + `optimize::report_from_outcome`; this pair survives ONLY as the
    /// oracle `metric_objectives_rank_identically_to_cmp_reports` compares that ordering against,
    /// which is what makes the retirement a proof rather than a claim.
    #[cfg(test)]
    fn sort_key(self, report: &BacktestReport) -> f64 {
        match self {
            RankMetric::MaxDrawdown => self.value(report),
            _ => -self.value(report),
        }
    }

    /// Compare two successful reports best-first for this metric. Unlike a raw
    /// `partial_cmp(...).unwrap_or(Equal)`, a NaN sort key always sorts LAST (worst) — never first —
    /// so a NaN-Sharpe grid point can't win a sweep and feed downstream selection (e.g. DSR).
    ///
    /// ⚠ TEST-ONLY — see `sort_key` above for why it is kept.
    #[cfg(test)]
    fn cmp_reports(self, a: &BacktestReport, b: &BacktestReport) -> std::cmp::Ordering {
        let (ka, kb) = (self.sort_key(a), self.sort_key(b));
        match (ka.is_nan(), kb.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater, // a's key is NaN → a is worse → sorts last
            (false, true) => std::cmp::Ordering::Less,    // b's key is NaN → b is worse
            (false, false) => ka.partial_cmp(&kb).expect("non-NaN f64s compare totally"),
        }
    }

    /// This metric's CLI/report name — the public twin of the internal [`RankMetric::label`], for
    /// callers (the `backtest` bin's euler path) that must label an objective-ranked report with
    /// the metric it was built from.
    pub fn name(self) -> &'static str {
        self.label()
    }

    fn label(self) -> &'static str {
        match self {
            RankMetric::Sharpe => "sharpe",
            RankMetric::TotalReturn => "return",
            RankMetric::MaxDrawdown => "max_dd",
            RankMetric::FinalEquity => "equity",
        }
    }

    /// This metric as a higher-is-better [`Objective`] — the built-in constructors of the
    /// objective seam. Direction is folded in (`MaxDrawdown` becomes `-max_drawdown`), so ranking
    /// by `self.objective()` through [`run_paramscan_with`] orders rows EXACTLY like the classic
    /// [`run_paramscan`] comparator, NaN-last included (see the
    /// `metric_objectives_rank_identically_to_cmp_reports` regression test).
    pub fn objective(self) -> Objective {
        match self {
            RankMetric::MaxDrawdown => Box::new(move |r: &BacktestReport| -self.value(r)),
            _ => Box::new(move |r: &BacktestReport| self.value(r)),
        }
    }
}

/// What ranked a [`ParamscanReport`]: a built-in [`RankMetric`] (the classic `--rank-by` names —
/// serializes as the same bare string as before, so default JSON is unchanged) or a named
/// [`Objective`] (serializes as its label, e.g. `"multi"`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum RankBy {
    Metric(RankMetric),
    Objective(String),
}

impl RankBy {
    /// The human label the report header prints: the metric's own label, or the objective's name.
    pub fn label(&self) -> &str {
        match self {
            RankBy::Metric(m) => m.label(),
            RankBy::Objective(name) => name,
        }
    }
}

/// One row of a [`ParamscanReport`]: the overrides that produced this point, plus either its
/// [`BacktestReport`] (success) or the stringified [`HarnessError`] (failure) — never both.
/// `score` is stamped only by the objective path ([`run_paramscan_with`]); the classic
/// [`run_paramscan`] leaves it `None`, which is skipped on serialization — so default `--json`
/// output is unchanged.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamscanRow {
    pub overrides: Vec<(String, toml::Value)>,
    pub report: Option<BacktestReport>,
    pub error: Option<String>,
    /// The [`Objective`] score of this row's report (higher is better), objective path only.
    /// A non-finite ("unrankable") score serializes as `null` — see [`ser_opt_score`].
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_score")]
    pub score: Option<f64>,
}

/// Serialize a stamped score, mapping a non-finite value (a NaN "unrankable" score) to JSON
/// `null` — `None` never reaches here (it is skipped at the field level).
///
/// NB serde_json already nulls non-finite floats on its own (only float MAP KEYS are an error
/// there), so this is not a rescue from a serialization failure: it PINS that shape as the
/// documented contract — `"score": null` means unrankable — independent of the serializer backend
/// (a format that hard-errors on non-finite floats would otherwise break `--json`).
fn ser_opt_score<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) if x.is_finite() => s.serialize_f64(*x),
        _ => s.serialize_none(),
    }
}

/// The ranked result of a whole sweep: every [`ParamscanPoint`] run through [`run_backtest`], sorted
/// best-first by `rank_by` (failed rows always sort last).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamscanReport {
    pub rows: Vec<ParamscanRow>,
    pub rank_by: RankBy,
    /// The METHOD's own cost line — euler's budget, tpe's trial line — or `None` for the grid,
    /// which says nothing about itself and never has.
    ///
    /// ⚠ **`skip_serializing_if` is load-bearing.** The grid path leaves this `None`, so a grid
    /// document is byte-identical to the one this crate emitted before the field existed — which is
    /// what lets `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
    /// `profile_sweep_is_byte_identical_local_and_remote` keep comparing bytes.
    ///
    /// ⚠ It exists because a REMOTE search had nowhere to report its cost. The engine binary prints
    /// `super::Optimized::summary` to stderr and always has; a client reading
    /// `vike_datahub_client::proto::Response::ParamscanReport` sees only this document, so before stage
    /// 7 a remote euler or tpe run reported its budget nowhere at all. `Display` deliberately does
    /// NOT render it — the engine prints it separately, and rendering it here too would double the
    /// line on the one surface that already had it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// **How many BUCKETS a trial's retained return vector holds — the opt-in that makes a trial
/// MATRIX possible at all, and `0` ([`ReturnBuckets::DISARMED`]) is the default.**
///
/// ⚠ **What this exists to dissolve.** A trial's equity curve is destroyed inside
/// `row_from_outcome`: it consumes the [`vike_analytics::BacktestResult`] to derive [`BacktestReport`]'s
/// scalars and drops `equity_curve`, `equity_ts`, `trades` and `per_symbol_curves`. So
/// `vike_analytics::overfit::pbo_cscv` and `deflated_sharpe_with_effective_n`, which need an
/// N-trial matrix of per-observation performance, had nothing to read — and
/// `crates/vike-backtest/src/backtest_cli.rs`'s `parse_keep_trials` refused
/// `--keep-trials series` by name, arguing that retaining curves raises peak RSS on a box whose
/// concurrency cap is already `DEFAULT_SWEEP_THREADS` because each point materialises its own
/// data slice.
///
/// ⚠ **The measurement that dissolves it: the statistic does not want the CURVE.** `pbo_cscv`
/// splits `T` observations into `n_splits` contiguous blocks and compares IN-sample against
/// OUT-of-sample block means, so it needs `T >= n_splits` — sixteen in practice
/// ([`super::optimize::DEFAULT_CSCV_SPLITS`]) — and nothing more. A FIXED-SIZE bucketed return
/// vector is therefore the same instrument at a fortieth of the cost, and the arithmetic is pinned
/// by `bucketed_returns_cost_a_fortieth_of_a_curve` rather than asserted here.
///
/// ⚠ **Why 512 and not 16, 64 or 20 000.** Three constraints meet at a power of two:
/// * `T` must clear `n_splits` with room for the blocks to be a SAMPLE rather than a point each —
///   at `T = 16` every CSCV block is ONE observation and a block mean is that observation, so the
///   in-sample/out-of-sample comparison measures single-bucket noise;
/// * `512 = 2^9` is divisible by every power-of-two split count up to itself, so
///   `pbo_cscv`'s `g * t / n_splits` bounds land on exact boundaries and no group is short —
///   a ragged final block weights one CSCV group differently from the rest;
/// * it is far below a real run's bar count, so bucketing is a genuine DOWNSAMPLE (each bucket
///   compounds many bars) rather than an upsample that would have to fabricate observations.
///
/// ⚠ **Bucketing does not deflate the significance test, and that is why `n_obs` may be `T`.**
/// A bucket return compounds `b` bar returns, so its Sharpe scales as `sr_bar * sqrt(b)` while the
/// PSR's own `sqrt(n - 1)` factor shrinks as `sqrt(n_bars / b)` — the product
/// `sr_per_obs * sqrt(n - 1)` is invariant under bucketing for iid returns. What DOES move is the
/// third and fourth moments (compounding pulls them toward Gaussian by the CLT), which is a
/// property of the aggregation FREQUENCY the deflated-Sharpe literature already treats as a
/// modelling choice — daily versus monthly — rather than an error.
///
/// A `Copy` newtype with a DISARMED constant rather than a bare `usize`, for exactly the reason
/// [`super::optimize::TradeFloor`] is one: it is a consuming-builder argument on
/// [`super::optimize::StoreEvaluator`], so "unchanged" is the value nobody has to write and an
/// unarmed search stays byte-identical BY CONSTRUCTION.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReturnBuckets(usize);

impl ReturnBuckets {
    /// Retain nothing. Byte-identical to this type never having existed — see the type doc.
    pub const DISARMED: ReturnBuckets = ReturnBuckets(0);

    /// The bucket count `--keep-trials returns` arms. The type doc argues the number.
    pub const DEFAULT_BUCKETS: usize = 512;

    /// [`ReturnBuckets::DEFAULT_BUCKETS`] buckets — what the CLI's opt-in installs.
    pub const DEFAULT: ReturnBuckets = ReturnBuckets(Self::DEFAULT_BUCKETS);

    /// `buckets` buckets. `0` builds [`ReturnBuckets::DISARMED`], so there is one meaning for "off".
    pub fn new(buckets: usize) -> Self {
        ReturnBuckets(buckets)
    }

    /// The requested count — what a document reports as its `T`.
    pub fn buckets(self) -> usize {
        self.0
    }

    /// Whether anything is retained at all.
    pub fn is_armed(self) -> bool {
        self.0 > 0
    }

    /// One trial's equity curve as AT MOST [`ReturnBuckets::buckets`] contiguous block returns, or
    /// `None` when this curve cannot produce an admissible column.
    ///
    /// ⚠ **Block boundaries, not a resample, and the difference is that this reads B+1 points
    /// instead of walking the curve.** The return of block `[a, b]` is `E[b] / E[a] - 1`, which is
    /// exactly the product of the per-bar returns inside it — so a one-million-bar curve is
    /// bucketed by 513 index reads and one divide each, and nothing proportional to the curve's
    /// length is allocated or scanned. `vike_model::runs::decimate` — what
    /// `crates/vike-backtest/src/backtest_cli.rs`'s `run_series_from` uses for a SINGLE run's
    /// record — SAMPLES instead, which is right for a picture of a curve and wrong here: a
    /// sampled point pair would drop the compounding between them.
    ///
    /// ⚠ **The effective count is `min(buckets, n_returns)`, so no bucket is ever EMPTY.** A
    /// shorter range yields a smaller `T` rather than a vector padded with fabricated zeros — a
    /// zero return is a real observation to every statistic downstream, and inventing 300 of them
    /// would move the PBO of a 200-bar run toward "no overfit" on evidence that does not exist.
    /// Every trial of one search runs the SAME `[data]` range (a `[paramscan]` axis overrides
    /// `strategy.params`, never the data window), so one search's columns share one `T` and the
    /// matrix is rectangular by construction; `super::optimize::overfit_stats` still refuses a
    /// column of a different length rather than trusting that.
    ///
    /// ⚠ **A non-finite block return is `None` for the WHOLE trial, not a `NaN` in the column.**
    /// `pbo_cscv` answers `NaN` if any cell anywhere is non-finite, so one trial whose equity
    /// touched exactly `0.0` at a boundary would otherwise take the statistic down for all 500.
    /// Refusing the column is the honest version of the same fact, and the excluded count is
    /// reported.
    pub(crate) fn capture(self, curve: &[f64]) -> Option<Vec<f64>> {
        if !self.is_armed() {
            return None;
        }
        // `n - 1` per-bar returns are available; a bucket needs at least one.
        let n = curve.len();
        if n < 2 {
            return None;
        }
        let span = n - 1;
        let t = self.0.min(span);
        let mut out = Vec::with_capacity(t);
        // `floor(k * span / t)` is STRICTLY increasing because `span / t >= 1`, so the boundaries
        // never repeat and no bucket spans zero bars.
        let mut prev = curve[0];
        for k in 1..=t {
            let idx = k * span / t;
            let cur = curve[idx];
            if prev == 0.0 {
                return None;
            }
            let r = cur / prev - 1.0;
            if !r.is_finite() {
                return None;
            }
            out.push(r);
            prev = cur;
        }
        Some(out)
    }
}

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
/// `super::walkforward`'s own window closure, which calls [`eval_scored_point_over_bars`]
/// directly, untouched.
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

/// Sort a scored sweep's rows in place, best-first — **the ONE ordering rule every score-ranked
/// lane shares** ([`run_paramscan_with`], euler, TPE, formerly three hand-rolled copies each annotated
/// "same ordering rule as `run_paramscan_with`"): score descending via [`cmp_scores_desc`] (higher is
/// better, NaN "unrankable" after finite), rows with no score (failures) last. `slice::sort_by`
/// is STABLE, so tied rows keep their input order — part of what keeps the parallel and
/// sequential execution paths byte-identical.
pub(crate) fn sort_scored_rows(rows: &mut [ParamscanRow]) {
    rows.sort_by(|a, b| match (a.score, b.score) {
        (Some(sa), Some(sb)) => cmp_scores_desc(sa, sb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
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

/// Render one row's `k=v` overrides joined by spaces, e.g. `size=2.0 threshold=0.1`. Empty for a
/// no-sweep single point.
fn format_overrides(overrides: &[(String, toml::Value)]) -> String {
    overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

impl fmt::Display for ParamscanReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "sweep ranked by {} ({} points)", self.rank_by.label(), self.rows.len())?;
        for (i, row) in self.rows.iter().enumerate() {
            let rank = if i == 0 { "*#1".to_string() } else { format!("#{}", i + 1) };
            let overrides = format_overrides(&row.overrides);
            match &row.report {
                Some(r) => {
                    write!(
                        f,
                        "{rank:<4} {overrides:<30} ret={:.4} sharpe={:.4} max_dd={:.4} trades={}",
                        r.total_return, r.sharpe, r.max_drawdown, r.n_trades
                    )?;
                    // Objective path only — the classic run_paramscan never stamps a score, so the
                    // default table stays byte-identical.
                    if let Some(score) = row.score {
                        write!(f, " score={score:.4}")?;
                    }
                    // A zero-trade / flat row (see `zero_trade::ZeroTradeReport::analyze`) surfaces
                    // its top probable cause inline. `zero_trade` is `None` for any row that traded
                    // or moved equity, so a normal sweep table is byte-identical.
                    if let Some(top) = r.zero_trade.as_ref().and_then(|zt| zt.causes.first()) {
                        write!(f, " -- {}", top.headline)?;
                    }
                    writeln!(f)?;
                }
                None => writeln!(
                    f,
                    "{rank:<4} {overrides:<30} FAILED: {}",
                    row.error.as_deref().unwrap_or("(unknown error)")
                )?,
            }
        }
        Ok(())
    }
}

#[path = "sweep_tests.rs"]
#[cfg(test)]
mod sweep_tests;
