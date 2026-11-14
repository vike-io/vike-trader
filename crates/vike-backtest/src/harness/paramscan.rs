//! Parameter-sweep expansion: turns a [`ProfileBacktest`] with a `[paramscan]` table into the
//! cartesian product of single-run profiles, each with `strategy.params` overridden per point and
//! its own `paramscan` field cleared (so every expanded point is itself a plain, runnable
//! single-backtest profile). A profile with no `[paramscan]` table (or an empty one) expands to
//! exactly one point with no overrides — the harness's non-sweep path is unchanged.
//!
//! `run_paramscan`/[`ParamscanReport`]: runs every [`ParamscanPoint`] from [`expand_paramscan`]
//! through [`run_backtest`], collects a [`BacktestReport`] (or the error string) per point, and
//! ranks the successful rows by a [`RankMetric`] — the ranked table/JSON the `backtesthub` bin prints
//! for a profile with a `[paramscan]` table.
//!
//! Pluggable ranking (the objective seam): [`run_paramscan_with`] ranks the same rows by ANY
//! [`SearchObjective`] (`crate::search::objective` — a higher-is-better scalar over each report, NaN
//! sorts last), stamping each successful row's `score`. The [`RankMetric`] variants are exposed as
//! built-in objective constructors ([`RankMetric::objective`], direction folded in) and are
//! ranking-equivalent to the classic path (regression-gated below); [`run_paramscan`] itself keeps
//! its original comparator untouched, so the default `--rank-by` ordering is byte-identical.
//!
//! **Parallel execution ([`ParamscanExecution`]).** Every expanded [`ParamscanPoint`] is a fully independent
//! single-run profile over one shared `Arc<dyn HistoryStore>`, so the points are run on a rayon pool
//! by default. DETERMINISM IS NOT NEGOTIABLE HERE and does not rest on scheduling: rayon's
//! `par_iter().map().collect::<Vec<_>>()` is ORDER-PRESERVING, so a batch comes back in
//! `expand_paramscan` order regardless of which point finished first, and the ranking sort that follows
//! is `slice::sort_by` (stable) over that same input order — the identical bytes the one-at-a-time
//! loop produced (pinned by `parallel_and_sequential_sweeps_are_byte_identical`). ⚠ The fan-out
//! itself now lives one level down, in `optimize::SearchEvaluator` (see the "Shared optimizer glue"
//! paragraph): every method inherits the same bound and the same order-preserving collect, and this
//! module keeps only [`map_bounded`], the cross-crate front door.
//!
//! **MEMORY: parallelism multiplies the data slice, and is therefore BOUNDED.** The shared
//! `Arc<dyn HistoryStore>` shares the store HANDLE, not the loaded data: every point calls
//! `run_backtest`, which independently re-scans and fully materializes its own `Vec<Bar>`/
//! `Vec<Tick>` slice. Peak RSS and store I/O are therefore `N_threads x slice`, NOT `1 x slice` —
//! on a large recorded tick slice an uncapped one-worker-per-logical-core sweep is an OOM shape,
//! and these sweeps run on boxes that also host live trading. So the points are NOT run on rayon's
//! GLOBAL pool: [`install_bounded`] builds a pool of [`paramscan_threads`] workers (default
//! `min(4, available_parallelism())`, overridable with the `preferences.paramscan_threads` row a root
//! installs). Raising that row raises the multiplier — size it against the slice, not against the
//! core count.
//!
//! Escape hatch: [`ParamscanExecution::Sequential`] forces the old one-at-a-time loop. Every public entry
//! point has an `_execution` twin taking it explicitly (what the determinism tests use, so they never
//! mutate process env), and the plain entry points default to [`ParamscanExecution::from_env`] —
//! `VIKE_PARAMSCAN_SEQUENTIAL=1` (the EXACT string `"1"`, the repo's env-gate idiom) pins the whole
//! process back to sequential. Below the `_execution` twins it is now an `optimize::SearchEvaluatorStore`
//! CONSTRUCTOR argument, which is what let the determinism gates keep their lever.
//!
//! **Shared optimizer glue.** This module also owns the per-point plumbing the SEARCH lanes
//! (`search::euler`, `search::tpe`) fold through instead of re-rolling it: `sweep_axis_array` /
//! `numeric_sweep_axes` (the ONE `[sweep]`-table reader — array/non-empty for the grid, plus the
//! numeric/type-homogeneity narrowing for the searches), `profile_with_overrides` (base +
//! overrides → the single-run profile every point becomes), `evaluation_scored_point` (point → scored
//! [`ParamscanRow`] + the raw steering score), and `sort_scored_rows` (THE best-first ordering rule
//! over `vike_analytics::metrics::cmp_scores_desc`). One implementation means the three optimizers cannot drift apart on
//! validation, evaluation, or ranking (regression-gated by
//! `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` in `search::euler`).
//!
//! **The trial MATRIX, and why it is 4 KB a trial rather than 160.** [`OverfitReturnBuckets`] is the
//! opt-in (DISARMED by default, so nobody who did not ask pays anything) that makes
//! `row_from_outcome` keep a FIXED-SIZE bucketed return vector out of the equity curve it is
//! about to destroy, which is what `vike_analytics::overfit::pbo_cscv` and
//! `deflated_sharpe_with_effective_n` need and what `--keep-trials series` used to be refused for
//! wanting. The vectors travel out through `optimize::OverfitCapturedReturns` — a side channel on
//! `optimize::SearchEvaluatorStore`, NOT a field on [`ParamscanRow`], because that row crosses the
//! datahub wire; that type's doc carries the argument, and
//! `bucketed_returns_cost_a_fortieth_of_a_curve` pins the cost arithmetic.
//!
//! ⚠ Since the optimizer seam landed, that glue is REACHED through `harness::optimize` rather than
//! called lane by lane: `optimize::SearchEvaluatorStore` folds `profile_with_overrides` +
//! `evaluation_scored_point` into one infallible `evaluate` and owns the bounded pool, and
//! `optimize::report_from_outcome` owns `sort_scored_rows`. [`SearchGrid`] is this module's own
//! method; the four public entry points below are ADAPTERS over that seam, and their output is
//! byte-identical (the existing suite is the equivalence gate).

