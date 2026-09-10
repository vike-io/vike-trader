//! Parameter-sweep expansion (Task 1 of the sweep harness): turns a [`BacktestProfile`] with a
//! `[sweep]` table into the cartesian product of single-run profiles, each with `strategy.params`
//! overridden per point and its own `sweep` field cleared (so every expanded point is itself a
//! plain, runnable single-backtest profile). A profile with no `[sweep]` table (or an empty one)
//! expands to exactly one point with no overrides — the harness's non-sweep path is unchanged.
//!
//! `run_sweep`/[`SweepReport`] (Task 2): runs every [`SweepPoint`] from [`expand_sweep`] through
//! [`run_backtest`], collects a [`BacktestReport`] (or the error string) per point, and ranks the
//! successful rows by a [`RankMetric`] — the ranked table/JSON the `backtest --sweep` bin (Task
//! 3, not implemented here) prints.
//!
//! Pluggable ranking (the objective seam): [`run_sweep_with`] ranks the same rows by ANY
//! [`Objective`] (`crate::objective` — a higher-is-better scalar over each report, NaN sorts
//! last), stamping each successful row's `score`. The [`RankMetric`] variants are exposed as
//! built-in objective constructors ([`RankMetric::objective`], direction folded in) and are
//! ranking-equivalent to the classic path (regression-gated below); [`run_sweep`] itself keeps
//! its original comparator untouched, so the default `--rank-by` ordering is byte-identical.
//!
//! **Parallel execution ([`SweepExec`]).** Every expanded [`SweepPoint`] is a fully independent
//! single-run profile over one shared `Arc<dyn HistStore>`, so the points are run on a rayon pool
//! by default. DETERMINISM IS NOT NEGOTIABLE HERE and does not rest on scheduling: rayon's
//! `par_iter().map().collect::<Vec<_>>()` is ORDER-PRESERVING, so `run_rows` yields rows in
//! `expand_sweep` order regardless of which point finished first, and the ranking sort that follows
//! is `slice::sort_by` (stable) over that same input order — the identical bytes the one-at-a-time
//! loop produced (pinned by `parallel_and_sequential_sweeps_are_byte_identical`).
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
//! Escape hatch: [`SweepExec::Sequential`] forces the old one-at-a-time loop. Every public entry
//! point has an `_exec` twin taking it explicitly (what the determinism tests use, so they never
//! mutate process env), and the plain entry points default to [`SweepExec::from_env`] —
//! `VIKE_SWEEP_SEQUENTIAL=1` (the EXACT string `"1"`, the repo's env-gate idiom) pins the whole
//! process back to sequential.
//!
//! **Shared optimizer glue.** This module also owns the per-point plumbing the SEARCH lanes
//! (`harness::euler`, `harness::tpe`) fold through instead of re-rolling it: `sweep_axis_array` /
//! `numeric_sweep_axes` (the ONE `[sweep]`-table reader — array/non-empty for the grid, plus the
//! numeric/type-homogeneity narrowing for the searches), `profile_with_overrides` (base +
//! overrides → the single-run profile every point becomes), `eval_scored_point` (point → scored
//! [`SweepRow`] + the raw steering score), and `sort_scored_rows` (THE best-first ordering rule
//! over [`cmp_scores_desc`]). One implementation means the three optimizers cannot drift apart on
//! validation, evaluation, or ranking (regression-gated by
//! `euler_and_tpe_rank_rows_exactly_like_run_sweep_with` in `harness::euler`).

use std::fmt;
use std::sync::Arc;

use rayon::prelude::*;
use vike_data::HistStore;

use crate::objective::Objective;

use super::{BacktestProfile, BacktestReport, HarnessError, run_backtest};

/// How a sweep's independent points are executed. Purely an execution-strategy knob: BOTH variants
/// produce byte-identical reports (see the module doc) — this exists as an escape hatch and as the
/// determinism tests' lever, never as a behavior switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SweepExec {
    /// Run the points on the BOUNDED sweep pool ([`install_bounded`] / [`sweep_threads`] — never
    /// rayon's global pool), collecting results in input order.
    #[default]
    Parallel,
    /// Run the points one at a time on the calling thread (the pre-parallel path).
    Sequential,
}

/// The env var that pins a process back to [`SweepExec::Sequential`].
pub const SWEEP_SEQUENTIAL_ENV: &str = "VIKE_SWEEP_SEQUENTIAL";

/// The env var that caps how many sweep points run CONCURRENTLY (see [`sweep_threads`]).
pub const SWEEP_THREADS_ENV: &str = "VIKE_SWEEP_THREADS";

/// The default concurrency cap when [`SWEEP_THREADS_ENV`] is unset or unparsable. SMALL ON
/// PURPOSE, and deliberately NOT the core count: each concurrent point materializes its OWN copy of
/// the data slice (see the module doc's memory note), so this number IS the peak-RSS multiplier.
const DEFAULT_SWEEP_THREADS: usize = 4;

/// How many sweep points run concurrently: `VIKE_SWEEP_THREADS` when it parses to a POSITIVE
/// integer, else `min(DEFAULT_SWEEP_THREADS, available_parallelism())`.
///
/// Never returns zero — rayon reads `num_threads(0)` as "use the default", i.e. one worker per
/// logical core, which is exactly the unbounded shape this cap exists to prevent.
pub fn sweep_threads() -> usize {
    match std::env::var(SWEEP_THREADS_ENV).ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            DEFAULT_SWEEP_THREADS.min(cores)
        }
    }
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
/// (vike-studio-core's `run_sweep_slice`) inherits the same cap without depending on rayon itself
/// — never use rayon's global pool for backtest points.
pub fn map_bounded<T, U>(items: Vec<T>, f: impl Fn(T) -> U + Send + Sync) -> Vec<U>
where
    T: Send,
    U: Send,
{
    install_bounded(move || items.into_par_iter().map(f).collect())
}

