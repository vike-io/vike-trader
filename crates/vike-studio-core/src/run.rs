//! The Run pipeline — headless (no egui) so CI tests it directly. Resolve the [`StrategySpec`]
//! (compile the Rhai buffer, or build a native registry strategy), load the picked [`DataSlice`]
//! from the `HistStore`, backtest it, return a result.
//!
//! Two axes widened past the original "one Rhai script over one bar series":
//!
//! - **Strategy source** — every runner takes a [`StrategySpec`] (`spec.rs`) instead of a `&str`
//!   of Rhai. [`build_strategy`] is the ONE place either source becomes a
//!   `Box<dyn Strategy<SimBroker>>`; the runners below are source-agnostic from there on.
//! - **Data slice** — [`DataSlice`] carries a symbol LIST and a [`SliceKind`]. `Bars` is the
//!   original `load_bars` + `StrategyEngine` path; `Ticks` routes to `vike_backtest::hist_replay::
//!   replay_ticks` over the store's recorded quote/trade/book series instead. Single-symbol bar
//!   slices keep their exact previous behavior — [`DataSlice::bars`] is the constructor for them.

#[cfg(doc)]
use crate::spec::StrategySpec;

mod paramscan;
mod single;
mod slice;
mod strategy;
mod walkforward;

use paramscan::cartesian;
pub use paramscan::{
    ParamscanEntry, StudioParamscan, run_paramscan_slice, run_paramscan_slice_with_params,
};
use single::run_tick_slice;
pub use single::{
    CompareOutcome, compare_all_slice, load_slice_bars, run_slice, spawn_compare_all, spawn_outcome,
};
pub use slice::{
    DataSlice, REPLAYABLE_TICK_KINDS, SeriesLists, SliceKind, UNREPLAYABLE_TICK_KIND, bar_series,
    depth_only_series, series_lists, tick_series,
};
pub use strategy::{
    RunError, RunOutcome, build_strategy, build_strategy_in, build_strategy_with,
    build_strategy_with_in, plugins_dir,
};
pub use walkforward::{
    WindowSearchPlan, run_walkforward_slice, run_walkforward_slice_searching,
    run_walkforward_slice_with_params,
};

#[cfg(test)]
mod tests;
