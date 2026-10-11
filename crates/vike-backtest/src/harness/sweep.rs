//! Parameter-sweep expansion: turns a [`BacktestProfile`] with a `[paramscan]` table into the
//! cartesian product of single-run profiles, each with `strategy.params` overridden per point and
//! its own `paramscan` field cleared (so every expanded point is itself a plain, runnable
//! single-backtest profile). A profile with no `[paramscan]` table (or an empty one) expands to
//! exactly one point with no overrides — the harness's non-sweep path is unchanged.
//!
//! `run_paramscan`/[`ParamscanReport`]: runs every [`ParamscanPoint`] from [`expand_paramscan`]
//! through [`run_backtest`], collects a [`BacktestReport`] (or the error string) per point, and
//! ranks the successful rows by a [`RankMetric`] — the ranked table/JSON the `backtest` bin prints
//! for a profile with a `[paramscan]` table.
//!
//! Pluggable ranking (the objective seam): [`run_paramscan_with`] ranks the same rows by ANY
//! [`Objective`] (`crate::search::objective` — a higher-is-better scalar over each report, NaN
//! sorts last), stamping each successful row's `score`. The [`RankMetric`] variants are exposed as
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
//! `min(4, available_parallelism())`, overridable with the `preferences.sweep_threads` row a root
//! installs). Raising that row raises the multiplier — size it against the slice, not against the
//! core count.
//!
//! Escape hatch: [`ParamscanExec::Sequential`] forces the old one-at-a-time loop. Every public entry
//! point has an `_exec` twin taking it explicitly (what the determinism tests use, so they never
//! mutate process env), and the plain entry points default to [`ParamscanExec::from_env`] —
//! `VIKE_SWEEP_SEQUENTIAL=1` (the EXACT string `"1"`, the repo's env-gate idiom) pins the whole
//! process back to sequential. Below the `_exec` twins it is now an `optimize::StoreEvaluator`
//! CONSTRUCTOR argument, which is what let the determinism gates keep their lever.
//!
//! **Shared optimizer glue.** This module also owns the per-point plumbing the SEARCH lanes
//! (`search::euler`, `search::tpe`) fold through instead of re-rolling it: `sweep_axis_array` /
//! `numeric_sweep_axes` (the ONE `[sweep]`-table reader — array/non-empty for the grid, plus the
//! numeric/type-homogeneity narrowing for the searches), `profile_with_overrides` (base +
//! overrides → the single-run profile every point becomes), `eval_scored_point` (point → scored
//! [`ParamscanRow`] + the raw steering score), and `sort_scored_rows` (THE best-first ordering rule
//! over [`cmp_scores_desc`]). One implementation means the three optimizers cannot drift apart on
//! validation, evaluation, or ranking (regression-gated by
//! `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` in `search::euler`).
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

pub(crate) mod grid;
mod rank;
pub(crate) mod threads;

use rayon::prelude::*;

use super::optimize::{Candidate, Optimizer};
use super::{BacktestProfile, HarnessError, report, run};
use threads::install_bounded;

pub use grid::{
    GridSearch, cmp_scores_desc, run_paramscan, run_paramscan_exec, run_paramscan_with,
    run_paramscan_with_exec,
};
pub use rank::{ParamscanReport, ParamscanRow, RankBy, RankMetric, ReturnBuckets};
pub use threads::{SWEEP_SEQUENTIAL_ENV, install_sweep_threads, sweep_threads};

#[cfg(doc)]
use super::{optimize, run_backtest};
#[cfg(doc)]
use crate::search::objective::Objective;
#[cfg(doc)]
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
// `crates/vike-backtest/tests/walkforward_optimize.rs`) plus `walkforward/runner.rs`'s control
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
/// touch [`expand_paramscan`] or [`ParamscanPoint`] at all: those two are consumed OUTSIDE this
/// module (`crates/vike-backtest/tests/walkforward_optimize.rs`,
/// `crates/vike-studio-core/src/wire_run.rs`), and [`crate::walkforward::runner`]'s window closure
/// passes BOTH a point's `profile` and its `overrides` — so dropping `ParamscanPoint`'s profile
/// field would turn N profile builds done ONCE into N x `n_splits`. Strictly additive:
/// `expand_paramscan` is now this plus `profile_with_overrides` per point, which is the fold every
/// searched point already went through anyway.
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

#[cfg(test)]
#[cfg(feature = "datafusion-store")]
use std::sync::Arc;
#[cfg(test)]
use threads::{DEFAULT_SWEEP_THREADS, INSTALLED_SWEEP_THREADS, default_sweep_threads};
#[cfg(test)]
use vike_analytics::report::BacktestReport;
#[cfg(test)]
#[cfg(feature = "datafusion-store")]
use vike_data::HistStore;

#[path = "sweep_tests.rs"]
#[cfg(test)]
pub(crate) mod sweep_tests;
