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
//!   ephemeral loopback port over those lanes, with ready-made `GatewayLink`s.
//! * `trade` (feature `trade`) — `trade::ScriptedTrade`, a `TradeLane` whose adapters record every call
//!   the order sessions make of them and play venue events on demand, so the wave-2 trade lane (and the
//!   tradehub's remote backend) are exercised over the real wire against answers a test wrote.
//!
//! # Not the real lanes
//!
//! The kit links no bridge and reads no store, environment or credential, so it composes with the `data`
//! feature or stands alone. It is also not the real lane with a double behind it: the real
//! `BridgeData` and `VenueRest` (feature `data`) have their own suites, which drive the production
//! tables. The kit's job is the OTHER side of the wire.

pub mod data;
pub mod events;
pub mod gateway;
pub mod rest;
#[cfg(feature = "trade")]
pub mod trade;
