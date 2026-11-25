//! **THE VENUE HALF of the fold, in the daemon that owns the venue connections** (wave 2: consolidated as
//! `venue_half` in `vike-tradehub-core` by W9a, moved here whole by W9b; `docs/decisions/0165-*`): every
//! module that needs a bridge's `VenueAdapter` trait object or a credential value.
//!
//! The tradehub's fold (`vike-tradehub-core`) decides WHICH account runs at WHICH tier and folds what a
//! venue hands back into an engine; it reads the [`ArmingTable`](vike_gatewayhub_client::proto::ArmingTableWire)
//! this half produces and drives the start of one adapter through the wire. Nothing of this folder
//! crosses to the tradehub but words, grids, opaque handles and static facts: never a `Resolution`, an
//! `AdapterInputs` or an `AccountCredentials`.
//!
//! This module is written with NO dependency on `crate::server` (decision 0165 verdict 1), and with none
//! on `vike-tradehub-core`: where the moved code named the fold's own types it names the daemon's
//! ([`snapshot::TradeLaneSnapshot`] for the three fields it read of `AdapterPolicy`, [`probe`] for the preflight
//! probe outcomes) or the wire's mirrors.
//!
//! * [`bridge_trade`] — `TradeLaneBridge`, the [`TradeLane`](crate::trade::lane::TradeLane) over the registry
//!   (`LocalBackend` moved whole).
//! * [`refreshed`] — `RefreshedTrade`, the production lane: a `TradeLaneBridge` re-read from the store at each
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
//! * [`symbol_facts_specs`] — the table of each list-answered venue's spec
//!   (`vike_bridge_core::symbol_properties::SymbolPropertiesListSpec`), which `symbol_facts` is generic over.
//! * [`keyed_facts`] — `SymbolPropertiesCacheKeyed<S>`, the per-key bounds (keep, single flight, budget, table size) of a venue with
//!   no list to keep, over its bridge's spec: ig's epics (`vike_ig::symbol_properties`) and ibkr's names
//!   (`vike_ibkr::symbol_properties`, which also assembles a converting answer of a kept grid and a live rate).
//! * [`leverage_capabilities`] — hyperliquid's confirmed leverage cap, kept per `(account, tier, coin, stated leverage)`
//!   for the adapter's lifetime so an `AdapterStart` that finds the adapter running re-reads nothing.
//! * [`posted_leverage`] — the lane's READ-ONLY half of an account's leverage on a venue that POSTS one
//!   (bybit, okx, binance; decision 0182): the probe before the bridge starts, the profile that carries the
//!   figure into the bridge's one write, and the poll that sees the venue hold it.
//! * [`agent_watch`] — hyperliquid's API-wallet approval, checked at a start and hourly, and told to the
//!   operator (a log line and one alert a day) when it is ending or gone. A warning only.
//! * [`quote_rate_asks`] — the guard in front of a costly rate ask (one ask per account, a hub cap, a short
//!   memory), over the bridge's spec (cTrader's).
//! * [`link_watch`] — `VenueLinkWatch`, the one [`LinkWatch`](crate::trade::lane::VenueLinkWatchSource) over a bridge's
//!   declared `VenueLinkProbe` (IBKR's gateway, FXCM's ForexConnect session), a fresh read of the account rows and the
//!   venue's FX-week calendar.
//! * [`venue_link`] — what the status verb says of an account's path to its venue: each bridge's cell read into
//!   the one reading (`vike_bridge_core::venue_link::VenueLinkState`) and its one translator to the wire.

pub mod agent_watch;
pub mod arming;
pub(crate) mod assumed_currency;
pub mod book_identity;
pub mod bridge_trade;
pub(crate) mod contract;
pub(crate) mod exclusive;
// IBKR's cash-settled futures roots at a future's admission: the table created at its first need, never at a start.
pub mod ibkr_roots;
pub mod keyed_facts;
pub mod leverage_capabilities;
pub mod link_watch;
pub(crate) mod orders_held;
pub mod posted_leverage;
pub mod probe;
pub mod quote_rate_asks;
pub mod refreshed;
pub mod registry;
pub mod server_time;
pub mod snapshot;
pub(crate) mod started_keys;
pub mod startup;
pub mod symbol_facts;
pub(crate) mod symbol_facts_specs;
pub mod symbol_grid;
pub(crate) mod venue_link;

/// The test rig: a `TradeLaneBridge` over a planted registry.
#[cfg(test)]
pub(crate) mod testutil;

/// The generic fold over a `VenueRow::Adapter` (`contract.rs`), driven through PLANTED venues: the
/// arming projection, the pre-connect refusal, the exclusive claim, the recon-trigger routing and
/// the outcome fold, with no real bridge.
#[cfg(test)]
mod contract_tests;

/// `TradeLaneBridge`'s own answers (the wire-shaped ones), over planted rows.
#[cfg(test)]
mod bridge_trade_tests;
