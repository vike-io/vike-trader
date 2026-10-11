//! Walk-forward over a bar slice: the fixed-parameter control and the per-window search.

use std::sync::mpsc::Receiver;
use vike_analytics::report::{BacktestReport, periods_per_year_for_interval};
use vike_analytics::validation::WalkMode;
use vike_backtest::harness::{RankMetric, cmp_scores_desc, map_bounded};
use vike_backtest::walkforward::runner::WindowSearch;
use vike_backtest::walkforward::{WalkForwardReport, WindowOutcome, walk_forward_strategy};
use vike_model::Bar;
use vike_sim::{EngineParams, StrategyEngine};

use super::{
    DataSlice, RunError, StoreHandle, build_strategy, build_strategy_with, cartesian,
    load_slice_bars, spawn_outcome,
};
use crate::cost_model::FillMix;
use crate::spec::StrategySpec;

#[cfg(doc)]
use super::{SliceKind, run_paramscan_slice_with_params};
#[cfg(doc)]
use vike_analytics::report::DAILY_PERIODS_PER_YEAR;

/// Walk-forward the strategy over `n_splits` OOS windows (anchored), default engine params. The thin
/// wrapper over [`run_walkforward_slice_with_params`] that supplies `EngineParams::default` — every
/// existing caller keeps its exact previous cost/cash behavior.
///
/// BAR slices only, and single-symbol: `walk_forward_strategy` splits ONE `&[Bar]` series into
/// windows. A tick or multi-symbol slice is a [`RunError::Data`] rather than a silent first-symbol
/// fallback.
///
/// `n_splits` must be `>= 1`, and the Sharpe annualization is derived from `slice.interval` rather
/// than fixed at the daily 252. Both belong to [`run_walkforward_slice_with_params`], whose doc
/// carries the argument for each — this wrapper adds only the default `EngineParams`.
pub fn run_walkforward_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
) -> Result<WalkForwardReport, RunError> {
    run_walkforward_slice_with_params(spec, slice, store, n_splits, EngineParams::default)
}

/// [`run_walkforward_slice`] with CALLER-SUPPLIED engine cost/cash: `make_params` builds a FRESH
/// [`EngineParams`] each time it is called, so every window (and the stitched cash line seeded from
/// `make_params().cash`) uses the SAME cost knobs. A per-call factory rather than one shared value
/// because `EngineParams` is not `Clone` (it can carry a `Box<dyn PositionSizer>`); the default-params
/// [`run_walkforward_slice`] delegates here with `EngineParams::default`.
///
/// ⚠ **This is the FIXED-parameter spelling, and it is no longer the only one.** It delegates to
/// [`run_walkforward_slice_searching`] with [`WindowSearchPlan::none`] — the control — so the two
/// walks are one driver rather than two programs, and the cost knobs still apply the same way. The
/// compute-to-data `RunWalkforward` verb reaches the SEARCHING entry directly
/// (`crate::wire_run`'s `run_walkforward_local`), because it also needs the realised fill mix that
/// entry returns.
///
/// The `Send + Sync` bound on the factory arrived with that delegation: the searching arm scores
/// candidates on the harness's bounded parallel pool, which requires it. `EngineParams::default`
/// and every wire-params closure satisfy it.
///
/// # The annualization comes from the SLICE, not from a literal
///
/// `WalkForwardReport::oos_sharpe` is scaled by `sqrt(periods_per_year)`, and that factor has to be
/// the number of RETURN OBSERVATIONS a year of THESE bars produces — one per bar. It is
/// [`periods_per_year_for_interval`] over `slice.interval`: the ONE home for the derivation, and
/// the same function `vike_backtest::harness::report::periods_per_year` wraps for the profile
/// plane. So the CLI/MCP door and the Studio door cannot report a different Sharpe for the same
/// strategy over the same bars.
///
/// This call used to pass a bare `252.0` while the harness derived its factor — and the harness's
/// own doc asserted the two planes "made the same two derivations", with no test comparing them.
/// They disagreed by `sqrt(24) ≈ 4.9x` on 1h bars and by `sqrt(1440) ≈ 37.9x` on 1m. A `"1d"`
/// slice is bit-identical to what it always reported ([`DAILY_PERIODS_PER_YEAR`] is preserved
/// exactly), so only intraday and longer-than-daily slices move.
///
/// The TICK branch the harness wrapper carries has no twin here, deliberately: [`load_slice_bars`]
/// runs first and refuses a [`SliceKind::Ticks`] slice as [`RunError::Data`], so the derivation is
/// reachable only on the bar path. A `kind`-check beside it would be unreachable code wearing the
/// shape of a safety net.
///
/// # `n_splits` has a floor, because the failure below it is SILENT
///
/// `vike_analytics::validation::walk_forward_splits` loops `1..=n_splits`, so `0` produces no
/// splits at all: `walk_forward_strategy` never enters its window loop, the stitched curve stays
/// empty, `equity` never leaves `cash`, and `wf_consistency` divides by `windows.len().max(1)`.
/// The report comes back all zeros — `oos_return` 0, `oos_sharpe` 0, `wf_consistency` 0, no
/// windows — which is a run that NEVER HAPPENED wearing the shape of a strategy that made nothing,
/// and nothing anywhere returns an error. So it is refused here as a [`RunError::Data`].
///
/// This plane checked it at no level while `vike_backtest::harness::run_walkforward` checks it at
/// two. The GUI was shielded only by `crates/vike-studio/src/studio/dispatch.rs`'s
/// `start_walkforward` passing a hardcoded literal; the compute-to-data `RunWalkforward` verb
/// (`crate::wire_run`'s `run_walkforward_local`) takes `n_splits` from its caller and applies
/// no floor of its own, so the hole was wire-reachable.
pub fn run_walkforward_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<WalkForwardReport, RunError> {
    run_walkforward_slice_searching(
        spec,
        slice,
        store,
        n_splits,
        WindowSearchPlan::none(),
        make_params,
    )
    .map(|(report, _mix)| report)
}

