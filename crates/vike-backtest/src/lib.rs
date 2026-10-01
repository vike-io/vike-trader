//! vike-backtest — the backtest HARNESS over the `vike-sim` simulator.
//!
//! A [`harness::BacktestProfile`] (TOML) names a data slice, engine costs and a registry-resolved
//! strategy; the harness loads the slice from a `HistStore` ([`hist_replay`] for ticks), runs it
//! through `vike_sim::StrategyEngine`, and composes the result into a report. Around that: parameter
//! sweeps and searches (`harness::{sweep, euler, tpe, genetic}`, [`search`], [`trial_ledger`]),
//! walk-forward ([`walkforward`]), the compute daemon (`compute_server`) and the five bins.
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

pub mod binutil; // shared bin glue: the repo-root hist-store default + stats provenance
// What a run is about to READ, answered before it reads it: the `data.explain` plan, the
// `data.require_coverage` gate and the `data.universe` rule, over ONE resolution of the slice
// (`run_fingerprint::planned_series_ids`, which the run ADDRESS already needed). Names
// `BacktestProfile` and the `HistStore` trait — the same level `compute_server` sits at, which is
// what lets the REMOTE route answer these too.
pub mod data_plan;
pub mod harness; // BacktestNode-lite: config-driven end-to-end backtest harness (Task 1: BacktestProfile)
pub mod hist_replay; // tick-replay Phase 1: HistStore -> Vec<Tick> -> run_ticks loader
// The COMPUTE daemon `vike-backend backtest --addr` runs (ruling 7): the seven verbs that RUN
// something, over the same node protocol `vike-datahub` serves the store on. The server holds the
// store as `Arc<dyn HistStore>` and names no concrete backend — only the DAEMON ARM in
// `backtest_cli` opens a `DataFusionHist`, which is why this module compiles in a DEFAULT build
// (the 2026-09-27 feature collapse made the whole harness one) while `backtest_cli` itself stays
// behind `datafusion-store` below.
pub mod compute_server;
pub mod objective; // pluggable sweep-ranking objectives over BacktestReport — feature-free like report
// The run profile's TOML schema as DATA, DERIVED from `harness/profile.rs` by `include_str!`
// rather than restated.
pub mod profile_surface;
pub mod run_fingerprint; // what a run's INPUTS were: the data slice as the store held it, and the address over it
pub mod search; // Euler (successive-halving) parameter search — pure core, feature-free like objective
// What a parameter SEARCH leaves behind — DOCUMENTS, compiled and tested unconditionally like the
// rest of the harness now; the RECORDER that writes them lives at `harness::trials`.
pub mod trial_ledger;
pub mod walkforward;
// `BacktestResult` -> the `WireRunResult` DTO, needing `vike-datahub-client`. It MOVED here from
// `vike_studio_core::wire_run` when the named run needed the same rendering from a crate that
// cannot name the Studio (see the module doc).
pub mod wire_result;
// The NAMED RUN's server half (`docs/decisions/0064-a-named-run-carries-no-source.md`) — the ONE
// run path in this crate that does not call `harness::registry::strategy_by_name`, because the
// resolution happens in a crate that cannot name `vike-script`. `compute_server` is its only
// caller: it needs the wire DTOs, the store trait and the generated user roster.
pub mod named_run;
// The `backtest` CLI as a library function, so the bin and the `vike-backend` multicall dispatcher
// reach one copy of the DataFusion closure instead of two. Gated exactly as the bin is
// (`required-features = ["datafusion-store"]`).
// The on-chain -> CLOB matching rule for the Polymarket measurement bins: the anchor and its two
// tolerances, split out of `vike_data::cheap_np_book` so the DATA crate carries the archive fold
// and not one study's matching heuristics. Gated with the two bins that are its only callers, for
// the reason its own module doc gives.
#[cfg(feature = "datafusion-store")]
pub mod backtest_cli;
#[cfg(feature = "datafusion-store")]
pub mod cheap_np_match;
// The published starter dataset — real bars over plain HTTPS, for the boxes a venue cannot be
// reached from.
//
// ⚠ **`pub mod fetch` used to sit directly above this line, and its removal is the point of the
// change that deleted it.** That module held this crate's ONE venue call
// (`vike_binance::data::fetch_klines_range`), which made the COMPUTE plane a client of an
// exchange. History is fetched by the data plane, once, into the store; `vike-cli data hist fetch` asks
// a datahub for it. This module is what is left, and it reaches no venue at all — a plain HTTPS GET
// of a prepared dataset from a GitHub release.
//
// ⚠ So the feature gating it still reads `venue-fetch` and no longer fetches from a venue. That is
// a name that has stopped describing its contents, left deliberately rather than by oversight: the
// rename was raised and DEPRIORITISED by the owner as churn, and it moves four gate rows
// (`crates/vike/Cargo.toml`'s `backtest`, `crates/vike-ops/tests/multicall_gate.rs`'s
// `SHIPPED_TOOL_FEATURES`, and this file) for no behaviour. Whoever renames it next is holding the
// argument for it here.
#[cfg(feature = "venue-fetch")]
pub mod starter;

pub use hist_replay::{
    ReplayError, SeriesKind, SeriesRef, TickReplayConfig, merge_quote_trade, merge_ticks,
    merge_ticks_by_arrival, properties_source, replay_ticks, tick_arrival_ts, tick_local_ts,
    tick_venue_ts,
};
// The concrete-`DataFusionHist` streaming loader is behind `datafusion-store` (it names the concrete
// backend), NOT the trait-only re-export above.
#[cfg(feature = "datafusion-store")]
pub use hist_replay::replay_ticks_streaming;
pub use objective::{
    MultiMetricParams, Objective, multi_metric, multi_metric_score, trade_count_penalty,
};
