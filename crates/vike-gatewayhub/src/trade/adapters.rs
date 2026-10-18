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
//! * [`started_keys`] — the one-way fingerprint of the key set each running adapter started with, so the
//!   arming table can name an account whose key in the database has changed since (a warning, never a restart).
//! * [`symbol_grid`] — the grid-source declaration read off a registry row.
//! * [`symbol_facts`] — the venue facts of any symbol from the public instrument list (`TradeLane::symbol_facts`).
//! * [`keyed_facts`] — the per-key bounds (keep, single flight, budget, table size) of a venue with no list to
//!   keep: [`ig_facts`] and `ibkr_facts` are its two users.
//! * [`leverage_caps`] — hyperliquid's confirmed leverage cap, kept per `(account, tier, coin, stated leverage)`
//!   for the adapter's lifetime so an `AdapterStart` that finds the adapter running re-reads nothing.
//! * [`ig_facts`] — IG's per-epic facts, bounded, over the asking account's own login (IG has no list).
//! * `ibkr_facts` (behind this crate's `ibkr` feature) — the bounds on IBKR's per-name lookups, which ask the
//!   account's gateway because IBKR has no list.
//! * [`agent_watch`] — hyperliquid's API-wallet approval, checked at a start and hourly, and told to the
//!   operator (a log line and one alert a day) when it is ending or gone. A warning only.

pub mod agent_watch;
pub mod arming;
pub mod book_identity;
pub mod bridge_trade;
pub(crate) mod contract;
pub(crate) mod exclusive;
#[cfg(feature = "ibkr")]
pub mod ibkr_facts;
pub mod ig_facts;
pub mod keyed_facts;
pub mod leverage_caps;
pub(crate) mod orders_held;
pub mod probe;
pub mod refreshed;
pub mod registry;
pub mod server_time;
pub mod snapshot;
pub(crate) mod started_keys;
pub mod startup;
pub mod symbol_facts;
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