/// WHICH search a window runs on its own training half, and under what ranking — the Studio door's
/// spelling of the knobs `[walkforward].search` / `[walkforward].rank_by` carry on the profile door.
///
/// ⚠ **[`WindowSearch::None`] is a VALUE here, not an absence**, and that is `0063`'s commitment
/// rather than a style choice: the profile door already treats the no-search walk as a first-class
/// mode on measured grounds (across seven selection criteria, "no search at all" landed inside one
/// seed's noise band at a fraction of the cost), and an optimizer whose null control is unreachable
/// cannot be evaluated. [`WindowSearchPlan::none`] is that control, and it reaches the SAME driver
/// the searching plan does — which is what makes the two comparable by construction instead of by
/// review.
#[derive(Debug, Clone, Copy)]
pub struct WindowSearchPlan<'a> {
    /// The search mode. [`WindowSearch::None`] walks the spec's own parameters.
    pub search: WindowSearch,
    /// The `(name, values)` axes each window scores on its TRAINING half — the same grid shape
    /// [`run_paramscan_slice_with_params`] takes. Ignored under [`WindowSearch::None`].
    pub grid: &'a [(String, Vec<f64>)],
    /// Which metric picks each window's winner. Ignored under [`WindowSearch::None`].
    pub rank: RankMetric,
}

impl<'a> WindowSearchPlan<'a> {
    /// **The CONTROL** — every window walks the spec's own parameters, and the report is the
    /// fixed-parameter walk's. See this type's own doc for why it is a value rather than an absent
    /// argument.
    pub fn none() -> Self {
        WindowSearchPlan { search: WindowSearch::None, grid: &[], rank: RankMetric::default() }
    }

    /// A SEARCHING plan: score `grid` on each window's training half and rank by `rank`.
    pub fn sweep(grid: &'a [(String, Vec<f64>)], rank: RankMetric) -> Self {
        WindowSearchPlan { search: WindowSearch::Sweep, grid, rank }
    }
}