pub(crate) mod grid;
mod rank;
pub mod threads;

use rayon::prelude::*;

use super::optimize::{Search, SearchCandidate};
use super::profile::SWEPT_KEYS;
use super::{HarnessError, ProfileBacktest, report, run};
use crate::search::SearchAxis;
use threads::install_bounded;
use vike_analytics::metrics::cmp_scores_desc;
use vike_model::params::ParamValue;

pub use grid::{
    SearchGrid, run_paramscan, run_paramscan_execution, run_paramscan_with,
    run_paramscan_with_execution,
};
pub use rank::{OverfitReturnBuckets, ParamscanReport, ParamscanRow, RankBy, RankMetric};
pub use threads::{PARAMSCAN_SEQUENTIAL_ENV, install_paramscan_threads, paramscan_threads};

#[cfg(doc)]
use super::{optimize, run_backtest};
#[cfg(doc)]
use crate::search::objective::SearchObjective;
#[cfg(doc)]
use vike_analytics::report::BacktestReport;

/// How a sweep's independent points are executed. Purely an execution-strategy knob: BOTH variants
/// produce byte-identical reports (see the module doc) — this exists as an escape hatch and as the
/// determinism tests' lever, never as a behavior switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParamscanExecution {
    /// Run the points on the BOUNDED sweep pool ([`install_bounded`] / [`paramscan_threads`] — never
    /// rayon's global pool), collecting results in input order.
    #[default]
    Parallel,
    /// Run the points one at a time on the calling thread (the pre-parallel path).
    Sequential,
}

/// Map `items` through `f` on the BOUNDED sweep pool ([`install_bounded`] / [`paramscan_threads`]),
/// preserving INPUT order — the cross-crate entry point into this module's parallel lane.
///
/// Determinism is the whole point (module doc): `into_par_iter().map().collect::<Vec<_>>()`
/// reassembles by index, never by completion, so `map_bounded(xs, f)[i] == f(xs[i])` for every `i`
/// regardless of scheduling. A caller that then sorts stably over this output gets byte-identical
/// results from the parallel and the one-at-a-time paths.
///
/// Exists so a caller whose points are ALSO `N_threads x data-slice` in memory (the walk-forward
/// window search, a crate outside this one) inherits the same cap without depending on rayon itself
/// — never use rayon's global pool for backtest points.
pub fn map_bounded<T, U>(items: Vec<T>, f: impl Fn(T) -> U + Send + Sync) -> Vec<U>
where
    T: Send,
    U: Send,
{
    install_bounded(move || items.into_par_iter().map(f).collect())
}

