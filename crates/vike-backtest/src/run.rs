//! One profile to one result: the single-run role of the backtest harness.
//!
//! A run is [`profile`] (the `ProfileBacktest` TOML config, its parse and validation, the strategy
//! FILE it names), [`strategy_builder`] (`ProfileStrategy::build`, the one door a run builds its
//! strategy through), the dispatcher (`run_backtest`, bar and tick modes, re-exported below from its
//! private child `dispatch`), [`data_plan`] (what a run is about to READ, answered before it reads
//! it: the `data.explain` plan, the `data.require_coverage` gate and the `data.universe` rule, over
//! ONE resolution of the slice), [`tick_replay`] (`HistoryStore -> Vec<Tick> -> run_ticks`), and
//! [`costs`] (what a run charges each fill: the strategy's `commission` and `slippage` params, else what
//! it states, else none), [`equity_log`] (the event log a saved bar run keeps in place of its whole curve, and
//! the curve's rebuild from it), [`report`] (the profile-coupled half of the `TradingResults` summary; the struct itself is
//! `vike_analytics::results::TradingResults`). [`profile_surface`] is the profile's TOML schema as
//! DATA, derived from the profile structs rather than restated (feature `profile-schema` only: its
//! test renders it and `release.yml` copies the committed fixture, so nothing that ships compiles it).
//!
//! A `pub mod` child is named by its path (`run::profile::ProfileBacktest`). The private children
//! (`dispatch`, `kept_rows`, `strategy_source`) are named through the re-exports below, their one
//! path: `run::run_backtest`, `run::KeptRows`, `run::StrategySource`.

// What a run charges each fill: the strategy's commission and slippage params (decision 0269).
pub mod costs;
pub mod data_plan;
pub mod equity_log;
// The dispatcher: `run_backtest` and the loaders and guards every door shares.
mod dispatch;
// The kept instrument rows a run reads its per-symbol facts from.
mod kept_rows;
pub mod profile;
#[cfg(feature = "profile-schema")]
pub mod profile_surface;
// Every refusal of a profile, one typed variant each, with its id and its one text.
pub mod refusals;
pub mod report;
pub mod strategy_builder;
// The strategy file's text as read.
mod strategy_source;
pub mod tick_replay;

pub use dispatch::{
    FundingAccrual, FundingJoinStats, FundingSchedule, RunBars, RunLedgerCoin, is_cancelled,
    names_a_perpetual, refuse_foreign_quoted_picks, refuse_foreign_quoted_symbols,
    refuse_inverse_contracts, run_backtest, run_backtest_cancellable, run_backtest_logged,
    run_ledger_coin, window_join_funding,
};
pub(crate) use dispatch::{
    NO_FUNDING_HISTORY, attaches_funding, bar_engine_params, cancelled_run, ledger_label,
    load_profile_bars, load_series_bars, profile_run_ledger_coin, refuse_after_failed_properties,
    refuse_off_ledger_kept, refuse_off_ledger_series, run_backtest_over_bars,
    run_backtest_over_tapes, source_label, tick_walk_tapes,
};
pub use kept_rows::KeptRows;
pub use strategy_source::StrategySource;
