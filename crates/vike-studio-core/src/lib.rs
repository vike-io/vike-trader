//! Headless Studio backend: the Run pipeline (resolve → backtest → score over a `HistStore`),
//! extracted from `vike-studio` so headless consumers — the in-app AI loop (`vike-ai`) and the
//! `vike-backend` daemon — reuse it without pulling in egui. No GUI here.
//!
//! The module map (each module's own `//!` carries its argument):
//!
//! - [`spec`] — [`StrategySpec`]: Rhai source, a native registry strategy, or a built plugin.
//! - `run` (private; its names are re-exported below) — [`DataSlice`] (venue, symbol LIST and a
//!   [`SliceKind`]: bars, or the recorded quote/trade/book ticks replayed through
//!   `vike_backtest::hist_replay`) and the runners over it: single run, walk-forward, sweep,
//!   compare-all.
//! - [`cost_model`] — what a run is COSTED under, derived from the slice the DTO door carries.
//! - [`wire_run`] — the compute-to-data BOUNDARY: `Wire*` DTO ↔ run-type conversions and the
//!   `*_local` runners the compute daemon serves `RunSlice`/`RunSweep`/`RunWalkforward` with.
//! - [`user_strategies`] — a user's strategies are FILES under `<project>/user_data/strategies/`;
//!   the loader and the preset resolver. Here rather than in `vike-studio` because a headless
//!   caller must read them too.
//! - [`listing`] — the RUNS under `<project>/user_data/runs/` and the STUDIES that produce them,
//!   enumerated with every failure NAMED. Reading half only.
//! - [`study_run`] / [`study_dispatch`] — running a study and leaving a run behind, and the one
//!   `match` on a study's tier a SURFACE calls. `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`
//!   carries why the runner lives here.
//! - [`rhai_study`] — the INTERPRETED study tier; [`harness_sim`] — the one `StudySim`.
//! - [`ml`] — the INFERENCE half only: load a saved model from its TEXT, score rows, report which
//!   input mattered. It fits nothing (no trainer binary ships off Linux).
//! - `study_cli` (feature `study-cli`) — the `vike-study` runner that can FIT, as a library fn.
//!
//! The default build is DataFusion-free; `crates/vike-studio-core/CLAUDE.md` carries what keeps it so.

pub mod cost_model;
pub mod harness_sim;
pub mod listing;
pub mod ml;
pub mod rhai_study;
mod run;
pub mod spec;
// Gated exactly as the bin is (`required-features = ["study-cli"]`): the `studio-standalone` lane
// greps this crate's DEFAULT normal-dep tree for a datafusion crate, so this stays invisible.
#[cfg(feature = "study-cli")]
pub mod study_cli;
pub mod study_dispatch;
pub mod study_run;
#[cfg(test)]
mod test_support;
pub mod user_strategies;
// Moved here from `vike-datahub` by ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`.
pub mod wire_run;

pub use cost_model::{CostModel, CostSource, FillMix, NOT_MODELLED};
pub use ml::{FeatureWeight, ModelError, ScoringModel};
pub use rhai_study::{RhaiStudy, StudyFit};
pub use run::{
    CompareOutcome, DataSlice, ParamscanEntry, REPLAYABLE_TICK_KINDS, RunError, RunOutcome,
    SeriesLists, SliceKind, StudioParamscan, UNREPLAYABLE_TICK_KIND, WindowSearchPlan, bar_series,
    build_strategy, build_strategy_in, build_strategy_with, build_strategy_with_in,
    compare_all_slice, depth_only_series, load_slice_bars, plugins_dir, run_paramscan_slice,
    run_paramscan_slice_with_params, run_slice, run_walkforward_slice,
    run_walkforward_slice_searching, run_walkforward_slice_with_params, series_lists,
    spawn_compare_all, spawn_outcome, tick_series,
};
pub use spec::{
    StrategySpec, empty_params, native_strategies, params_from_rows, params_with_overrides,
};
pub use study_dispatch::{
    RECIPE_EXT, StudyRunPlan, empty_recipe, read_recipe, recipes, run_study_plan, spawn_study,
};
pub use study_run::{
    STUDY_METRICS_NOTE, STUDY_RUN_KIND, StudyPersistError, StudyRun, StudyRunError,
    StudyRunRequest, run_study, run_study_with,
};
pub use wire_run::{
    engine_params_for, run_error_to_wire, run_slice_local, run_study_local, run_sweep_local,
    run_walkforward_local, to_data_slice, to_engine_params, to_strategy_spec, to_wire_cost_model,
    to_wire_paramscan_result, to_wire_walkforward_result,
};