impl SweepExec {
    /// The process default: [`SweepExec::Parallel`] unless `VIKE_SWEEP_SEQUENTIAL` is the EXACT
    /// string `"1"` (the repo's env-gate idiom — never a fuzzy truthy parse).
    pub fn from_env() -> Self {
        match std::env::var(SWEEP_SEQUENTIAL_ENV) {
            Ok(v) if v == "1" => SweepExec::Sequential,
            _ => SweepExec::Parallel,
        }
    }
}

/// One point in a parameter sweep: the `(param name, value)` overrides applied on top of the
/// base profile's `strategy.params`, plus the resulting single-run [`BacktestProfile`] (its
/// `sweep` field is always `None` — it is ready to hand straight to the single-backtest runner).
#[derive(Debug, Clone)]
pub struct SweepPoint {
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
pub fn expand_sweep(base: &BacktestProfile) -> Result<Vec<SweepPoint>, HarnessError> {
    if !base.is_sweep() {
        let mut profile = base.clone();
        profile.sweep = None;
        return Ok(vec![SweepPoint { overrides: Vec::new(), profile }]);
    }

    // Safe: is_sweep() is true, so `sweep` is Some.
    let sweep = base.sweep.as_ref().expect("is_sweep() guarantees Some");

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
        let overrides = axes
            .iter()
            .zip(idx.iter())
            .map(|((k, values), &i)| (k.clone(), values[i].clone()))
            .collect();
        let (profile, overrides) = profile_with_overrides(base, overrides)?;
        points.push(SweepPoint { overrides, profile });

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
/// consume, where the grid lane ([`expand_sweep`]) keeps raw `toml::Value`s. `values` are the
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
/// axis order [`expand_sweep`] uses, and the ONE array/non-empty/numeric/type-homogeneity
/// validation loop the search lanes share (previously triplicated across the euler and TPE
/// readers with diverging error strings). `search` names the lane (`"euler"`/`"tpe"`) in the
/// error messages.
///
/// Rules (each a [`HarnessError::Validation`]):
/// - a missing or empty `[sweep]` table: there is nothing to search;
/// - every entry must be a non-empty TOML array (`sweep_axis_array`, the grid lane's own rule);
/// - every value must be numeric — a string/bool axis has no notion of "between two values", so
///   the error points the caller at `--search grid`;
/// - each axis must be type-HOMOGENEOUS (all-integer or all-float): a MIXED `[1, 2.5]` axis has
///   no single TOML type to render coordinates back as, and picking one would make a searched
///   point deserialize differently than on the grid path (which clones each raw value) — a
///   strategy reading the param via `as_integer()` would behave differently under a search than
///   under `--search grid`. Reject rather than silently diverge.
pub(crate) fn numeric_sweep_axes(
    base: &BacktestProfile,
    search: &str,
) -> Result<Vec<NumericAxis>, HarnessError> {
    let Some(sweep) = base.sweep.as_ref().filter(|_| base.is_sweep()) else {
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
                 make the axis all-integer or all-float, or use --search grid"
            )));
        }

        let mut values = Vec::with_capacity(arr.len());
        for v in arr {
            let x = v.as_integer().map(|i| i as f64).or_else(|| v.as_float()).ok_or_else(|| {
                HarnessError::Validation(format!(
                    "{search} search needs numeric sweep axes, but sweep.{key} contains {v:?} — \
                     use --search grid for non-numeric axes"
                ))
            })?;
            values.push(x);
        }

        axes.push(NumericAxis { key: key.clone(), values, integral: all_int });
    }

    Ok(axes)
}

/// Build the single-run profile for one set of param `overrides`: `base` with `strategy.params`
/// overridden per `(key, value)` pair (in the given order) and its own `sweep` cleared — exactly
/// the shape every expanded/searched point takes, so it runs through the identical
/// [`run_backtest`] path regardless of which optimizer proposed it. The ONE profile builder the
/// grid expander and the search lanes' `profile_for` adapters all fold through; the overrides come
/// back untouched, paired with the profile, for the caller's [`SweepRow`].
pub(crate) fn profile_with_overrides(
    base: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> Result<(BacktestProfile, Vec<(String, toml::Value)>), HarnessError> {
    let mut profile = base.clone();
    profile.sweep = None;
    let params = profile
        .strategy
        .params
        .as_table_mut()
        .ok_or_else(|| HarnessError::Validation("strategy.params must be a table".into()))?;
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
    /// direction handled separately by [`RankMetric::sort_key`].
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
    /// zero-variance curve) is ordered LAST among successes by [`RankMetric::cmp_reports`], so a
    /// degenerate point can never rank first.
    fn sort_key(self, report: &BacktestReport) -> f64 {
        match self {
            RankMetric::MaxDrawdown => self.value(report),
            _ => -self.value(report),
        }
    }

    /// Compare two successful reports best-first for this metric. Unlike a raw
    /// `partial_cmp(...).unwrap_or(Equal)`, a NaN sort key always sorts LAST (worst) — never first —
    /// so a NaN-Sharpe grid point can't win a sweep and feed downstream selection (e.g. DSR).
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
    /// by `self.objective()` through [`run_sweep_with`] orders rows EXACTLY like the classic
    /// [`run_sweep`] comparator, NaN-last included (see the
    /// `metric_objectives_rank_identically_to_cmp_reports` regression test).
    pub fn objective(self) -> Objective {
        match self {
            RankMetric::MaxDrawdown => Box::new(move |r: &BacktestReport| -self.value(r)),
            _ => Box::new(move |r: &BacktestReport| self.value(r)),
        }
    }
}

/// What ranked a [`SweepReport`]: a built-in [`RankMetric`] (the classic `--rank-by` names —
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

/// One row of a [`SweepReport`]: the overrides that produced this point, plus either its
/// [`BacktestReport`] (success) or the stringified [`HarnessError`] (failure) — never both.
/// `score` is stamped only by the objective path ([`run_sweep_with`]); the classic
/// [`run_sweep`] leaves it `None`, which is skipped on serialization — so default `--json`
/// output is unchanged.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepRow {
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

/// The ranked result of a whole sweep: every [`SweepPoint`] run through [`run_backtest`], sorted
/// best-first by `rank_by` (failed rows always sort last).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SweepReport {
    pub rows: Vec<SweepRow>,
    pub rank_by: RankBy,
}

