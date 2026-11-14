//! The Studio's Run pipeline, on the server that runs it. **A Studio run IS a profile run**: the
//! picked `vike_studio_core::StrategySpec` becomes the profile's `[strategy]` table ([`strategy_config`]),
//! the picked `vike_studio_core::DataSlice` its `[data]`
//! (`vike_backtest::harness::ProfileBacktest::for_slice`), and the harness runs it — a single run
//! through `run_backtest`, a sweep through its grid search and `overfit_stats`, a walk-forward through
//! `run_walkforward_optimized`. So the Studio and `vike-cli backtest run` cannot answer one slice two
//! ways.
//!
//! - **Strategy source** — a Rhai editor buffer is a `.rhai` source the run holds, a strategy FILE
//!   is read from the hub's `strategies/` folder (`.rhai`) or built where `StrategyFiles` says
//!   (`.rs`: a hub routes it to a sandboxed child), a built artifact rides the table's builder.
//! - **The wire entries** — `wire`'s [`run_slice_local`], [`run_paramscan_local`],
//!   [`run_walkforward_local`] and [`run_study_local`] are what `crate::backtesthub_server` answers the
//!   Studio verbs and `RunStudy` with.
//!
//! ⚠ **This was `vike-studio-core`'s private `run` module until 2026-11-10.** The vocabulary it speaks
//! (`DataSlice`, `StrategySpec`, `RunOutcome`, `StudioParamscan`) stayed there, and the pipeline came
//! here because it runs the harness and `vike-studio-core` is what the desktop links: the thin client
//! links no engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendment of that day, held by `crates/vike-ops/tests/architecture/named_run_closure_gate.rs`'s
//! thin-client fence).

mod paramscan;
mod single;
mod strategy;
mod walkforward;
mod wire;

pub(crate) use paramscan::paramscan_profile;
pub use paramscan::{run_paramscan_slice, run_paramscan_slice_with_params};
pub use single::run_slice;
pub(crate) use single::{data_error, run_slice_profile, slice_profile};
pub use strategy::{
    EDITOR_BUFFER_FILE, RustFileBuild, StrategyFileBuild, StrategyFiles, build_strategy,
    build_strategy_with, loaded_file_builder, strategy_config, strategy_files_dir,
};
pub(crate) use walkforward::walkforward_profile;
pub use walkforward::{
    run_walkforward_slice, run_walkforward_slice_searching, run_walkforward_slice_with_params,
};
pub use wire::{
    run_paramscan_local, run_slice_local, run_study_local, run_walkforward_local,
    to_wire_cost_model,
};

#[cfg(test)]
mod tests;
