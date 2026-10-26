//! vike-market-stream — the one live-data TUBE.
//!
//! # What this crate is
//!
//! A `vike_data::LiveDataSink` call on one side of a socket becomes the same call on the other
//! (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`). It holds the
//! three halves of that hop, so every consumer of it is ONE implementation:
//!
//! * `wire` — the frames: `StreamUp` from the subscriber, `StreamDown` from the sender, one per sink
//!   verb, keyed by a per-session `k`; `GapVerb`, `WireStream`, `WireLane` and the per-verb DTOs.
//! * the SENDING half — `queue` (the per-session bounded queue and the overflow policy that makes a
//!   laggard's losses honest), `sink` (`WireSink`, the `LiveDataSink` a venue client calls and the
//!   writer-side API) and `writer` (the thread that owns the socket). A venue pump never blocks on a
//!   consumer and a loss is never silent.
//! * the RECEIVING half — `market` (`RemoteDataClient`, a `DataClient` over one `stream` session with
//!   reconnect, replay and gap disclosure), generalised over an `opener::Opener` the client crate
//!   supplies, and `intern` (the leak-once map from a sender's `Unsupported` text to a `&'static str`).
//! * `frame` and `opener` — the frame discipline both halves share (a frame read and written under a
//!   ceiling) and the seam to the hosting protocol: the dial, the handshake and the open reply are the
//!   client crate's, never this one's.
//!
//! # What it must NEVER link
//!
//! * **No venue bridge and no `vike-bridge-core`**, and no `tungstenite`, `rustls` or `ureq`: the tube
//!   speaks `std::net` and `serde_json` frames and opens no venue socket. The rule is held by NAME in
//!   `crates/vike-ops/tests/architecture/named_run_closure_gate/fences.rs`, whatever the layer ranks say.
//! * **No hub protocol.** The handshake, the scopes, the keys and the address belong to the client crate
//!   whose protocol hosts the `stream` socket; this crate sees them only as an `Opener`.
//! * **Nothing the live core or a user-code host reaches.** The core meets a venue only through the
//!   `vike-exec` lanes, and user code never reaches a socket.
//!
//! Live data only: history fetches, rpc lookups and the order path stay outside (decision 0167,
//! verdict 4).

pub mod frame;
pub mod intern;
pub mod market;
pub mod opener;
pub mod queue;
pub mod sink;
pub mod wire;
pub mod writer;