/// Run ONE point's single-run profile through [`run_backtest`] into its (unscored) report row —
/// the shared row builder EVERY optimizer lane (grid, euler, TPE) folds through. Pure per-point
/// work over shared-by-reference inputs (`&BacktestProfile`, `&Arc<dyn HistStore>`) — nothing here
/// touches state another point can see, which is what makes the points safely parallelizable.
/// A failure is recorded as this row's `error`, never propagated. `score` stays `None`: the
/// classic [`run_sweep`] never stamps one, and the search lanes stamp theirs on top via
/// [`eval_scored_point`].
fn point_row(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> SweepRow {
    row_from_outcome(base, profile, overrides, run_backtest(profile, store.clone()))
}

/// [`point_row`] over bars the CALLER already holds — the walk-forward window's per-candidate unit.
///
/// Same row, same report, same failure rule; only the bar source differs. See
/// [`super::run::run_backtest_over_bars`] for why a window cannot use the store-driven spelling:
/// it would reload the profile's whole range once per candidate, and the window's slice is an
/// INDEX range that `crate::walkforward::walk_forward_strategy` cannot express as a store range.
fn point_row_over_bars(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    bars: Vec<(String, Vec<vike_model::Bar>)>,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> SweepRow {
    let outcome = super::run::run_backtest_over_bars(profile, store, bars);
    row_from_outcome(base, profile, overrides, outcome)
}

/// The ONE row builder both spellings above fold through — the report composition, the
/// annualization and the failure-is-a-row-not-an-error rule live here once. Split out when the
/// bars-in twin landed, precisely so the two sources could never disagree about what a sweep row
/// MEANS; before that this body was `point_row`'s whole match.
fn row_from_outcome(
    base: &BacktestProfile,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
    outcome: Result<crate::BacktestResult, HarnessError>,
) -> SweepRow {
    match outcome {
        Ok(r) => SweepRow {
            overrides,
            report: Some(BacktestReport::from_result(
                base.name.clone(),
                &r,
                super::report::periods_per_year(profile),
            )),
            error: None,
            score: None,
        },
        Err(e) => SweepRow { overrides, report: None, error: Some(e.to_string()), score: None },
    }
}

/// [`point_row`] over an expanded [`SweepPoint`] — the grid lanes' per-point unit.
fn run_point(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    point: SweepPoint,
) -> SweepRow {
    let SweepPoint { overrides, profile } = point;
    point_row(base, store, &profile, overrides)
}

/// Evaluate ONE searched point (an already-built single-run `profile` plus its `overrides`) into
/// its SCORED report row and the raw steering score — **the ONE evaluate-a-point block the search
/// lanes (euler, TPE) share** instead of each hand-rolling the `run_backtest` → report →
/// objective fold (they were line-for-line twins). A successful backtest stamps
/// `score: Some(objective(&report))`; a failed one keeps the row's `score` at `None` (the
/// report/JSON shape is unchanged) and returns `NaN` as the steering score — a failed point is
/// unrankable and must never become a refinement centre or steer the model.
pub(crate) fn eval_scored_point(
    base: &BacktestProfile,
    store: &Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    profile: &BacktestProfile,
    overrides: Vec<(String, toml::Value)>,
) -> (SweepRow, f64) {
    score_row(point_row(base, store, profile, overrides), objective)
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
) -> (SweepRow, f64) {
    score_row(point_row_over_bars(base, store, bars, profile, overrides), objective)
}

/// Stamp a row's steering score — the ONE place the "a failed point is unrankable" rule is
/// written, shared by both `eval_scored_point*` spellings. A successful backtest carries
/// `score: Some(objective(&report))`; a failure keeps `score: None` (the report/JSON shape is
/// unchanged) and steers with `NaN`, which `cmp_scores_desc` sorts LAST — so a failed point can
/// never become a refinement centre, steer a model, or win a walk-forward window.
fn score_row(mut row: SweepRow, objective: &Objective) -> (SweepRow, f64) {
    match row.report.as_ref() {
        Some(report) => {
            let score = objective(report);
            row.score = Some(score);
            (row, score)
        }
        None => (row, f64::NAN),
    }
}

/// Expand `base`'s `[sweep]` table and run every point through [`run_backtest`], unranked —
/// the shared front half of [`run_sweep`] and [`run_sweep_with`] (each then sorts with its own
/// comparator). A per-point failure does NOT fail the whole sweep — it is recorded as that row's
/// `error` so the report still shows every point (and how many/which failed).
///
/// Rows come back in `expand_sweep` order under BOTH [`SweepExec`] variants: `par_iter().map()
/// .collect::<Vec<_>>()` preserves input order (rayon reassembles by index, not by completion),
/// so the caller's sort sees the exact sequence the sequential loop produced.
///
/// The parallel branch runs on the BOUNDED sweep pool ([`install_bounded`]), never the global one:
/// concurrency here is a `N_threads x data-slice` memory multiplier (module doc).
fn run_rows(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    exec: SweepExec,
) -> Result<Vec<SweepRow>, HarnessError> {
    let points = expand_sweep(base)?;

    let rows: Vec<SweepRow> = if exec == SweepExec::Sequential {
        points.into_iter().map(|p| run_point(base, &store, p)).collect()
    } else {
        install_bounded(|| -> Vec<SweepRow> {
            points.into_par_iter().map(|p| run_point(base, &store, p)).collect()
        })
    };
    Ok(rows)
}

/// Expand `base`'s `[sweep]` table and run every resulting point through [`run_backtest`],
/// ranking the successful rows by `rank_by` (best first) with failed rows sorted last.
///
/// The ORIGINAL ranking path, comparator untouched ([`RankMetric::cmp_reports`]) — default
/// `--rank-by` behavior stays byte-identical. For a custom objective use [`run_sweep_with`].
pub fn run_sweep(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    rank_by: RankMetric,
) -> Result<SweepReport, HarnessError> {
    run_sweep_exec(base, store, rank_by, SweepExec::from_env())
}

/// [`run_sweep`] with the execution strategy passed explicitly instead of read from the env — the
/// determinism gate's lever. Both [`SweepExec`] variants return byte-identical reports.
pub fn run_sweep_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    rank_by: RankMetric,
    exec: SweepExec,
) -> Result<SweepReport, HarnessError> {
    let mut rows = run_rows(base, store, exec)?;

    rows.sort_by(|a, b| match (&a.report, &b.report) {
        (Some(ra), Some(rb)) => rank_by.cmp_reports(ra, rb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    Ok(SweepReport { rows, rank_by: RankBy::Metric(rank_by) })
}

/// **The ONE best-first ranking comparator over raw f64 scores** — every score-ranked sort in and
/// out of this crate (objective sweeps, TPE, euler, vike-studio-core's Sharpe rank) goes through
/// it rather than hand-rolling `partial_cmp(..).unwrap_or(Equal)`, the shape that lets a NaN
/// compare Equal to everything and keep whatever position it started in. Higher is better; a NaN
/// ("unrankable") score sorts LAST among successes — the objective-path twin of
/// [`RankMetric::cmp_reports`]'s NaN rule, so a degenerate point can never rank first here either.
pub fn cmp_scores_desc(a: f64, b: f64) -> std::cmp::Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => std::cmp::Ordering::Equal,
        (true, false) => std::cmp::Ordering::Greater, // a unrankable → a is worse → sorts last
        (false, true) => std::cmp::Ordering::Less,
        (false, false) => b.partial_cmp(&a).expect("non-NaN f64s compare totally"),
    }
}

/// Sort a scored sweep's rows in place, best-first — **the ONE ordering rule every score-ranked
/// lane shares** ([`run_sweep_with`], euler, TPE, formerly three hand-rolled copies each annotated
/// "same ordering rule as `run_sweep_with`"): score descending via [`cmp_scores_desc`] (higher is
/// better, NaN "unrankable" after finite), rows with no score (failures) last. `slice::sort_by`
/// is STABLE, so tied rows keep their input order — part of what keeps the parallel and
/// sequential execution paths byte-identical.
pub(crate) fn sort_scored_rows(rows: &mut [SweepRow]) {
    rows.sort_by(|a, b| match (a.score, b.score) {
        (Some(sa), Some(sb)) => cmp_scores_desc(sa, sb),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// [`run_sweep`], ranked by an arbitrary [`Objective`] instead of a [`RankMetric`]: every
/// successful row's report is scored (stamped into [`SweepRow::score`]) and rows sort by score
/// descending (higher is better), NaN scores after finite ones, failed rows last. `label` names
/// the objective in the report header/JSON (`rank_by`), e.g. `"multi"`.
pub fn run_sweep_with(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
) -> Result<SweepReport, HarnessError> {
    run_sweep_with_exec(base, store, objective, label, SweepExec::from_env())
}

/// [`run_sweep_with`] with the execution strategy passed explicitly instead of read from the env.
/// Both [`SweepExec`] variants return byte-identical reports.
pub fn run_sweep_with_exec(
    base: &BacktestProfile,
    store: Arc<dyn HistStore + Send + Sync>,
    objective: &Objective,
    label: impl Into<String>,
    exec: SweepExec,
) -> Result<SweepReport, HarnessError> {
    let mut rows = run_rows(base, store, exec)?;

    for row in &mut rows {
        row.score = row.report.as_ref().map(objective);
    }

    sort_scored_rows(&mut rows);

    Ok(SweepReport { rows, rank_by: RankBy::Objective(label.into()) })
}

/// Render one row's `k=v` overrides joined by spaces, e.g. `size=2.0 threshold=0.1`. Empty for a
/// no-sweep single point.
fn format_overrides(overrides: &[(String, toml::Value)]) -> String {
    overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

impl fmt::Display for SweepReport {
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
                    // Objective path only — the classic run_sweep never stamps a score, so the
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base_with_sweep(sweep: &str) -> BacktestProfile {
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
[strategy.params]
size = 1.0
{sweep}
"#
        ))
        .unwrap()
    }

    #[test]
    fn expand_cartesian_product() {
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
        let pts = expand_sweep(&base).unwrap();
        assert_eq!(pts.len(), 4, "2 x 2 grid");
        // every point overrides strategy.params.size + .threshold, and its own sweep field is cleared
        for p in &pts {
            assert!(p.profile.sweep.is_none(), "expanded point is a plain single-run profile");
            assert!(p.profile.strategy.params.get("size").is_some());
            assert!(p.profile.strategy.params.get("threshold").is_some());
        }
        // deterministic order + labels present
        assert!(!pts[0].overrides.is_empty());
    }

    #[test]
    fn no_sweep_is_single_point() {
        let base = base_with_sweep(""); // no [sweep]
        let pts = expand_sweep(&base).unwrap();
        assert_eq!(pts.len(), 1);
        assert!(pts[0].overrides.is_empty());
    }

    #[test]
    fn non_array_sweep_value_errors() {
        let base = base_with_sweep("[sweep]\nsize = 3.0"); // not an array
        assert!(matches!(expand_sweep(&base), Err(HarnessError::Validation(_))));
    }

    #[test]
    fn expand_is_deterministic() {
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0]\nthreshold = [0.1, 0.2]");
        let a = expand_sweep(&base).unwrap();
        let b = expand_sweep(&base).unwrap();
        let a_pairs: Vec<Vec<(String, toml::Value)>> =
            a.iter().map(|p| p.overrides.clone()).collect();
        let b_pairs: Vec<Vec<(String, toml::Value)>> =
            b.iter().map(|p| p.overrides.clone()).collect();
        assert_eq!(a_pairs, b_pairs);
    }

    // Only the store-backed tests below build bars into a concrete `DataFusionHist`; gated with them.
    #[cfg(feature = "datafusion-store")]
    fn bar(ts: i64, price: f64) -> vike_model::Bar {
        vike_model::Bar {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[cfg(feature = "datafusion-store")]
    #[test]
    fn run_sweep_ranks_points() {
        use vike_data::DataFusionHist;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0]");
        let rep = run_sweep(&base, store, RankMetric::Sharpe).unwrap();

        assert_eq!(rep.rows.len(), 2, "one row per sweep point");
        assert!(
            rep.rows.iter().all(|r| r.report.is_some() && r.error.is_none()),
            "both points ran"
        );
        assert_eq!(rep.rank_by, RankBy::Metric(RankMetric::Sharpe));
        assert!(
            rep.rows.iter().all(|r| r.score.is_none()),
            "the classic run_sweep never stamps a score"
        );

        // Ranked best-first: non-increasing Sharpe across successful rows (NaN, if any, sorts
        // last per `sort_key`'s `partial_cmp` fallback, so this holds even on a degenerate curve).
        let sharpes: Vec<f64> =
            rep.rows.iter().map(|r| r.report.as_ref().unwrap().sharpe).collect();
        for w in sharpes.windows(2) {
            if w[0].is_nan() || w[1].is_nan() {
                continue;
            }
            assert!(w[0] >= w[1], "rows must be ranked best-first by sharpe: {sharpes:?}");
        }

        // Display renders a compact ranked table, winner marked first — and no score column
        // (objective path only), keeping the default table unchanged.
        let s = rep.to_string();
        assert!(s.contains("sweep ranked by sharpe"));
        assert!(s.contains("*#1"));
        assert!(s.contains("size="));
        assert!(!s.contains("score="), "default table must not grow a score column");

        // Default JSON is unchanged too: rank_by is the bare metric string and no `score` key.
        let json = serde_json::to_string(&rep).unwrap();
        assert!(
            json.contains("\"rank_by\":\"sharpe\""),
            "rank_by must serialize as before: {json}"
        );
        assert!(!json.contains("\"score\""), "default JSON must not grow a score field: {json}");
    }

    /// DETERMINISM GATE (the rayon lane): the SAME sweep run in parallel and sequentially must
    /// produce byte-identical output — same rows, same rank order, same table, same JSON. Ranking
    /// must never depend on which point finished first; `par_iter().map().collect::<Vec<_>>()`
    /// reassembles by index (not completion), and the ranking sort is stable over that order.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn parallel_and_sequential_sweeps_are_byte_identical() {
        use vike_data::DataFusionHist;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        // Enough points that the pool really interleaves them.
        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]");

        for rank_by in [
            RankMetric::Sharpe,
            RankMetric::TotalReturn,
            RankMetric::MaxDrawdown,
            RankMetric::FinalEquity,
        ] {
            let run = |exec| run_sweep_exec(&base, store.clone(), rank_by, exec).unwrap();
            let seq = run(SweepExec::Sequential);
            let par = run(SweepExec::Parallel);

            assert_eq!(seq.rows.len(), 8, "{rank_by:?}: one row per grid point");
            assert_eq!(
                seq.to_string(),
                par.to_string(),
                "{rank_by:?}: the ranked table must be byte-identical"
            );
            assert_eq!(
                serde_json::to_string(&seq).unwrap(),
                serde_json::to_string(&par).unwrap(),
                "{rank_by:?}: --json output must be byte-identical"
            );
            // And bit-exact on the floats the table rounds for display.
            for (a, b) in seq.rows.iter().zip(&par.rows) {
                assert_eq!(a.overrides, b.overrides, "{rank_by:?}: same point at the same rank");
                match (&a.report, &b.report) {
                    (Some(ra), Some(rb)) => {
                        assert_eq!(ra.final_equity.to_bits(), rb.final_equity.to_bits());
                        assert_eq!(ra.total_return.to_bits(), rb.total_return.to_bits());
                        assert_eq!(ra.max_drawdown.to_bits(), rb.max_drawdown.to_bits());
                    }
                    (None, None) => {}
                    _ => panic!("{rank_by:?}: success/failure disagreed between exec modes"),
                }
            }
        }
    }

    /// The escape hatch is the EXACT string `"1"`, never a fuzzy truthy parse.
    #[test]
    fn sweep_exec_defaults_to_parallel() {
        assert_eq!(SweepExec::default(), SweepExec::Parallel);
        // `from_env` is only asserted here for the UNSET/other-value case that every CI process
        // has; mutating process env from a test would race the other tests in this binary.
        if std::env::var(SWEEP_SEQUENTIAL_ENV).is_err() {
            assert_eq!(SweepExec::from_env(), SweepExec::Parallel);
        }
    }

    /// The concurrency bound is a CAP, not a suggestion: parallelism multiplies the materialized
    /// data slice (module doc), so with `VIKE_SWEEP_THREADS` unset the pool must be sized
    /// `min(DEFAULT_SWEEP_THREADS, cores)` — never one worker per logical core, and never zero
    /// (rayon reads `num_threads(0)` as "all cores", the exact shape this cap prevents).
    #[test]
    fn sweep_threads_defaults_to_a_small_cap() {
        // Same reason as above: only asserted for the UNSET case, never by mutating process env.
        if std::env::var(SWEEP_THREADS_ENV).is_err() {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            let n = sweep_threads();
            assert!(n >= 1, "a zero-sized pool would mean 'all cores' to rayon");
            assert!(n <= DEFAULT_SWEEP_THREADS, "default concurrency must stay capped, got {n}");
            assert!(n <= cores, "never more workers than cores, got {n} on {cores} cores");
        }
    }

    #[test]
    fn run_sweep_records_failure() {
        // A synthetic failed row (no live backtest error path is easy to trigger here without
        // network/venue setup) exercises the sort-puts-failures-last rule and the Display FAILED
        // branch directly — the real error-capture wiring (`Err(e) => ... error: Some(..)`) is
        // exercised by every OTHER run_sweep test succeeding without ever populating `error`.
        let ok_report = BacktestReport {
            name: None,
            final_equity: 1100.0,
            total_return: 0.1,
            n_trades: 1,
            win_rate: 1.0,
            sharpe: 1.5,
            max_drawdown: 0.02,
            profit_factor: 2.0,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        };
        let mut rows = vec![
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(9.0))],
                report: None,
                error: Some("boom".to_string()),
                score: None,
            },
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
                report: Some(ok_report),
                error: None,
                score: None,
            },
        ];
        rows.sort_by(|a, b| match (&a.report, &b.report) {
            (Some(ra), Some(rb)) => RankMetric::Sharpe.cmp_reports(ra, rb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        let rep = SweepReport { rows, rank_by: RankBy::Metric(RankMetric::Sharpe) };

        assert!(rep.rows[0].report.is_some(), "successful row ranks first");
        assert!(rep.rows[1].error.is_some(), "failed row ranks last");

        let s = rep.to_string();
        assert!(s.contains("FAILED: boom"));
    }

    #[test]
    fn nan_sharpe_ranks_last_never_first() {
        // Regression: a NaN Sharpe (e.g. a zero-variance equity curve) must sort LAST among
        // successes — with the old `partial_cmp(...).unwrap_or(Equal)` it compared Equal to every
        // finite point and, being placed first here, stayed ranked #1.
        let good = BacktestReport {
            name: None,
            final_equity: 1200.0,
            total_return: 0.2,
            n_trades: 3,
            win_rate: 1.0,
            sharpe: 2.0,
            max_drawdown: 0.01,
            profit_factor: 3.0,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        };
        let degenerate = BacktestReport {
            name: None,
            final_equity: 1000.0,
            total_return: 0.0,
            n_trades: 0,
            win_rate: 0.0,
            sharpe: f64::NAN,
            max_drawdown: 0.0,
            profit_factor: 0.0,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        };
        // NaN row deliberately placed first, so a broken comparator would leave it ranked #1.
        let mut rows = [
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
                report: Some(degenerate),
                error: None,
                score: None,
            },
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
                report: Some(good),
                error: None,
                score: None,
            },
        ];
        rows.sort_by(|a, b| match (&a.report, &b.report) {
            (Some(ra), Some(rb)) => RankMetric::Sharpe.cmp_reports(ra, rb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        assert_eq!(rows[0].report.as_ref().unwrap().sharpe, 2.0, "finite-Sharpe point wins");
        assert!(
            rows[1].report.as_ref().unwrap().sharpe.is_nan(),
            "NaN-Sharpe point ranks last, not first"
        );
    }

    #[test]
    fn rank_metric_from_str() {
        assert_eq!(RankMetric::from_str_ci("SHARPE"), Some(RankMetric::Sharpe));
        assert_eq!(RankMetric::from_str_ci("return"), Some(RankMetric::TotalReturn));
        assert_eq!(RankMetric::from_str_ci("max_dd"), Some(RankMetric::MaxDrawdown));
        assert_eq!(RankMetric::from_str_ci("Equity"), Some(RankMetric::FinalEquity));
        assert_eq!(RankMetric::from_str_ci("nope"), None);
    }

    /// A varied report for the ranking-equivalence regression below.
    fn rep(total_return: f64, sharpe: f64, max_dd: f64, final_eq: f64) -> BacktestReport {
        BacktestReport {
            name: None,
            final_equity: final_eq,
            total_return,
            n_trades: 5,
            win_rate: 0.5,
            sharpe,
            max_drawdown: max_dd,
            profit_factor: 1.5,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        }
    }

    /// REGRESSION (default ranking unchanged): for EVERY `RankMetric`, ranking through its
    /// objective constructor (`RankMetric::objective` + the objective comparator) orders a varied
    /// report set — ties, negatives, and a NaN — EXACTLY like the classic `cmp_reports`
    /// comparator `run_sweep` still uses. So the two paths cannot drift apart.
    ///
    /// NB the equivalence covers TIED rows (rows 0 and 3 tie on `max_dd`) only because BOTH
    /// paths sort stably (`slice::sort_by`) and neither comparator has a secondary tie-break key,
    /// so ties keep input order on both sides. If `cmp_reports` ever grows a tie-break, the
    /// objective comparator must grow the same one — this test would catch it.
    #[test]
    fn metric_objectives_rank_identically_to_cmp_reports() {
        let reports = [
            rep(0.2, 1.5, 0.05, 1200.0),
            rep(-0.1, -0.3, 0.30, 900.0),
            rep(0.2, f64::NAN, 0.00, 1000.0), // NaN sharpe, zero drawdown
            rep(0.05, 0.9, 0.05, 1050.0),     // max_dd tie with row 0
            rep(0.4, 2.5, 0.10, 1400.0),
        ];
        for metric in [
            RankMetric::Sharpe,
            RankMetric::TotalReturn,
            RankMetric::MaxDrawdown,
            RankMetric::FinalEquity,
        ] {
            let objective = metric.objective();

            let mut classic: Vec<usize> = (0..reports.len()).collect();
            classic.sort_by(|&a, &b| metric.cmp_reports(&reports[a], &reports[b]));

            let scores: Vec<f64> = reports.iter().map(&objective).collect();
            let mut via_objective: Vec<usize> = (0..reports.len()).collect();
            via_objective.sort_by(|&a, &b| cmp_scores_desc(scores[a], scores[b]));

            assert_eq!(
                classic, via_objective,
                "{metric:?}: objective ranking must match cmp_reports"
            );
        }
    }

    /// A report with every field the composite objective reads, for the losing-grid regression.
    fn scored_rep(
        total_return: f64,
        max_dd: f64,
        pf: f64,
        win_rate: f64,
        n: usize,
    ) -> BacktestReport {
        BacktestReport {
            name: None,
            final_equity: 1000.0 * (1.0 + total_return),
            total_return,
            n_trades: n,
            win_rate,
            sharpe: -1.0,
            max_drawdown: max_dd,
            profit_factor: pf,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        }
    }

    /// REGRESSION (the sign law, through the RANKING comparator): on an ALL-LOSING grid — the
    /// common case when tuning a bad strategy — `--rank-by multi` must crown the LEAST-bad point.
    /// The pre-fix score multiplied a negative base by sub-1 shaping factors, so the catastrophic
    /// point (win_rate 0 -> coeff 0, pf 0 -> ln(1) = 0) collapsed to `-0.0` and ranked #1, and the
    /// thin 1-trade sample outranked the 100-trade one for the same reason.
    #[test]
    fn all_losing_grid_never_crowns_the_worst_point() {
        use crate::objective::{MultiMetricParams, multi_metric_score};

        let p = MultiMetricParams::default();
        let points = [
            ("catastrophic", scored_rep(-0.95, 0.95, 0.0, 0.0, 200)),
            ("bad", scored_rep(-0.40, 0.45, 0.4, 0.2, 120)),
            ("mild", scored_rep(-0.01, 0.02, 0.9, 0.45, 80)),
            ("mild-but-thin", scored_rep(-0.01, 0.02, 0.9, 0.45, 1)),
        ];
        let scores: Vec<f64> = points.iter().map(|(_, r)| multi_metric_score(r, &p)).collect();
        assert!(scores.iter().all(|s| *s < 0.0), "every point loses money: {scores:?}");

        let mut order: Vec<usize> = (0..points.len()).collect();
        order.sort_by(|&a, &b| cmp_scores_desc(scores[a], scores[b]));
        let ranked: Vec<&str> = order.iter().map(|&i| points[i].0).collect();
        assert_eq!(
            ranked,
            ["mild", "mild-but-thin", "bad", "catastrophic"],
            "losers must rank by (quality-amplified) loss size: {scores:?}"
        );
    }

    /// The objective path's JSON shape: a stamped finite score serializes as a NUMBER, and an
    /// "unrankable" NaN score goes through `ser_opt_score` to `null` — never a bare `NaN` token
    /// (invalid JSON) and never a serialization error. `None` staying skipped is pinned by
    /// `run_sweep_ranks_points`.
    #[test]
    fn objective_json_stamps_scores_and_nulls_unrankable() {
        let rows = vec![
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
                report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
                error: None,
                score: Some(1.25),
            },
            SweepRow {
                overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
                report: Some(rep(0.2, 1.5, 0.05, 1200.0)),
                error: None,
                score: Some(f64::NAN),
            },
        ];
        let report = SweepReport { rows, rank_by: RankBy::Objective("multi".to_string()) };

        let json = serde_json::to_string(&report).expect("an unrankable row must not fail --json");
        assert!(!json.contains("NaN"), "a raw NaN token would be invalid JSON: {json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["rank_by"], "multi", "an objective serializes as its label");
        assert_eq!(v["rows"][0]["score"], 1.25);
        assert!(v["rows"][1]["score"].is_null(), "NaN score must serialize as null: {json}");
    }

    /// The objective path end-to-end over a real store: scores are stamped, rows sort best-first
    /// by score, the header names the objective, and the Sharpe objective reproduces the classic
    /// `run_sweep` order on the same data.
    #[cfg(feature = "datafusion-store")]
    #[test]
    fn run_sweep_with_ranks_by_objective() {
        use crate::objective::{MultiMetricParams, multi_metric};
        use vike_data::DataFusionHist;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
        let bars = vec![bar(0, 100.0), bar(1000, 101.0), bar(2000, 102.0), bar(3000, 99.0)];
        store.append_bars("binance", "BTCUSDT", "1d", &bars, None).unwrap();

        let base = base_with_sweep("[sweep]\nsize = [1.0, 2.0]");

        // (a) multi objective: every successful row gets a score, sorted best-first (NaN-aware).
        let obj = multi_metric(MultiMetricParams::default());
        let rep = run_sweep_with(&base, store.clone(), &obj, "multi").unwrap();
        assert_eq!(rep.rank_by, RankBy::Objective("multi".to_string()));
        assert_eq!(rep.rows.len(), 2);
        assert!(rep.rows.iter().all(|r| r.report.is_some() && r.score.is_some()));
        let scores: Vec<f64> = rep.rows.iter().map(|r| r.score.unwrap()).collect();
        for w in scores.windows(2) {
            if w[0].is_nan() || w[1].is_nan() {
                assert!(!w[0].is_nan(), "a NaN score must not rank above a finite one: {scores:?}");
                continue;
            }
            assert!(w[0] >= w[1], "rows must be ranked best-first by score: {scores:?}");
        }
        let s = rep.to_string();
        assert!(s.contains("sweep ranked by multi"));
        assert!(s.contains("score="), "objective table shows the score column");

        // (b) the Sharpe built-in objective orders the rows exactly like the classic path.
        let classic = run_sweep(&base, store.clone(), RankMetric::Sharpe).unwrap();
        let sharpe_obj = RankMetric::Sharpe.objective();
        let via_obj = run_sweep_with(&base, store, &sharpe_obj, "sharpe").unwrap();
        let order = |r: &SweepReport| -> Vec<Vec<(String, toml::Value)>> {
            r.rows.iter().map(|row| row.overrides.clone()).collect()
        };
        assert_eq!(order(&classic), order(&via_obj), "built-in objective must match run_sweep");
    }

    /// SweepReport wiring: a zero-trade / flat row surfaces its top probable cause inline in the
    /// ranked table, while a row that traded is untouched (byte-identical). Built from literal rows
    /// so it does not depend on a strategy that happens to produce a flat run.
    #[test]
    fn sweep_table_annotates_zero_trade_rows_only() {
        let traded = BacktestReport {
            name: None,
            final_equity: 1200.0,
            total_return: 0.2,
            n_trades: 4,
            win_rate: 0.75,
            sharpe: 1.8,
            max_drawdown: 0.03,
            profit_factor: 2.5,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: None,
        };
        let flat = BacktestReport {
            name: None,
            final_equity: 1000.0,
            total_return: 0.0,
            n_trades: 0,
            win_rate: 0.0,
            sharpe: f64::NAN,
            max_drawdown: 0.0,
            profit_factor: 0.0,
            per_symbol_pnl: Vec::new(),
            funding_paid: 0.0,
            zero_trade: Some(crate::zero_trade::ZeroTradeReport {
                causes: vec![crate::zero_trade::ZeroTradeCause {
                    code: "orders-denied",
                    headline: "All 5 submitted order(s) were rejected before filling.".to_string(),
                    detail: "d".to_string(),
                }],
            }),
        };
        let report = SweepReport {
            rows: vec![
                SweepRow {
                    overrides: vec![("size".to_string(), toml::Value::Float(1.0))],
                    report: Some(traded),
                    error: None,
                    score: None,
                },
                SweepRow {
                    overrides: vec![("size".to_string(), toml::Value::Float(2.0))],
                    report: Some(flat),
                    error: None,
                    score: None,
                },
            ],
            rank_by: RankBy::Metric(RankMetric::Sharpe),
        };
        let s = report.to_string();
        assert!(
            s.contains("-- All 5 submitted order(s) were rejected before filling."),
            "the zero-trade row must show its top cause: {s}"
        );
        // The row that traded carries no `--` cause hint.
        let traded_line = s.lines().find(|l| l.contains("trades=4")).unwrap();
        assert!(!traded_line.contains("--"), "a row that traded is untouched: {traded_line}");
    }
}
