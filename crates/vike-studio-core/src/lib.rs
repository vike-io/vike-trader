//! Headless Studio vocabulary and study host, with no GUI and NO ENGINE: what a Studio run is asked
//! and what it answers, the wire conversions between those and the backtesthub's DTOs, and the study
//! runner. `vike-studio` (the egui shell the desktop mounts) and `vike-backtesthub` (the server that
//! runs every Studio backtest) both link it.
//!
//! ⚠ **The Run PIPELINE is not here since 2026-11-10.** It runs the harness, and this crate is what
//! the desktop links: the thin client links no engine
//! (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s amendment of that
//! day; `crates/vike-ops/tests/architecture/named_run_closure_gate.rs`'s thin-client fence holds it).
//! `run_slice`, the sweep and walk-forward runners, the `*_local` wire entries, the cost stamp,
//! `harness_sim::HarnessSim` and the `vike-study` CLI are `vike_backtesthub`'s now (`studio_run`,
//! `harness_sim`, `study_cli`).
//!
//! The module map (each module's own `//!` carries its argument):
//!
//! - [`spec`] — [`StrategySpec`]: Rhai source, a strategy file of the server's folder, or a built strategy file.
//! - `run` (private; its names are re-exported below) — [`DataSlice`] (venue, symbol LIST and a
//!   `vike_model::run_request::replay::ReplayKind`: bars, or the recorded quote/trade/book
//!   ticks), the store catalog folds the picker lists, and what a run answers:
//!   [`RunOutcome`]/[`RunError`], [`StudioParamscan`] and [`spawn_outcome`], the one worker spawn.
//! - [`cost_model`] — what a run's cost stamp declares beyond its profile: the unpriced terms and
//!   the realised fill mix.
//! - [`wire_run`] — the compute-to-data BOUNDARY's conversions: `Wire*` DTO ↔ run type, both halves.
//! - [`listing`] — the RUNS under `<project>/user_data/runs/` and the STUDIES that produce them,
//!   enumerated with every failure NAMED. Reading half only.
//! - [`study_run`] / [`study_dispatch`] — running a study and leaving a run behind, and the one
//!   `match` on a study's tier a SURFACE calls. `docs/decisions/0031-the-study-runner-lives-above-run-persistence.md`
//!   carries why the runner lives here. The HOST hands the backtest seam ([`StudyRunPlan`]'s `sim`).
//! - [`rhai_study`] — the INTERPRETED study tier.
//!
//! The default build is DataFusion-free; `crates/vike-studio-core/CLAUDE.md` carries what keeps it so.

pub mod cost_model;
pub mod listing;
pub mod rhai_study;
mod run;
pub mod spec;
pub mod study_dispatch;
pub mod study_run;
/// The seeded walk and the Rhai strategies the unit tests and the integration binaries share
/// (`test-support` only: a default build never compiles it).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
// Moved here from `vike-datahub` by ruling 7 of
// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`.
pub mod wire_run;

pub use cost_model::{FillMix, NOT_MODELLED};
pub use rhai_study::{RhaiStudy, StudyFit};
pub use run::{
    DataSlice, ParamscanEntry, RunError, RunOutcome, SeriesLists, StudioParamscan,
    UNREPLAYABLE_TICK_KIND, bar_series, depth_only_series, series_lists, spawn_outcome,
    tick_series,
};
pub use spec::{StrategySpec, empty_params, params_from_rows};
pub use study_dispatch::{
    RECIPE_EXT, StudyRunPlan, empty_recipe, read_recipe, recipes, run_study_plan, spawn_study,
};
pub use study_run::{
    STUDY_METRICS_NOTE, STUDY_RUN_KIND, StudyPersistError, StudyRun, StudyRunError,
    StudyRunRequest, run_study, run_study_with,
};
pub use wire_run::{
    from_wire_paramscan_result, from_wire_walkforward_result, run_error_from_wire, to_data_slice,
    to_strategy_spec, to_wire_paramscan_result, to_wire_slice, to_wire_spec,
    to_wire_walkforward_result,
};