impl ParamscanExecution {
    /// The process default: [`ParamscanExecution::Parallel`] unless `VIKE_PARAMSCAN_SEQUENTIAL` is the EXACT
    /// string `"1"` (the repo's env-gate idiom — never a fuzzy truthy parse).
    pub fn from_env() -> Self {
        match std::env::var(PARAMSCAN_SEQUENTIAL_ENV) {
            Ok(v) if v == "1" => ParamscanExecution::Sequential,
            _ => ParamscanExecution::Parallel,
        }
    }
}

/// One point in a parameter sweep: the `(param name, value)` overrides applied on top of the
/// base profile's `strategy.params`, plus the resulting single-run [`ProfileBacktest`] (its
/// `sweep` field is always `None` — it is ready to hand straight to the single-backtest runner).
#[derive(Debug, Clone)]
pub struct ParamscanPoint {
    pub overrides: Vec<(String, ParamValue)>,
    pub profile: ProfileBacktest,
}

/// Expand `base`'s `[sweep]` table (if any) into the cartesian product of single-run profiles.
///
/// - No `[sweep]` table (or an empty one): one point, no overrides, `sweep` cleared.
/// - Otherwise: every `[sweep]` entry's value must be a non-empty TOML array (each array is one
///   axis of the grid); the cartesian product over all axes is returned in deterministic order
///   (axes sorted by key, values in their array order). Each point overrides the corresponding
///   `strategy.params` keys and clears `sweep`.
// The early return keeps the no-sweep point exactly the profile it was handed (with `paramscan`
// cleared) and builds it without `profile_with_overrides`. `expand_paramscan` has out-of-crate
// consumers (`crates/vike-studio-core/src/wire_run.rs`,
// `crates/vike-backtest/tests/walkforward_optimize.rs`) plus `walkforward/runner.rs`'s control
// path.
pub fn expand_paramscan(base: &ProfileBacktest) -> Result<Vec<ParamscanPoint>, HarnessError> {
    if !base.is_paramscan() {
        let mut profile = base.clone();
        profile.paramscan = None;
        return Ok(vec![ParamscanPoint { overrides: Vec::new(), profile }]);
    }
    Ok(expand_paramscan_overrides(base)?
        .into_iter()
        .map(|point| {
            let (profile, overrides) = profile_with_overrides(base, point);
            ParamscanPoint { overrides, profile }
        })
        .collect())
}

/// [`expand_paramscan`]'s ODOMETER half: the same cartesian product in the same deterministic order,
/// as bare [`SearchCandidate`]s, WITHOUT the per-point [`ProfileBacktest`] clone.
///
/// This is what [`SearchGrid`] searches with, and it is why the optimizer seam did not have to
/// touch [`expand_paramscan`] or [`ParamscanPoint`] at all: those two are consumed OUTSIDE this
/// module (`crates/vike-backtest/tests/walkforward_optimize.rs`,
/// `crates/vike-studio-core/src/wire_run.rs`), and [`crate::walkforward::runner`]'s window closure
/// passes BOTH a point's `profile` and its `overrides` — so dropping `ParamscanPoint`'s profile
/// field would turn N profile builds done ONCE into N x `n_splits`. Strictly additive:
/// `expand_paramscan` is now this plus `profile_with_overrides` per point, which is the fold every
/// searched point already went through anyway.
pub fn expand_paramscan_overrides(
    base: &ProfileBacktest,
) -> Result<Vec<SearchCandidate>, HarnessError> {
    if !base.is_paramscan() {
        return Ok(vec![Vec::new()]);
    }

    // Safe: is_paramscan() is true, so `paramscan` is Some.
    let sweep = base.paramscan.as_ref().expect("is_paramscan() guarantees Some");

    // `Params` iterates in key order: the axes come out sorted by key.
    let mut axes: Vec<(String, Vec<ParamValue>)> = Vec::with_capacity(sweep.len());
    for (key, value) in sweep {
        axes.push((key.clone(), sweep_axis_array(base, key, value)?.clone()));
    }

    let total: usize = axes.iter().map(|(_, vs)| vs.len()).product();
    let mut points = Vec::with_capacity(total);

    // Index odometer over the axes: idx[i] is the current index into axes[i].1.
    let mut index = vec![0usize; axes.len()];
    for _ in 0..total {
        points.push(
            axes.iter()
                .zip(index.iter())
                .map(|((k, values), &i)| (k.clone(), values[i].clone()))
                .collect::<SearchCandidate>(),
        );

        // Advance the odometer (least-significant axis first).
        for i in (0..axes.len()).rev() {
            index[i] += 1;
            if index[i] < axes[i].1.len() {
                break;
            }
            index[i] = 0;
        }
    }

    Ok(points)
}

