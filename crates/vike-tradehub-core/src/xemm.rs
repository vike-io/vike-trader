//! The CROSS-EXCHANGE maker MOUNT — stand a cross-exchange maker up on the production live core,
//! resting on one venue and hedging on another.
//!
//! The maker is the CALLER's: every builder takes it as a `maker` that is handed the mount, the
//! tunables and the round trip [`validate_xemm_mount`] priced, so no strategy is named here (the
//! shipped one is the strategy file `strategies/market_making/cross_exchange_maker.rs`'s
//! `XemmMaker`).
//!
//! Its configuration is the strategy-free [`crate::MountPlacement`] every mount takes, whose ONE declared
//! leg is the hedge on the taker venue ([`vike_core::MountLeg::at`], [`xemm_mount`]), plus the
//! maker's own tunables, [`vike_model::XemmParams`] ([`xemm_params`]); [`validate_xemm_mount`]
//! refuses a pair that would be silently wrong at runtime.
//!
//! The paper rehearsal is the generic multi-leg paper builder
//! ([`crate::build_paper_multi_strategy_core_with`]), which gives the hedge leg's venue an engine
//! and a single-symbol book of its own: **two engines, two single-symbol paper books, one per
//! venue**, each with its own `Account`/`RiskGate`/[`vike_model::FeeSchedule`], so a misrouted
//! order lands in the WRONG log. Not one `vike_paper::ExecutionClientPaperMulti`: it routes by
//! `request.symbol` only, never `request.venue`, and a single-symbol book stamps ITS OWN symbol on
//! every fill — a two-leg rehearsal on one book would conceal a live misroute.
//!
//! # ⚠ The live path needs FEEDS the caller wires
//!
//! Like [`crate::build_live_strategy_core`], nothing here spawns a feed. Three inbound streams on the
//! one `CoreHandle`:
//!
//! 1. the MAKER venue's L1/L2 — `passive_clamp`'s anchor;
//! 2. the TAKER venue's L1 — delivered to `on_reference_quote` via [`vike_core::MountLeg::at`];
//! 3. a periodic `MountSchedule` under the mount's id — the ONE lane that fires when both venues go
//!    silent; without it a total outage leaves the quotes resting.

mod config;
mod live;
mod paper;

pub use config::{XemmConfigError, validate_xemm_mount, xemm_mount, xemm_params};
pub use live::{XemmCoreError, XemmCoreLive, build_live_xemm_core};
pub use paper::{XemmCorePaper, build_paper_xemm_core, build_paper_xemm_core_with};

#[cfg(test)]
use crate::node::RuntimeMarket;
#[cfg(test)]
use crate::run::StrategyCorePaperHalt;
#[cfg(test)]
use vike_core::MountLeg;

#[path = "xemm_tests.rs"]
#[cfg(test)]
mod xemm_tests;
