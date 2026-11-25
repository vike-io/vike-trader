//! Walk-forward over a bar slice: the profile's own walk, fixed or searching each window.

use std::sync::Arc;

use vike_backtest::run::profile::{ProfileBacktest, ProfileWalkforward};
use vike_backtest::walkforward::runner::run_walkforward_optimized;
use vike_backtesthub_client::wire_studio::{StrategyEngineConfigWire, WalkforwardWindowSearchWire};
use vike_data::HistoryStore;

use super::paramscan::sweep_table;
use super::single::{SliceRun, data_error, slice_profile};
use super::strategy::StrategyFiles;
use vike_analytics::walkforward::WalkforwardResults;
use vike_studio_core::{DataSlice, RunError, StrategySpec};

/// Walk the strategy forward over `n_splits` out-of-sample windows (anchored), at the engine's
/// default cash and the venue's own fees: see [`run_walkforward_slice_searching`].
pub fn run_walkforward_slice(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    n_splits: usize,
) -> Result<WalkforwardResults, RunError> {
    run_walkforward_slice_with_params(spec, files, slice, store, n_splits, None)
}

/// [`run_walkforward_slice`] at `costs`, the FIXED-parameter walk: every window runs the spec's
/// own parameters. [`run_walkforward_slice_searching`] with no search.
pub fn run_walkforward_slice_with_params(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    n_splits: usize,
    costs: Option<&StrategyEngineConfigWire>,
) -> Result<WalkforwardResults, RunError> {
    run_walkforward_slice_searching(spec, files, slice, store, n_splits, None, costs)
}

/// **The slice's profile (`super::single::slice_profile`) walked by the harness's own walk-forward**
/// (`vike_backtest::walkforward::runner::run_walkforward_optimized`): `[walkforward] n_splits` and,
/// when `search` asks, `search`/`rank_by` with its grid as the `[sweep]` table. So the window list,
/// the per-window search, the annualization (`data.interval`'s), the single-series and bar-only
/// rules and every refusal are the profile walk's, word for word: an unknown method or metric, a
/// `"sweep"` with no axes and `n_splits = 0` are refused by the profile's validation.
///
/// `search` absent, or `"none"`, is the FIXED walk — the control `0063` keeps reachable.
pub fn run_walkforward_slice_searching(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    n_splits: usize,
    search: Option<&WalkforwardWindowSearchWire>,
    costs: Option<&StrategyEngineConfigWire>,
) -> Result<WalkforwardResults, RunError> {
    let profile = walkforward_profile(
        slice_profile(spec, files, slice, costs, store.as_ref(), SliceRun::Points)?,
        n_splits,
        search,
    )?;
    run_walkforward_optimized(&profile, store.clone()).map_err(data_error)
}

/// A slice profile given its `[walkforward]` table (and, for a search, its `[sweep]` grid), then
/// validated as a profile file would be.
pub(crate) fn walkforward_profile(
    mut profile: ProfileBacktest,
    n_splits: usize,
    search: Option<&WalkforwardWindowSearchWire>,
) -> Result<ProfileBacktest, RunError> {
    if let Some(search) = search {
        profile.paramscan = Some(sweep_table(&search.grid.axes));
    }
    profile.walkforward = Some(ProfileWalkforward {
        n_splits: Some(n_splits),
        search: search.map(|s| s.method.clone()),
        rank_by: search.and_then(|s| s.rank_by.clone()),
        ..ProfileWalkforward::default()
    });
    profile.validate().map_err(data_error)?;
    Ok(profile)
}