/// Validate ONE `[sweep]` entry as a non-empty array of values — the array/non-empty rule EVERY
/// optimizer lane shares (previously copied into each lane's reader with diverging error strings).
/// The grid path keeps the raw [`ParamValue`]s it returns; `numeric_sweep_axes` narrows them
/// further for the search lanes.
///
/// A key is a `strategy.params` field, or one of the run's two slippage keys
/// (`engine.slippage.ticks`, `engine.slippage.value`): those vary `base`'s own `[engine.slippage]`,
/// whose model must be the one that owns the key, and every value must be a number. Any other
/// `engine.` key is refused: the grid opens the slippage alone.
fn sweep_axis_array<'a>(
    base: &ProfileBacktest,
    key: &str,
    value: &'a ParamValue,
) -> Result<&'a Vec<ParamValue>, HarnessError> {
    let arr = value.as_array().ok_or_else(|| {
        HarnessError::Validation(format!(
            "paramscan.{key} must be an array of values to scan over, got {value:?}"
        ))
    })?;
    if arr.is_empty() {
        return Err(HarnessError::Validation(format!("paramscan.{key} must be a non-empty array")));
    }
    if key.starts_with("engine.") {
        let Some(model) = run_axis_model(key) else {
            return Err(HarnessError::Validation(format!(
                "paramscan.{key}: a grid varies strategy params and, of the run's own keys, only \
                 engine.slippage.ticks and engine.slippage.value"
            )));
        };
        if base.engine.slippage.as_ref().is_none_or(|s| s.model != model) {
            return Err(HarnessError::Validation(format!(
                "paramscan.{key} varies the {model} slippage model, so the profile's [engine.slippage] \
                 must state model = \"{model}\" and its other keys"
            )));
        }
        if let Some(v) = arr.iter().find(|v| axis_number(v).is_none()) {
            return Err(HarnessError::Validation(format!(
                "paramscan.{key} must be numbers, and it holds {v:?}"
            )));
        }
    }
    Ok(arr)
}

/// The slippage model a run-side grid key ([`SWEPT_KEYS`]) varies, or `None` for any other key.
fn run_axis_model(key: &str) -> Option<&'static str> {
    SWEPT_KEYS.iter().find(|(k, _)| *k == key).map(|(_, model)| *model)
}

/// A grid value as a number: an integer or a float, never a word.
fn axis_number(v: &ParamValue) -> Option<f64> {
    v.as_integer().map(|i| i as f64).or_else(|| v.as_float())
}

/// Read `base`'s `[sweep]` table as numeric search axes, sorted by key — the SAME deterministic
/// axis order [`expand_paramscan`] uses, and the ONE array/non-empty/numeric/type-homogeneity
/// validation loop the search lanes share (previously triplicated across the euler and TPE
/// readers with diverging error strings). `search` names the lane (`"euler"`/`"tpe"`) in the
/// error messages.
///
/// Each axis's `values` are its grid values in array order, where the grid lane
/// ([`expand_paramscan`]) keeps raw [`ParamValue`]s; `integral` records that the axis was
/// all-integer TOML, so a searched coordinate renders back as a TOML integer — never silently
/// changing the type the strategy receives versus the grid path.
///
/// Rules (each a [`HarnessError::Validation`]):
/// - a missing or empty `[sweep]` table: there is nothing to search;
/// - every entry must be a non-empty TOML array (`sweep_axis_array`, the grid lane's own rule);
/// - every value must be numeric — a string/bool axis has no notion of "between two values", so
///   the error names the GRID method ([`SearchGrid`]'s own `name`) as the way to search it;
/// - each axis must be type-HOMOGENEOUS (all-integer or all-float): a MIXED `[1, 2.5]` axis has
///   no single TOML type to render coordinates back as, and picking one would make a searched
///   point deserialize differently than on the grid path (which clones each raw value) — a
///   strategy reading the param via `as_integer()` would behave differently under a search than
///   under the grid. Reject rather than silently diverge.
pub(crate) fn numeric_sweep_axes(
    base: &ProfileBacktest,
    search: &str,
) -> Result<Vec<SearchAxis>, HarnessError> {
    // The METHOD an operator is sent to when this narrowing refuses their space. Read off
    // `SearchGrid::name` rather than spelled here, so the refusal, the method and its
    // operator-facing name cannot drift into three literals (it used to name the retired `--search`
    // FLAG rather than a method).
    let grid = SearchGrid.name();
    let Some(sweep) = base.paramscan.as_ref().filter(|_| base.is_paramscan()) else {
        return Err(HarnessError::Validation(format!(
            "{search} search needs a [paramscan] table (a single-point profile has nothing to search)"
        )));
    };

    // `Params` iterates in key order: the same axis order `expand_paramscan_overrides` walks.
    let mut axes = Vec::with_capacity(sweep.len());
    for (key, value) in sweep {
        let arr = sweep_axis_array(base, key, value)?;

        let all_int = arr.iter().all(|v| v.as_integer().is_some());
        let all_float = arr.iter().all(|v| v.as_float().is_some());
        if !all_int
            && !all_float
            && arr.iter().all(|v| v.as_integer().is_some() || v.as_float().is_some())
        {
            return Err(HarnessError::Validation(format!(
                "paramscan.{key} mixes integer and float values; {search} search must render every \
                 coordinate as one TOML type, which would change what the strategy receives — \
                 make the axis all-integer or all-float, or use the {grid} optimizer (--optimizer {grid})"
            )));
        }

        let mut values = Vec::with_capacity(arr.len());
        for v in arr {
            let x = v.as_integer().map(|i| i as f64).or_else(|| v.as_float()).ok_or_else(|| {
                HarnessError::Validation(format!(
                    "{search} search needs numeric paramscan axes, but paramscan.{key} contains {v:?} — \
                     use the {grid} optimizer (--optimizer {grid}) for non-numeric axes"
                ))
            })?;
            values.push(x);
        }

        axes.push(SearchAxis { key: key.clone(), values, integral: all_int });
    }

    Ok(axes)
}

