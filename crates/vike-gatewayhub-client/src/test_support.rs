//! The FAKE PEERS: scripted gatewayhubs on loopback that speak this crate's protocol and record every
//! frame a client sends, so a test reads what was sent off the wire and not off what the client says.
//!
//! Compiled ONCE here, under the `test-support` feature, rather than once per test binary: this crate's
//! own suites turn the feature on through their self-referential dev edge, and another crate's tests
//! (the tradehub's wire suites) through theirs. A build that carries it carries a peer that answers any
//! request with whatever the test scripted, so only a `[dev-dependencies]` edge may turn it on
//! (`crates/vike-ops/tests/architecture/test_surface_gate.rs` refuses a shipped one).
//!
//! * [`fake_peer`] — the data class (`stream`, `rpc`, `feed`), compiled with the wire.
//! * `fake_trade_peer` (feature `trade`) — the trade lease, its `cseq` watermark, the epoch and the ring.
//! * [`props`] — the generator the two event round-trip property suites share.

#[cfg(any(feature = "data", feature = "trade"))]
pub mod fake_peer;
#[cfg(feature = "trade")]
pub mod fake_trade_peer;
pub mod props;
