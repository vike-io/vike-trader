//! The TESTKIT: a scripted gatewayhub other crates' tests can dial.
//!
//! The datahub reaches every venue through `vike-gatewayhub-client` now, so its tests need something
//! on the other end of that client that is not a venue. This module is that: two scripted lanes behind
//! the REAL server, so the datahub's lanes (a feed broker client, a backfill source, a catalog
//! provider, an options poll) are exercised over the real wire, against answers a test wrote, and what
//! the daemon was asked can be read back.
//!
//! Compiled only for this crate's own tests and under the `test-support` feature, and a feature of
//! this kind belongs to a `[dev-dependencies]` edge alone
//! (`crates/vike-ops/tests/architecture/test_surface_gate.rs` refuses a shipped one): a build that
//! carries it carries doubles that answer any venue request with whatever the test said.
//!
//! * [`data`] — [`data::ScriptedData`], a `DataLane` whose venue clients are scripted `DataClient`s: a
//!   recorder of every call the server makes of them, any of the twelve `LiveDataSink` verbs played on
//!   demand, a hook inside `subscribe_*` (a seed emitted before the call returns), scripted refusals.
//! * [`rest`] — [`rest::ScriptedRest`], a `RestLane` answering klines, funding, ticks, the catalog,
//!   the option index, the options provider, a rolling family and a presence word from scripts
//!   ([`rest::Answer`] is rows, a `SourceError`, or a slow answer that honours a cancel), and recording
//!   the requests it received.
//! * [`events`] — the sink side: a script is a `Vec<vike_data::SinkCall>`, played into any sink and
//!   compared line by line, with [`events::canonical_script`] exercising all twelve verbs bit-exactly.
//! * [`gateway`] — [`gateway::LoopbackGateway`], the real `crate::server::accept::serve` on an
//!   ephemeral loopback port over those lanes, with ready-made `GatewayLink`s; its server thread is a
//!   [`gateway::ServedThread`], the stop flag and join this crate's own suite servers and the tradehub's
//!   hop rig hold too.
//! * `trade` (feature `trade`) — `trade::ScriptedTrade`, a `TradeLane` whose adapters record every call
//!   the order sessions make of them and play venue events on demand, so the wave-2 trade lane (and the
//!   tradehub's remote backend) are exercised over the real wire against answers a test wrote.
//!
//! # Not the real lanes
//!
//! The scripted lanes link no bridge and read no store, environment or credential, so the kit composes
//! with the `data` feature or stands alone (the one store it touches is [`account_store`]'s, which a
//! credential suite plants in its OWN temp directory). It is also not the real lane with a double behind it: the real
//! `BridgeData` and `VenueRest` (feature `data`) have their own suites, which drive the production
//! tables. The kit's job is the OTHER side of the wire.
//!
//! # This crate's own suites' helpers
//!
//! The rest of the kit is what SEVERAL test binaries share (another crate's or more than one of this
//! crate's groups), compiled once here instead of once per binary: [`raw_peer`] (the handshake and frames
//! with no client library), [`hop_stats`] (the hop harnesses' stamps, shapes and `HOP-LATENCY` lines, the
//! datahub's price harness included), `trade_rig` (feature `trade`: the loopback gateway over the
//! scripted trade lane, with a `TradeSession` and its events) and [`arming_fixtures`] (the credential
//! FIELDS that arm each roster venue's arm, which the roster's arming parity golden and the tradehub's
//! `venue_backend_parity` share), and [`shipped_bin`] (the deadlined spawn of a shipped hub binary this
//! crate's and the datahub's `help_cli` suites share). The lib's OWN unit tests reach the same items as
//! `crate::testkit`, so a fixture a unit suite and an integration group both need is written once here:
//! [`fake_deribit`] (the loopback book-summary endpoint the options suites count requests on),
//! [`account_store`] (the credential suites' planted settings database) and [`fixtures`] (the small
//! builders and the log capture both sides of the crate boundary used to restate). A helper only ONE
//! group uses lives in that group's directory instead, so editing it rebuilds that group and not this lib
//! (`tests/server/support/`).

pub mod account_store;
pub mod arming_fixtures;
pub mod data;
pub mod events;
pub mod fake_deribit;
pub mod fixtures;
pub mod gateway;
pub mod hop_stats;
pub mod raw_peer;
pub mod rest;
pub mod shipped_bin;
#[cfg(feature = "trade")]
pub mod trade;
#[cfg(feature = "trade")]
pub mod trade_rig;
