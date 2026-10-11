//! The CROSS-EXCHANGE maker MOUNT — stand a [`vike_mm::XemmMaker`] up on the production live core,
//! resting on one venue and hedging on another.
//!
//! A SIBLING of the single-venue maker mount, not an extension: [`crate::MakerMountConfig`] is ONE
//! `(venue, symbol)` and its paper builder's tripwire (`spec.legs.is_empty()`, in
//! [`crate::build_paper_strategy_core_with`]) refuses multi-symbol mounts, because a single-symbol
//! `PaperExecutionClient` stamps ITS OWN symbol on every fill — a two-leg rehearsal would book both
//! legs under one symbol and conceal a live misroute.
//!
//! This builder meets that by CONSTRUCTION: **two engines, two single-symbol paper books, one per
//! venue** ([`vike_core::spawn_core_multi`] takes one client TYPE, not one instance), each with its
//! own `Account`/`RiskGate`/[`vike_model::FeeSchedule`], so a misrouted order lands in the WRONG
//! log. Not `vike_paper::MultiPaperExecutionClient`: it routes by `request.symbol` only, never
//! `request.venue`.
//!
//! # ⚠ The live path needs FEEDS the caller wires
//!
//! Like [`crate::build_live_maker_core`], nothing here spawns a feed. Three inbound streams on the
//! one `CoreHandle`:
//!
//! 1. the MAKER venue's L1/L2 — `passive_clamp`'s anchor;
//! 2. the TAKER venue's L1 — delivered to `on_reference_quote` via [`vike_core::MountLeg::at`];
//! 3. a periodic `LiveSchedule` under the mount's id — the ONE lane that fires when both venues go
//!    silent; without it a total outage leaves the quotes resting.

mod config;
mod live;
mod paper;

pub(crate) use config::xemm_mount_legs;
pub use config::{XemmConfigError, XemmMountConfig, build_xemm_maker};
pub use live::{LiveXemmMount, XemmMountError, build_live_xemm_core};
pub use paper::{PaperXemmMount, build_paper_xemm_core, build_paper_xemm_core_with};

#[cfg(test)]
use crate::node::WiredMarket;
#[cfg(test)]
use crate::run::PaperHalt;
#[cfg(test)]
use paper::xemm_paper_books;
#[cfg(test)]
use vike_core::MountLeg;

#[path = "xemm_tests.rs"]
#[cfg(test)]
mod xemm_tests;
