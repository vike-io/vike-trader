//! The parameter sweep over a slice: grid expansion, Sharpe ranking, deflated Sharpe and PBO.

use vike_analytics::BacktestResult;
use vike_analytics::metrics::{returns, sharpe};
use vike_analytics::overfit::{deflated_sharpe_with_effective_n, pbo_cscv, sharpe_moments};
use vike_analytics::report::DAILY_PERIODS_PER_YEAR;
use vike_backtest::harness::{cmp_scores_desc, map_bounded};
use vike_data::StoreHandle;
use vike_model::params::ParamValue;
use vike_sim::{EngineParams, StrategyEngine};

use super::{DataSlice, RunError, SliceKind, build_strategy_with, load_slice_bars, run_tick_slice};
use crate::spec::StrategySpec;

#[cfg(doc)]
use super::{run_slice, run_walkforward_slice_with_params};

/// One sweep grid point's outcome.
#[derive(Debug, Clone)]
pub struct ParamscanEntry {
    pub overrides: Vec<(String, ParamValue)>,
    pub result: BacktestResult,
}

/// A ranked parameter sweep + its deflated Sharpe and PBO across the trial set.
#[derive(Debug, Clone)]
pub struct StudioParamscan {
    pub entries: Vec<ParamscanEntry>, // ranked best-first by annualized Sharpe
    pub dsr: f64,
    /// Probability of backtest overfitting (CSCV) across the trials; `NaN` = not assessed
    /// (< 2 trials, too-short slice, or non-finite returns).
    pub pbo: f64,
    pub best_index: usize, // always 0 (entries are pre-ranked) — kept explicit for the UI
}

/// CSCV split count for PBO — even (López de Prado canonical S = 16; `C(16,8) = 12_870` combos,
/// computed off the UI thread inside `run_paramscan_slice`). Guarded by `T >= N_SPLITS`.
pub(super) const N_SPLITS: usize = 16;

/// The N per-trial per-observation return columns, each truncated to the common length T (the min
/// column length — all trials run over identical bars so lengths are equal in practice; the
/// truncation is a defensive guard against a jagged matrix). Column-major — exactly what
/// `deflated_sharpe_with_effective_n` wants; PBO uses [`transpose`].
pub(super) fn trial_columns(entries: &[ParamscanEntry]) -> (Vec<Vec<f64>>, usize) {
    let mut cols: Vec<Vec<f64>> = entries.iter().map(|e| returns(&e.result.equity_curve)).collect();
    let t = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    for c in &mut cols {
        c.truncate(t);
    }
    (cols, t)
}

/// Column-major `columns[N][T]` -> row-major `matrix[T][N]` (`matrix[t][j] = columns[j][t]`) — the
/// layout [`pbo_cscv`] indexes.
pub(super) fn transpose(columns: &[Vec<f64>], t: usize) -> Vec<Vec<f64>> {
    (0..t).map(|row| columns.iter().map(|c| c[row]).collect()).collect()
}

/// Rank sweep entries best-first by annualized Sharpe through the harness's ONE ranking
/// comparator ([`vike_backtest::harness::cmp_scores_desc`]): higher Sharpe first, a NaN
/// ("unrankable") Sharpe — e.g. an equity curve poisoned by a NaN from a blown-up strategy —
/// LAST, so a degenerate point can never sit at `best_index` 0 and feed the DSR. (The old
/// hand-rolled `partial_cmp(..).unwrap_or(Equal)` compared a NaN Equal to everything, so a
/// NaN point placed first by grid order stayed ranked #1.) `sort_by` is stable, so
/// equal-Sharpe points keep grid order.
///
/// ⚠ **The annualization factor here is ORDERING-INERT, and that is why it stays a constant while
/// [`run_walkforward_slice_with_params`] derives its own from the slice.** `metrics::sharpe`
/// multiplies by `sqrt(periods_per_year)`, so any positive factor is one uniform positive scale
/// applied to EVERY entry: it cannot reorder a comparison, and a NaN stays NaN under it. Nothing
/// downstream reads this number either — the DSR and PBO inputs are
/// `overfit::sharpe_moments(..).sr_per_obs`, which is PER-OBSERVATION and annualization-independent
/// by construction (see the comment beside the DSR block in [`run_paramscan_slice_with_params`]).
/// [`DAILY_PERIODS_PER_YEAR`] is therefore the honest spelling of "the scale does not matter here":
/// deriving it per slice would change no rank, no `best_index`, no `dsr` and no `pbo`, so a future
/// reader who "fixes" it into a derived value will have changed exactly nothing.
/// `ranking_order_is_invariant_to_the_annualization_factor` below is that claim's test.
pub(super) fn rank_entries_by_sharpe(entries: &mut [ParamscanEntry]) {
    entries.sort_by(|a, b| {
        cmp_scores_desc(
            sharpe(&a.result.equity_curve, DAILY_PERIODS_PER_YEAR),
            sharpe(&b.result.equity_curve, DAILY_PERIODS_PER_YEAR),
        )
    });
}

/// Cartesian product of a `(name, values)` grid into per-point `(name, value)` override lists.
pub(super) fn cartesian(grid: &[(String, Vec<ParamValue>)]) -> Vec<Vec<(String, ParamValue)>> {
    let mut out: Vec<Vec<(String, ParamValue)>> = vec![Vec::new()];
    for (name, values) in grid {
        let mut next = Vec::new();
        for base in &out {
            for v in values {
                let mut row = base.clone();
                row.push((name.clone(), v.clone()));
                next.push(row);
            }
        }
        out = next;
    }
    out
}

