//! **THE VENUE HALF of the fold, in the daemon that owns the venue connections** (wave 2: consolidated as
//! `venue_half` in `vike-tradehub-core` by W9a, moved here whole by W9b; `docs/decisions/0165-*`): every
//! module that needs a bridge's `VenueAdapter` trait object or a credential value.
//!
//! The tradehub's fold (`vike-tradehub-core`) decides WHICH account runs at WHICH tier and folds what a
//! venue hands back into an engine; it reads the [`ArmingTable`](vike_gatewayhub_client::proto::ArmingTable)
//! this half produces and drives the start of one adapter through the wire. Nothing of this folder
//! crosses to the tradehub but words, grids, opaque handles and static facts: never a `Resolution`, an
//! `AdapterInputs` or an `AccountCredentials`.
//!
//! This module is written with NO dependency on `crate::server` (decision 0165 verdict 1), and with none
//! on `vike-tradehub-core`: where the moved code named the fold's own types it names the daemon's
//! ([`snapshot::TradeSnapshot`] for the three fields it read of `AdapterPolicy`, [`probe`] for the preflight
//! probe outcomes) or the wire's mirrors.
//!
//! * [`bridge_trade`] — `BridgeTrade`, the [`TradeLane`](crate::trade::lane::TradeLane) over the registry
//!   (`LocalBackend` moved whole).
//! * [`refreshed`] — `RefreshedTrade`, the production lane: a `BridgeTrade` re-read from the store at each
//!   tradehub-driven step (W9c), so a tradehub restart picks up a rotation or an edit.
//! * [`snapshot`] — what the venue half reads of the operator's settings (accounts, venue settings, the
//!   preflight stop).
//! * [`probe`] — the outcomes of a clock read, a credential read and the network reading.
//! * [`registry`] — `VenueRow`, the roster-keyed table [`crate::registry::trade`] fills, and `row_of`.
//! * [`arming`] — the registry-driven arming projection and [`arming::arming_table`], the producer of
//!   the table the tradehub reads.
//! * [`contract`] — the generic fold over a contract row: the arming probe's refusals, the
//!   resolve-half and start-half tier interlock, the process-exclusive claim, the bridge's `start`.
//! * [`exclusive`] — the process-exclusive resource: which account holds it, and the claim.
//! * [`orders_held`] — the order hold wrapped around a held arm's bound client.
//! * [`book_identity`] — which venue book an account trades, and the identity record.
//! * [`server_time`] — the per-venue server-clock declaration table and the clock read.
//! * [`startup`] — the real preflight probes (clock, credential, network).
//! * [`symbol_grid`] — the grid-source declaration read off a registry row.

pub mod arming;
pub mod book_identity;
pub mod bridge_trade;
pub(crate) mod contract;
pub(crate) mod exclusive;
pub(crate) mod orders_held;
pub mod probe;
pub mod refreshed;
pub mod registry;
pub mod server_time;
pub mod snapshot;
pub mod startup;
pub mod symbol_grid;

/// The test rig: a `BridgeTrade` over a planted registry.
#[cfg(test)]
pub(crate) mod testutil;

/// The generic fold over a `VenueRow::Adapter` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no real bridge.
#[cfg(test)]
mod contract_tests;

/// `BridgeTrade`'s own answers (the wire-shaped ones), over planted rows.
#[cfg(test)]
mod bridge_trade_tests;
