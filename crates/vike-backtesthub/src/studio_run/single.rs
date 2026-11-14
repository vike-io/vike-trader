//! One strategy over one slice (bar or tick) as the profile run it is. (The one worker-thread spawn
//! every Studio caller waits on stayed with the vocabulary: `vike_studio_core::spawn_outcome`.)

use std::sync::Arc;

use vike_backtest::harness::{HarnessError, KeptRows, ProfileBacktest, run_backtest};
use vike_backtesthub_client::wire_studio::StrategyEngineConfigWire;
use vike_data::HistoryStore;

use super::strategy::{StrategyFiles, spec_error};
use super::strategy_config;
use vike_studio_core::{DataSlice, RunError, RunOutcome, StrategySpec};

/// **The profile a Studio run of `spec` over `slice` IS** (`ProfileBacktest::for_slice`), so the
/// Studio and `vike-cli backtest run` answer one question one way: the venue's own fee table unless
/// `costs` states a rate, the stored funding of a perpetual, every `[engine]` default.
///
/// Before it is built: the strategy is resolved once, so a bad script stays a
/// [`RunError::Compile`] and an unknown name a [`RunError::Strategy`]; and a symbol this door cannot
/// book is refused in words that send the user to a profile run — one another fiat quotes (a picked
/// slice has no ledger field) and a coin-margined contract (it has no contract value). Every other
/// refusal is the profile's, a [`RunError::Data`].
///
/// The slice's KEPT instrument rows are asked of `store` (the datahub) once, here: the quote the picks
/// refusal reads and every fact the profile's run reads (`ProfileBacktest::kept`).
pub(crate) fn slice_profile(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    costs: Option<&StrategyEngineConfigWire>,
    store: &(dyn HistoryStore + Send + Sync),
) -> Result<ProfileBacktest, RunError> {
    let strategy = strategy_config(spec, files).map_err(|e| spec_error(spec, &e))?;
    strategy.build().map_err(|e| spec_error(spec, &e))?;
    let picks: Vec<(&str, &str)> =
        slice.symbols.iter().map(|s| (slice.venue.as_str(), s.as_str())).collect();
    let series: Vec<(String, String)> =
        picks.iter().map(|(v, s)| (v.to_string(), s.to_string())).collect();
    let kept = KeptRows::read(store, &series);
    vike_backtest::harness::refuse_foreign_quoted_picks(picks.iter().copied(), &kept)
        .map_err(data_error)?;
    vike_backtest::harness::refuse_inverse_contracts(picks.iter().copied()).map_err(data_error)?;
    let mut profile = ProfileBacktest::for_slice(
        &slice.venue,
        &slice.symbols,
        &slice.interval,
        slice.range,
        slice.kind,
        costs,
        strategy,
    )
    .map_err(data_error)?;
    profile.kept = kept;
    Ok(profile)
}

/// A harness refusal of a slice's run, in the Studio's error type.
pub(crate) fn data_error(e: HarnessError) -> RunError {
    RunError::Data(e.message().to_string())
}

/// Run a slice's profile ([`slice_profile`]) through `vike_backtest::harness::run_backtest`.
///
/// A slice whose symbols hold nothing in range runs an engine over nothing (`replay_ticks` skips a
/// symbol with no quotes/trades/books, and an empty bar series has no step) and returns a flat
/// curve: that is a run that never happened, so it is a [`RunError::Data`].
pub(crate) fn run_slice_profile(
    profile: &ProfileBacktest,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> RunOutcome {
    let result = run_backtest(profile, store.clone()).map_err(data_error)?;
    if result.equity_curve.is_empty() {
        let symbols = profile.data.symbols.join(", ");
        return Err(RunError::Data(format!(
            "nothing recorded for {} [{symbols}] in the selected range",
            profile.data.default_venue()
        )));
    }
    Ok(result)
}

/// Resolve + load + backtest, synchronously: [`slice_profile`] run by
/// `vike_backtest::harness::run_backtest`. Pure logic; call it off the UI thread via
/// `vike_studio_core::spawn_outcome`.
pub fn run_slice(
    spec: &StrategySpec,
    files: &StrategyFiles,
    slice: &DataSlice,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    costs: Option<&StrategyEngineConfigWire>,
) -> RunOutcome {
    run_slice_profile(&slice_profile(spec, files, slice, costs, store.as_ref())?, store)
}

// ⚠ `compare_all_slice`, `spawn_compare_all` and `CompareOutcome` stood here and are DELETED: the
// Saved pane's "Compare all" ran the simulator in the GUI process on `StrategyEngineConfig::default()` (zero
// fee), a live second backtest path decision 0078 had ruled out, so its table ranked at zero cost
// while a Run of the same row charged the lane fee. It is one hub `RunSlice` per saved row now
// (`crates/vike-studio/src/backend/remote.rs`'s `spawn_compare_remote`).