/// **The ONE walk-forward driver this crate has**, and the entry both the control and the searching
/// arm reach — `0063`'s "the no-search control stays reachable" is a property of this signature
/// rather than of a convention.
///
/// Returns the stitched report AND the [`FillMix`] its OUT-OF-SAMPLE windows realised, because a
/// cost-model stamp cannot be read off an equity curve and the run that charged the fees is the
/// only thing that knows. The mix folds the VALIDATION runs only: a search's training scores are in
/// no reported number, so counting their fills would describe work the report does not contain.
///
/// # What the searching arm does, per window
///
/// Expand `plan.grid`, score every point on the window's TRAINING half through
/// `plan.rank`'s objective, take the best-scoring point — and only it — onto the window's
/// validation half, and record what was chosen in `WfWindow::chosen_params`. That is the protocol
/// "walk-forward" usually names, and it asks a different question from the control's: the control
/// asks whether FIXED parameters were stable out of sample, this asks whether the PROCEDURE of
/// fit-then-trade survives out of sample. Both stitch through the same runner and return the same
/// report type, which is the entire point of routing the control through here.
///
/// ⚠ **A `Sweep` plan with an EMPTY grid is REFUSED, not silently demoted.** `cartesian(&[])`
/// answers ONE point carrying no overrides, so an empty grid would make the search "no search"
/// while every window still recorded a choice and the report still said it had optimized — the
/// exact refusal `vike_backtest::walkforward::runner::run_walkforward_optimized_with` states for a
/// profile naming `search = "sweep"` with no `[sweep]` table.
///
/// ⚠ **A window in which every candidate FAILED is an error, not an empty choice.** The window
/// closure cannot return `Err` (the runner's signature owns it), so the failure is captured and
/// raised after the walk — the harness optimizer's own shape.
///
/// The annualization, the single-symbol rule and the `n_splits >= 1` floor are unchanged and are
/// argued on [`run_walkforward_slice_with_params`].
pub fn run_walkforward_slice_searching(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
    plan: WindowSearchPlan<'_>,
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<(WalkForwardReport, FillMix), RunError> {
    // pre-resolve so a bad script/name fails as Compile/Strategy before the loop (the closure's
    // own build can't return an error).
    build_strategy(spec)?;
    if slice.symbols.len() > 1 {
        return Err(RunError::Data(
            "walk-forward runs over ONE bar series — pick a single symbol".into(),
        ));
    }
    // Zero windows is an all-zeros REPORT, not an error, anywhere below this line — see the
    // `n_splits` section of this function's doc for the mechanism. Refused before the store read,
    // because a run that cannot produce a window should not cost a bar load either.
    if n_splits == 0 {
        return Err(RunError::Data(
            "walk-forward needs at least ONE out-of-sample window — n_splits must be >= 1".into(),
        ));
    }
    // Refused BEFORE the store read, like the `n_splits` floor above and for the same reason: a
    // request that cannot honour what it asked for should not cost a bar load either.
    if plan.search.searches() && plan.grid.is_empty() {
        return Err(RunError::Data(
            "a searching walk-forward needs a parameter GRID — with no axes the search expands to \
             ONE point carrying no overrides, which is the fixed walk wearing a report that says \
             it optimized. Send at least one axis, or ask for search = \"none\", the control."
                .into(),
        ));
    }
    let series = load_slice_bars(slice, store)?;
    let (sym, bars) = series.into_iter().next().expect("load_slice_bars rejects an empty list");
    let cash = make_params().cash;
    // The annualization is DERIVED from this slice's own bar step (see the doc above). `slice` is
    // known to be `SliceKind::Bars` by here — `load_slice_bars` refuses anything else — so
    // `interval` is a real bar step rather than the empty string a tick slice carries.
    //
    // `train` is used only by the SEARCHING arm; under the control it is ignored deliberately —
    // that walk is an out-of-sample stability check rather than an optimize-then-test protocol.
    //
    // ⚠ Both halves above arrived from DIFFERENT branches and this call is where they met. Taking
    // either side alone was wrong in a way only the merged tree shows: the branch's side still
    // carried the bare `252.0`, so merging it would have SILENTLY REVERTED the annualization fix
    // that shipped separately.
    let periods_per_year = periods_per_year_for_interval(&slice.interval);
    let objective = plan.rank.objective();
    // Out-of-band failure propagation: the window closure owns no `Err` channel, so a window whose
    // whole candidate set failed is recorded here and raised after the walk (the harness
    // optimizer's own shape).
    let mut failed_window: Option<String> = None;
    let mut mix = FillMix::default();
    let rep = walk_forward_strategy(
        &bars,
        n_splits,
        WalkMode::Anchored,
        cash,
        periods_per_year,
        |train, window| {
            let chosen = match plan.search {
                WindowSearch::None => None,
                WindowSearch::Sweep => {
                    let winner = best_point_on_train(
                        spec,
                        &sym,
                        train,
                        plan.grid,
                        &objective,
                        periods_per_year,
                        &make_params,
                    );
                    if winner.is_none() {
                        // Recorded, not raised: the runner owns this closure's signature and it
                        // has no `Err` channel. The walk still produces an outcome for this
                        // window; the whole report is discarded when the error is raised below.
                        failed_window.get_or_insert_with(|| {
                            format!(
                                "every candidate failed to run on the training half of the \
                                 out-of-sample window [{}, {}) — a walk cannot report a winner it \
                                 never found",
                                window.first().map_or(0, |b| b.ts),
                                window.last().map_or(0, |b| b.ts),
                            )
                        });
                    }
                    winner
                }
            };
            let strat = match &chosen {
                None => build_strategy(spec).expect("pre-checked above"),
                Some(overrides) => build_strategy_with(spec, overrides)
                    .expect("a winner is a candidate that already built once, on the train half"),
            };
            let result =
                StrategyEngine::new(vec![(sym.clone(), window.to_vec())], strat, make_params())
                    .run();
            // The realised mix describes the VALIDATION runs, which are the only ones any reported
            // number comes from.
            mix.add(&result);
            WindowOutcome {
                result,
                // Rendered onto the wire as TOML text by `wire_run`'s `to_wire_wf_window`; a Studio
                // axis is an `f64`, so `Float` is the whole of the mapping.
                chosen_params: chosen.map(|overrides| {
                    overrides
                        .into_iter()
                        .map(|(k, v)| (k, toml::Value::Float(v)))
                        .collect::<Vec<_>>()
                }),
            }
        },
    );
    if let Some(why) = failed_window {
        return Err(RunError::Data(why));
    }
    Ok((rep, mix))
}

/// One SCORED candidate: its objective value, and the parameter overrides that produced it.
///
/// Named rather than spelled inline because [`best_point_on_train`] holds it twice — once wrapped
/// in the `Option` a skipped candidate leaves behind — and the nested spelling is what
/// `clippy::type_complexity` objects to. The pair is deliberately score-FIRST, so
/// [`cmp_scores_desc`] reads off `.0` the way the harness's own ranking does.
type ScoredPoint = (f64, Vec<(String, f64)>);

/// Score every grid point on one window's TRAINING half and return the winner's overrides, or
/// `None` when every candidate failed to build or run.
///
/// Ranked through the harness's ONE comparator ([`cmp_scores_desc`]) so a NaN objective — a
/// degenerate candidate, e.g. a Sharpe over a zero-variance curve — sorts LAST and can never win.
/// `sort_by` is stable and [`map_bounded`] reassembles by index, so equal-scoring candidates keep
/// grid order and the winner does not depend on scheduling.
///
/// Parallel on the harness's BOUNDED pool, the same one [`run_paramscan_slice_with_params`] enters:
/// each candidate clones the training series, so concurrency is an `N_threads x window` memory
/// multiplier on boxes that may also be hosting live trading.
fn best_point_on_train(
    spec: &StrategySpec,
    sym: &str,
    train: &[Bar],
    grid: &[(String, Vec<f64>)],
    objective: &vike_backtest::search::objective::Objective,
    periods_per_year: f64,
    make_params: &(impl Fn() -> EngineParams + Send + Sync),
) -> Option<Vec<(String, f64)>> {
    let points = cartesian(grid);
    let scored: Vec<Option<ScoredPoint>> = map_bounded(points, |overrides| {
        let strat = build_strategy_with(spec, &overrides).ok()?; // skip a failing candidate
        let result =
            StrategyEngine::new(vec![(sym.to_string(), train.to_vec())], strat, make_params())
                .run();
        let report = BacktestReport::from_result(None, &result, periods_per_year);
        Some((objective(&report), overrides))
    });
    let mut scored: Vec<ScoredPoint> = scored.into_iter().flatten().collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| cmp_scores_desc(a.0, b.0));
    Some(scored.swap_remove(0).1)
}

/// Worker-thread twin of [`run_walkforward_slice`] (the SP2 `spawn_run` pattern).
pub fn spawn_walkforward(
    spec: StrategySpec,
    slice: DataSlice,
    store: StoreHandle,
    n_splits: usize,
) -> Receiver<Result<WalkForwardReport, RunError>> {
    spawn_outcome(move || run_walkforward_slice(&spec, &slice, &store, n_splits))
}
