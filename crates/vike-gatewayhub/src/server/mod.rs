//! The SERVER: the listener over `vike_hub_proto`, the handshake, the scopes, the caps, the per-session
//! queues and the back-pressure. It serves the wire `vike-gatewayhub-client` declares and calls the two
//! seams (`crate::data::DataLane`, `crate::rest::RestLane`) for everything that touches a venue: the
//! split is one function wide, and the server never sees a venue's socket while a lane never sees a
//! frame.
//!
//! The model is `crates/vike-datahub/src/server.rs` and its children (blocking, one named thread per
//! connection, decode separated from framing, a bad request never drops a connection), with the
//! differences this daemon's job forces:
//!
//! * **Always keyed.** `GATEWAYHUB_DOMAIN`, scopes `Read` and `Write`; no keys, no start; a coarse
//!   `AuthDenied` that never distinguishes a missing key from a wrong one (`handshake`).
//! * **Loopback only, with no flag** (`bind`): the handshake is plaintext in front of venue credentials.
//! * **Three socket classes**, decided by the first post-auth request (`connection`): `rpc` (positional,
//!   with the progress-framed `fetch` family and the idle read timeout), `stream` (duplex, one venue
//!   client per session, a bounded queue, heartbeat liveness) and `feed` (push-only).
//! * **A stop that reaches every connection** (`accept`, `ctl`): the daemon holds sessions, so its stop
//!   interrupts and joins them instead of ending with the process.
//!
//! # The back-pressure contract (drift register R3)
//!
//! A venue pump never blocks on a consumer and a loss is never silent. The mechanism - the wire sink, the
//! bounded queue with its reserved control lane and gap ledger, and the writer thread that owns the
//! socket and cuts a session off (`TooSlow`) that keeps losing frames - is the tube's
//! (`vike_market_stream::{sink, queue, writer}`, decision 0167); `stream` wires it to a session's socket.
//!
//! Every submodule is public so the daemon's tests can drive them directly; the entry point is
//! `accept::serve`.

pub mod accept;
pub mod bind;
pub mod connection;
pub mod ctl;
pub mod features;
pub mod feed;
pub mod fetch;
pub mod frame;
pub mod handshake;
pub mod lanes;
pub mod limits;
pub mod rpc;
pub mod shared;
pub mod stats;
pub mod stream;
