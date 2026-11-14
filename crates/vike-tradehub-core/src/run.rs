//! The strategy-mount layer (the former `vike-run` crate, docs/decisions/0098): mount ANY strategy
//! on the PRODUCTION live core and feed it. Public items are re-exported at the crate root. It names
//! no strategy: a mount is a boxed `Strategy<LiveBroker>` plus the strategy-free [`MountPlacement`], and
//! the A-S maker is one such strategy, built by its caller from its own params table
//! (the spread maker file's `SpreadMaker::from_params` over a `vike_market_making::SpreadMakerPreset`; step 10
//! of the one-broker plan moved the maker's presets out of this crate).
//!
//! The paper mount ([`build_paper_strategy_core_with`]) runs the REAL runtime
//! ([`vike_core::spawn_core`] + [`vike_core::Mount`]) with the **paper exchange**
//! ([`vike_paper::ExecutionClientPaper`]) as the `ExecutionClient`: the live-core path a venue mount
//! uses, with ZERO real-money / credential / geo risk. A feed drives it through a [`TickSink`] (a
//! [`vike_data::DataClientSink`] onto the tick lane).
//!
//! Why a tick→bar bridge: a maker QUOTES on ticks, but the paper client FILLS on CLOSED BARS
//! (next-open, the R7 paper law) and Polymarket serves no candles, so [`TickBarSynthesizer`] folds
//! the feed mid into event-time [`vike_marketdata::Bar`]s. No seam changes; a resting quote fills when
//! the market crosses it within a bar.
//!
//! The LIVE feed is `vike-tradehub`'s: it wires each venue into the SAME [`TickSink`] the tests
//! script (`crates/vike-tradehub/src/feeds.rs`). This crate names no venue bridge: registry and
//! markets are `wired_markets` over `RuntimeConfig::backend`'s arming table (docs/decisions/0096).
//! A live mount's strategy may be rebuilt at its account's own fee rate ([`FeeRearm`]).
//!
//! ⚠ The toxicity producer (`ToxicityEmitter`, with the `polymarket_maker_paper`/`ibkr_mount` bins)
//! was DELETED, measured unused on the latency box, the CI box, the dev box and the Dublin host; the maker's
//! `with_flow_toxicity` and `ToxicityAggregator` remain, inert until something feeds `on_flow` again.

mod config;
mod live;
mod multi;
pub(crate) mod paper;
mod sink;

pub use config::{MountPlacement, maker_round_trip_fee_for};
pub use live::{
    FeeRearm, StrategyCoreLive, build_live_multi_strategy_core,
    build_live_multi_strategy_core_armed, build_live_strategy_core,
    build_live_strategy_core_with_preflight, fee_rearm_mounts,
};
pub use multi::{
    MountBuild, MountMultiCore, MountMultiFills, build_paper_multi_strategy_core_with,
};
pub use paper::{
    StrategyCorePaper, StrategyCorePaperConfig, StrategyCorePaperHalt,
    build_paper_strategy_core_with,
};
pub use sink::{TickBarSynthesizer, TickSink};

#[cfg(test)]
use crate::node::RuntimeConfig;
#[cfg(test)]
use live::fold_strategy_mount;
#[cfg(test)]
use paper::paper_client_for;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use vike_marketdata::Bar;

#[path = "run_tests.rs"]
#[cfg(test)]
mod run_tests;
