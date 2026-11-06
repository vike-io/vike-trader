//! vike-backtest — the backtest HARNESS over the `vike-sim` simulator.
//!
//! A [`harness::BacktestProfile`] (TOML) names a data slice, engine costs and a registry-resolved
//! strategy; the harness loads the slice from a `HistStore` ([`hist_replay`] for ticks), runs it
//! through `vike_sim::StrategyEngine`, and composes the result into a report. Around that: parameter
//! sweeps and searches (`harness::sweep`, [`search`] with its methods, [`trial_ledger`]) and
//! walk-forward ([`walkforward`]).
//!
//! ⚠ **This is a plain LIBRARY, with no feature and no DataFusion.** The backtesthub, the
//! `backtest` CLI and the Polymarket research bins that used to sit here (`backtesthub_server`,
//! `backtest_cli`, `research`, `starter`, `binutil`, `bin/`) are `vike-backtesthub`'s now (layer 65,
//! beside the other daemons), together with the `datafusion-store` and `venue-fetch` features that
//! existed only to keep their weight out of this crate's consumers. The concrete store engine
//! (`vike_store::DataFusionHist`) reaches this crate as a DEV dependency alone.
//!
//! ⚠ **The SIMULATOR is not in this crate any more.** `engine` (`StrategyEngine`/`SimBroker`),
//! `vector_engine`, `impact`, `latency`, `queue_model`, `schedule`, `timeframe`, `bar_buffer` and
//! `ref_strategies` left for `vike-sim` (layer 25) —
//! docs/decisions/0087-the-simulator-leaves-the-harness.md. Spell them `vike_sim::…`, from inside
//! this crate as well as outside it: nothing here re-exports them.
//!
//! ⚠ **Nothing from another crate is re-exported here either**, and that is the other half of the
//! same change. This file used to carry `pub use` blocks for eight `vike_analytics` modules, four
//! `vike_fills` modules and their flat names, `BacktestResult`, and a "backward compatibility"
//! `vike_model::{Strategy, consolidate_quotes, consolidate_trades}` — each one a second name for
//! something that lives one crate down, which the root `CLAUDE.md`'s one-name rule forbids. Every
//! caller names the owner now: `vike_analytics::{metrics, report, overfit, …}`,
//! `vike_analytics::BacktestResult`, `vike_fills::{broker_sim, fill_model, …}`. (The same rule
//! retired `binutil`'s argv re-export, `harness::BuyHold` and the harness's second `report` alias.)
//! The one exception the rule allows — "the consumer cannot name the canonical crate" — applies to
//! no consumer: every one ranks above `vike-analytics` (15) and `vike-fills` (20).

#![warn(unreachable_pub)]

// What a run is about to READ, answered before it reads it: the `data.explain` plan, the
// `data.require_coverage` gate and the `data.universe` rule, over ONE resolution of the slice
// (`run_fingerprint::planned_series_ids`, which the run ADDRESS already needed). Names
// `BacktestProfile` and the `HistStore` trait — the same level `vike-backtesthub`'s backtesthub sits
// at, which is what lets the REMOTE route answer these too.
pub mod data_plan;
pub mod harness; // BacktestNode-lite: config-driven end-to-end backtest harness
pub mod hist_replay; // tick-replay Phase 1: HistStore -> Vec<Tick> -> run_ticks loader
// The run profile's TOML schema as DATA, DERIVED from the `harness/profile.rs` and
// `harness/profile/*.rs` sources by `include_str!` rather than restated.
pub mod profile_surface;
pub mod run_fingerprint; // what a run's INPUTS were: the data slice as the store held it, and the address over it
pub mod search; // the search family: Euler core + euler, tpe, genetic, select, trials, objective
// What a parameter SEARCH leaves behind — DOCUMENTS, compiled and tested unconditionally like the
// rest of the harness now; the RECORDER that writes them lives at `search::trials`.
pub mod trial_ledger;
pub mod walkforward;
// `BacktestResult` -> the `WireRunResult` DTO, needing `vike-datahub-client`. It MOVED here from
// `vike_studio_core::wire_run` when the named run needed the same rendering from a crate that
// cannot name the Studio (see the module doc).
pub mod wire_result;
// The NAMED RUN's server half (`docs/decisions/0064-a-named-run-carries-no-source.md`) — the ONE
// run path in this crate that does not call `harness::registry::strategy_by_name`, because the
// resolution happens in a crate that cannot name `vike-script`. `vike-backtesthub`'s backtesthub is
// its only caller: it needs the wire DTOs, the store trait and the generated user roster.
pub mod named_run;