/// Build the single-run profile for one set of param `overrides`: `base` with `strategy.params`
/// overridden per `(key, value)` pair (in the given order) and its own `sweep` cleared — exactly
/// the shape every expanded/searched point takes, so it runs through the identical
/// [`run_backtest`] path regardless of which optimizer proposed it. The ONE profile builder the
/// grid expander and the search lanes' `profile_for` adapters all fold through; the overrides come
/// back untouched, paired with the profile, for the caller's [`ParamscanRow`].
///
/// A run-side key ([`SWEPT_KEYS`]: `engine.slippage.ticks`, `engine.slippage.value`) sets that key
/// of the profile's own `[engine.slippage]` instead, and never reaches the strategy; the axis reader
/// (`sweep_axis_array`) has already held its model and its numbers.
///
/// INFALLIBLE BY TYPE: `strategy.params` is a `vike_model::params::Params` table, so there is no
/// non-table bag to refuse here — a profile whose `[strategy.params]` is not a table never loads
/// (`ProfileBacktest`'s serde read refuses it). This is what makes
/// `optimize::SearchEvaluator::evaluate` infallible. A run-side value that is not a number (no axis
/// reader let one through) becomes NaN, which the slippage table's own check refuses as the point's
/// failure.
pub fn profile_with_overrides(
    base: &ProfileBacktest,
    overrides: Vec<(String, ParamValue)>,
) -> (ProfileBacktest, Vec<(String, ParamValue)>) {
    let mut profile = base.clone();
    profile.paramscan = None;
    let (run_side, params): (Vec<_>, Vec<_>) =
        overrides.iter().cloned().partition(|(k, _)| run_axis_model(k).is_some());
    profile.strategy = base.strategy.with_overrides(&params);
    if let Some(slippage) = profile.engine.slippage.as_mut() {
        for (key, v) in &run_side {
            slippage.set_swept(key, axis_number(v).unwrap_or(f64::NAN));
        }
    }
    (profile, overrides)
}

/// Sort a scored sweep's rows in place, best-first — **the ONE ordering rule every score-ranked
/// lane shares** ([`run_paramscan_with`], euler, TPE, formerly three hand-rolled copies each annotated
/// "same ordering rule as `run_paramscan_with`"): score descending via `vike_analytics::metrics::cmp_scores_desc` (higher is
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
use std::sync::Arc;
#[cfg(test)]
use threads::{DEFAULT_PARAMSCAN_THREADS, INSTALLED_SWEEP_THREADS, default_sweep_threads};
#[cfg(test)]
use vike_analytics::report::BacktestReport;
#[cfg(test)]
use vike_data::HistoryStore;

#[path = "paramscan_tests.rs"]
#[cfg(test)]
pub(crate) mod paramscan_tests;
