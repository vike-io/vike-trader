//! The strategy-mount layer (the former `vike-run` crate, docs/decisions/0098): mount a strategy on
//! the PRODUCTION live core and feed it. Public items are re-exported at the crate root.
//!
//! First mount: the Avellaneda–Stoikov [`vike_mm::SpreadMaker`] on a Polymarket token through the
//! REAL runtime ([`vike_core::spawn_core`] + [`vike_core::StrategyMount`]) with the **paper
//! exchange** ([`vike_paper::PaperExecutionClient`]) as the `ExecutionClient`: the live-core path a
//! venue mount uses, with ZERO real-money / credential / geo risk. Wiring: [`MakerMountConfig`] →
//! [`build_paper_maker_core`] → [`MakerSink`] (a [`vike_data::LiveDataSink`] onto the tick lane).
//!
//! Why a tick→bar bridge: the maker QUOTES on ticks, but the paper client FILLS on CLOSED BARS
//! (next-open, the R7 paper law) and Polymarket serves no candles, so [`TickBarSynthesizer`] folds
//! the feed mid into event-time [`vike_model::Bar`]s. No seam changes; a resting quote fills when
//! the market crosses it within a bar.
//!
//! The LIVE feed is `vike-tradehub`'s: it wires each venue into the SAME [`MakerSink`] the tests
//! script (`crates/vike-tradehub/src/feeds.rs`). This crate names no venue bridge: registry and
//! markets arrive as `NodeConfig::registry` / `NodeConfig::markets` (docs/decisions/0096).
//!
//! ⚠ The toxicity producer (`ToxicityEmitter`, with the `polymarket_maker_paper`/`ibkr_mount` bins)
//! was DELETED, measured unused on the latency box, the CI box, the dev box and the Dublin host;
//! `with_flow_toxicity` and `ToxicityAggregator` remain, so [`MakerMountConfig::toxicity`] is inert
//! until something feeds `on_flow` again.

mod config;
mod live;
mod multi;
mod paper;
mod sink;

pub use config::{MakerBreaker, MakerMountConfig, MakerSkew, MountSpec};
pub use live::{
    LiveMakerMount, build_live_maker_core, build_live_multi_strategy_core,
    build_live_strategy_core, build_live_strategy_core_with_preflight,
};
pub use multi::{
    BookFills, MultiStrategyMount, StrategyMountSpec, build_paper_multi_strategy_core_with,
};
pub use paper::{
    MakerMount, PaperHalt, PaperMountOpts, build_maker, build_paper_maker_core,
    build_paper_maker_core_with, build_paper_strategy_core_with,
};
pub use sink::{MakerSink, TickBarSynthesizer};

#[cfg(test)]
use crate::node::NodeConfig;
#[cfg(test)]
use live::fold_strategy_mount;
#[cfg(test)]
use paper::paper_client_for;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use vike_model::{Bar, HorizonMode, PriceDomain, VarianceMode};
#[cfg(test)]
use vike_model::{RefreshTolerance, RewardParams, ToxicityParams};

#[path = "run_tests.rs"]
#[cfg(test)]
mod run_tests;
