//! The Run VOCABULARY — what a Studio run is ASKED (a [`DataSlice`] of a `ReplayKind`) and what it
//! ANSWERS (a [`RunOutcome`], a [`StudioParamscan`]), plus the store catalog folds the slice picker
//! lists. Engine-free: **a Studio run IS a profile run on the backtest server**, and the pipeline
//! that turns the picked [`StrategySpec`] and slice into that profile and runs it —
//! `slice_profile`, the single run, the grid search with `overfit_stats`, the walk-forward — is
//! `vike_backtesthub::studio_run` since 2026-11-10, beside the daemon that serves it. The desktop
//! links this crate and no engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendment of that day).
//!
//! - **Data slice** — [`DataSlice`] carries a symbol LIST and a `ReplayKind`: bars at an interval,
//!   or the recorded quote/trade/book ticks the profile's tick lane replays.
//! - **Outcome** — [`RunError`]/[`RunOutcome`], [`StudioParamscan`], and [`spawn_outcome`], the one
//!   worker spawn the Studio waits on.

#[cfg(doc)]
use crate::spec::StrategySpec;

mod outcome;
mod slice;

pub use outcome::{ParamscanEntry, RunError, RunOutcome, StudioParamscan, spawn_outcome};
pub use slice::{
    DataSlice, SeriesLists, UNREPLAYABLE_TICK_KIND, bar_series, depth_only_series, series_lists,
    tick_series,
};
