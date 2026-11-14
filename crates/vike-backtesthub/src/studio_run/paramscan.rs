//! The parameter sweep over a slice: the profile's own grid search, ranked by Sharpe, with the
//! deflated Sharpe and PBO the harness computes for it.

use std::sync::Arc;

use vike_backtest::harness::optimize::{DEFAULT_CSCV_SPLITS, overfit_stats};
use vike_backtest::harness::paramscan::{OverfitReturnBuckets, SearchGrid};
use vike_backtest::harness::{
    ParamscanExecution, ProfileBacktest, RankMetric, SearchEvaluatorStore, optimize,
};
use vike_backtesthub_client::wire_studio::StrategyEngineConfigWire;
use vike_data::HistoryStore;
use vike_model::params::{ParamValue, Params};

use super::single::{data_error, slice_profile};
use super::strategy::StrategyFiles;
use vike_studio_core::{DataSlice, ParamscanEntry, RunError, StrategySpec, StudioParamscan};

/// Sweep the strategy over the grid with the engine's default cash and the venue's own fees: see
/// [`run_paramscan_slice_with_params`].
pub fn run_paramscan_slice(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    grid: &[(String, Vec<ParamValue>)],
) -> Result<StudioParamscan, RunError> {
    run_paramscan_slice_with_params(spec, files, slice, store, grid, None)
}

/// **The slice's profile (`super::single::slice_profile`, `costs` as a single run takes them) with
/// `grid` as its `[sweep]` table, searched by the harness's grid** — the `vike-cli backtest run`
/// search, point for point: `vike_backtest::harness::paramscan::expand_paramscan_overrides` expands it
/// (axes in key order), every point is a profile run on the bounded pool, the rows rank by Sharpe
/// through the harness's one ordering rule, and `vike_backtest::harness::optimize::overfit_stats`
/// deflates the winner and computes the PBO over each trial's bucketed returns.
///
/// ⚠ **A point that FAILED fails the sweep, naming it.** The wire answer has a result per point and
/// no row for a failure, and dropping one would leave the winner, `dsr` and `pbo` computed over
/// fewer trials than the grid named, in a result that carries no count of them.
pub fn run_paramscan_slice_with_params(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    grid: &[(String, Vec<ParamValue>)],
    costs: Option<&StrategyEngineConfigWire>,
) -> Result<StudioParamscan, RunError> {
    paramscan_profile(slice_profile(spec, files, slice, costs, store.as_ref())?, grid, store)
}

/// The `[sweep]` table of a wire grid: each axis an array of its values.
pub(crate) fn sweep_table(grid: &[(String, Vec<ParamValue>)]) -> Params {
    let mut table = Params::new();
    for (name, values) in grid {
        table.insert(name.clone(), ParamValue::List(values.clone()));
    }
    table
}

/// The sweep of an already-built slice profile — [`run_paramscan_slice_with_params`]'s body, shared
/// with the wire verb, which stamps its answer from the same profile.
pub(crate) fn paramscan_profile(
    mut profile: ProfileBacktest,
    grid: &[(String, Vec<ParamValue>)],
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> Result<StudioParamscan, RunError> {
    profile.paramscan = Some(sweep_table(grid));
    let evaluation = SearchEvaluatorStore::classic(
        &profile,
        store.clone(),
        RankMetric::Sharpe,
        ParamscanExecution::Parallel,
    )
    .with_return_buckets(OverfitReturnBuckets::DEFAULT)
    .with_results();
    let report = optimize(&SearchGrid, &profile, &evaluation).map_err(data_error)?.report;
    let overfit = overfit_stats(&report.rows, &evaluation.captured_returns(), DEFAULT_CSCV_SPLITS);
    let mut results = evaluation.take_results();
    let entries = report
        .rows
        .into_iter()
        .map(|row| match (row.error, results.remove(&row.overrides)) {
            (None, Some(result)) => Ok(ParamscanEntry { overrides: row.overrides, result }),
            (error, _) => {
                let point: Vec<String> =
                    row.overrides.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                Err(RunError::Data(format!(
                    "the sweep point [{}] failed, so the sweep is refused rather than ranked \
                     without it: {}",
                    point.join(", "),
                    error.unwrap_or_else(|| "it left no result".to_string())
                )))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let nan_if_none = |x: Option<f64>| x.unwrap_or(f64::NAN);
    Ok(StudioParamscan {
        entries,
        dsr: nan_if_none(overfit.as_ref().and_then(|o| o.deflated_sharpe)),
        pbo: nan_if_none(overfit.as_ref().and_then(|o| o.pbo)),
        best_index: 0,
    })
}
