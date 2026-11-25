//! `vike-strategy` — the strategy-authoring layer, extracted from `vike-model` so the bottom
//! domain crate stays pure data. No strategy is compiled in here: a strategy is a FILE under
//! `<project>/strategies/` (the shipped set is `strategies/` at the repository root).
//!
//! What it holds:
//!
//! * the FRAMEWORK a file builds on: the position-executor STATE MACHINE
//!   ([`position_executor::PositionExecutor`]; the barrier arithmetic it runs is `vike_model`'s,
//!   decision 0269) and the CONTROLLER seam ([`controller::Controller`],
//!   [`controller::ControllerHarness`]). Pure, deterministic, injected-clock logic — no I/O — so it
//!   runs bit-identically on the backtest and live engines;
//! * the strategy FILE ([`file`](mod@file)): the header format, its parser, the load-time params checks
//!   against it, and the scan of the folder;
//! * the shipped-file seed ([`seed_shipped_strategies`], [`differs_from_shipped`]): it copies a
//!   release's strategy files into `<project>/strategies/`, replaces a copy nobody edited and writes
//!   the new version beside one somebody did, and runs no strategy;
//! * the prediction-market helpers the shipped files, the backtest's resolution source and the
//!   research runners share ([`strategies`]): the up/down token grammar ([`TokenId`]), the
//!   fair-value maths (`strategies::fair_value`, the resting-ask re-scoring
//!   `strategies::cheap_np_ask`) and the copy-trading conviction detector ([`SignalTracker`]).
//!
//! The file scan and the seed are the crate's only filesystem I/O.
//!
//! **Layering:** the NORMAL dependencies are `vike-model`, `vike-marketdata`, `vike-math` (the
//! compensated sum and `strategies::fair_value::trailing_sigma`'s log returns) and the file layer's
//! `serde`, `toml`, `sha2` and `vike-paths`, and `vike-utility` (the seed's atomic replace, the scan's
//! hex) — all down-only. The PARAM types the framework consumes —
//! [`vike_model::TripleBarrier`] and [`vike_model::PositionControllerParams`] — deliberately stay in
//! `vike-model` because they are transitively part of the `StrategyParams` payload the `vike-execution`
//! ingest lane carries, and `vike-execution` sits below any strategy layer.

// The framework every strategy is built on.
pub mod controller;
// A strategy FILE: its header format, the load-time params checks against it, the folder scan.
// Named `vike_strategy::file::…`, not at this root.
pub mod file;
pub mod position_executor;
// The prediction-market helpers, apart from the framework.
pub mod strategies;
// The shipped strategy files and the one seed that copies them into a project (file I/O).
mod shipped;

pub use controller::{Controller, ControllerHarness};
pub use position_executor::{
    PositionExecutor, PositionExecutorEntryKind, PositionExecutorIntent, PositionExecutorOutcome,
    PositionExecutorRefreshMode, PositionExecutorRefreshPolicy, PositionExecutorRetryPolicy,
    PositionExecutorState,
};
pub use shipped::{NEW_VERSION_SUFFIX, SeedReport, differs_from_shipped, seed_shipped_strategies};
pub use strategies::cheap_np::TokenId;
pub use strategies::sport_taker::{
    SignalCopyTrade, SignalTracker, WalletBuy, flat_stake_pnl, market_symbol, signal_symbol,
};
