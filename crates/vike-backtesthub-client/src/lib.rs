//! vike-backtesthub-client — the LIGHT client half of the backtesthub.
//!
//! The wire [`proto`]col of the verbs that RUN something, the blocking [`BacktesthubClient`], the
//! Studio DTOs ([`wire_studio`]) and the named run ([`named_run`]). Every hub reached from outside has
//! its own client (`docs/decisions/0252-the-cli-and-the-mcp-server-are-two-doors-over-one-client-and-every-hub-has-its-own-client.md`,
//! verdict 4); this one left `vike-datahub-client` with zero bytes changed on the wire, frozen by
//! `tests/backtest_plane_wire_pin.rs`.
//!
//! # Why it stays light
//!
//! It names `vike-hub-protocol`, `vike-model` and `vike-marketdata` and nothing else: no store, no
//! simulator, no harness, no DataFusion. The engine lives in `vike-backtest` and the daemon in
//! `vike-backtesthub`; the GUI and the CLI link this crate to ask, never to run.
//!
//! What both data-plane hubs share (the frame codec, the keyed handshake, the connection, which verb
//! belongs to which hub) is `vike-hub-protocol`'s, below both clients.

pub mod client;
// What a msgpack body carries that a JSON one does not: the whole reports as their tree, the long lists as
// columns ("JSON only at the edges", 2026-11-19, `vike_hub_protocol::features::FEATURE_WIRE_MSGPACK`).
pub mod msgpack_body;
pub mod named_run;
pub mod proto;
// The versioned strategy SDK's source as the server serves it to the Studio (decision 0296, verdict 3).
pub mod sdk_source;
// The research verbs' answers: what a study run on the server reports and the listings the Studio's
// Research pane reads, since studies run only on the server (the owner's ruling of 2026-11-19).
pub mod study;
pub mod summary;
// The negotiation suites' one fake peer, compiled once here instead of once per binary. Behind
// `test-support`, which only dev edges turn on.
#[cfg(feature = "test-support")]
pub mod test_support;
// `TradingRecord` <-> `wire_studio::TradingRecordWire`, both halves beside the DTO (it was
// `vike_backtest::wire_record` until 2026-11-10; the module doc says why it moved).
pub mod wire_record;
pub mod wire_studio;

pub use client::BacktesthubClient;