// THE SWEEP-THREADS FORWARDER IS DELETED, and what it forwarded is not.
//
// It re-exposed `vike_backtest::harness::install_sweep_threads` for exactly one caller:
// `vike-desktop`, which links `vike-studio` (which re-exported this in turn) and does NOT
// link `vike-backtest`, so without the seam the GUI could not hand its resolved
// `preferences.sweep_threads` to the pool its in-process `Backend::Local` sweep entered.
//
// That backend is gone. A sweep now runs in the compute daemon, which IS vike-backtest and
// calls `crate::harness::install_sweep_threads` directly, so the forwarder had no caller left
// and a re-export whose consumer is gone is a second name that rots. `run_paramscan_slice`
// below is untouched and still enters the same bounded pool - it is reached from
// `crate::wire_run` on the daemon side now instead of from a GUI thread here.

/// Sweep the strategy over the grid: build-per-point (with that point's overrides), backtest each,
/// rank by annualized Sharpe (NaN Sharpe LAST — see [`rank_entries_by_sharpe`]), and compute the
/// deflated Sharpe across the trial Sharpes. A point that fails to build is skipped (resilient,
/// like the harness `run_paramscan`); all-failing -> error.
///
/// **Parallel, on the harness's BOUNDED pool** (`vike_backtest::harness::map_bounded` —
/// `install_bounded`/`sweep_threads`, `VIKE_SWEEP_THREADS`-capped, never rayon's global pool). Same
/// reasoning as the harness sweep: each point materializes its own copy of the slice (a tick point
/// re-scans the store outright), so concurrency is an `N_threads x slice` memory multiplier on
/// boxes that also host live trading.
///
/// **Determinism does not rest on scheduling.** `map_bounded` reassembles by index, so the surviving
/// entries reach the ranking sort in exact `cartesian(grid)` order — the same sequence a
/// one-at-a-time loop produced — and `sort_by` is stable, so equal-Sharpe points keep grid order.
pub fn run_paramscan_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<ParamValue>)],
) -> Result<StudioParamscan, RunError> {
    run_paramscan_slice_with_params(spec, slice, store, grid, EngineParams::default)
}

/// [`run_paramscan_slice`] with CALLER-SUPPLIED engine cost/cash: `make_params` builds a FRESH
/// [`EngineParams`] for EACH grid point, so every point runs under the SAME cost knobs (cash /
/// fee_rate / slippage). A per-point factory rather than one shared value because `EngineParams` is
/// not `Clone` (it can carry a `Box<dyn PositionSizer>`) AND each point runs on the bounded parallel
/// pool — hence the `Send + Sync` bound on the factory. The default-params [`run_paramscan_slice`]
/// delegates here with `EngineParams::default`. This is the seam the compute-to-data `RunSweep` verb
/// reaches so a profile's `[engine]` costs apply server-side, mirroring how [`run_slice`] already
/// takes an `EngineParams`.
pub fn run_paramscan_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<ParamValue>)],
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<StudioParamscan, RunError> {
    // Bar slices load ONCE and every point clones the series (the load is the expensive part and it
    // is identical across points). Tick slices have no such hoist: `replay_ticks` owns its own
    // scan, so each point re-reads the store — hence the bounded pool above.
    let bars = match slice.kind {
        SliceKind::Bars => Some(load_slice_bars(slice, store)?),
        SliceKind::Ticks => None,
    };
    let points = cartesian(grid);
    let rows: Vec<Option<ParamscanEntry>> = map_bounded(points, |overrides| {
        let strat = build_strategy_with(spec, &overrides).ok()?; // skip a failing point
        let result = match &bars {
            Some(series) => StrategyEngine::new(series.clone(), strat, make_params()).run(),
            None => run_tick_slice(strat, slice, store, make_params()).ok()?,
        };
        Some(ParamscanEntry { overrides, result })
    });
    let mut entries: Vec<ParamscanEntry> = rows.into_iter().flatten().collect();
    if entries.is_empty() {
        return Err(RunError::Compile("every sweep point failed to run".into()));
    }
    rank_entries_by_sharpe(&mut entries);
    // Every DSR input comes from the shared `overfit::sharpe_moments` derivation, which owns the
    // per-period-Sharpe and non-excess-kurtosis conventions (see its doc — both are footguns; the
    // annualized-Sharpe one already shipped as a bug in vike-ai). The ranking sort above is
    // deliberately the ANNUALIZED Sharpe: a display/ordering quantity, never a DSR input.
    let trials: Vec<f64> =
        entries.iter().map(|e| sharpe_moments(&e.result.equity_curve).sr_per_obs).collect();
    let (columns, t) = trial_columns(&entries);
    let best = &entries[0].result;
    let m = sharpe_moments(&best.equity_curve);
    let dsr =
        deflated_sharpe_with_effective_n(m.sr_per_obs, &trials, &columns, m.n_obs, m.skew, m.kurt);
    let pbo = if entries.len() >= 2 && t >= N_SPLITS {
        pbo_cscv(&transpose(&columns, t), N_SPLITS)
    } else {
        f64::NAN
    };
    Ok(StudioParamscan { entries, dsr, pbo, best_index: 0 })
}
