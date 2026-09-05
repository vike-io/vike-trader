//! vike-datahub-client — the LIGHT client half of the vike-datahub data-service (Phase 2 of the
//! "thin-client GUI" architecture).
//!
//! # What this crate is
//!
//! The DataFusion-free half of vike-datahub: the wire [`proto`]col, the blocking [`DatahubClient`],
//! and [`RemoteHistStore`] — a [`vike_data::HistStore`] whose GUI-relevant READ verbs are answered
//! by a remote `vike-datahub` server over RPC. It was split out of `vike-datahub` (Phase 1) so the
//! GUI can talk to the data-service WITHOUT linking the Arrow/DataFusion engine.
//!
//! # Why it stays light (the whole point)
//!
//! This crate depends on `vike-data` with **DEFAULT features** — the `HistStore` TRAIT only, NOT
//! `hist-datafusion` — plus `vike-model` + serde. It names NO concrete backend, no `vike-backtest`,
//! no DataFusion/Arrow/Parquet. `RemoteHistStore` implements the trait purely by RPC, so a consumer
//! that links this crate (the GUI, in Phase 3) inherits none of that weight. The heavy engine lives
//! only in `vike-datahub` (the server) and behind its `serve-datafusion` feature.
//!
//! # Phased plan
//!
//! - Phase 1 (`vike-datahub`): the compute-to-data backtest SERVER over localhost TCP.
//! - Phase 2 (this crate): split the proto + client out, and add [`RemoteHistStore`] so an EXISTING
//!   local `HistStore` consumer can point at remote data.
//! - Phase 3 (future): the GUI thin-client drops its DataFusion link and reads through this seam.
//!
//! # The shared node-auth primitive ([`node_auth`])
//!
//! [`node_auth`] is the HMAC-SHA256 nonce-challenge handshake BOTH localhost services sign with —
//! this one and the `vike-tradehub` node — generalized over its domain separator. It lives here for
//! the same reason the FRAMING does: `vike-tradehub-client` (layer 50) already re-exports this
//! crate's (layer 30) frame codec rather than growing a second one, and auth is the same class of
//! primitive. See that module's own doc for the argument, and
//! `docs/decisions/0025-datahub-remote-posture.md` for the verdict that put it there. It adds no
//! transport and no I/O: `hmac`/`sha2` are the crates `vike-bridge-core`'s venue signer already
//! links, and the keys arrive as a caller-supplied map.

pub mod client;
pub mod node_auth;
pub mod proto;
pub mod remote;
pub mod wire_studio;

pub use client::DatahubClient;
pub use node_auth::{NodeKeys, Scope};
pub use proto::{
    BackfillDone, FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_COVERAGE, MAX_FRAME_LEN, PROTO_VERSION,
    Request, Response, read_frame, read_frame_raw, read_frame_raw_capped, write_frame,
};
pub use remote::RemoteHistStore;
pub use wire_studio::{
    WireEngineParams, WireRunError, WireRunResult, WireSlice, WireSliceKind, WireSpec, WireSweep,
    WireSweepEntry, WireSweepResult, WireTrade, WireWalkforward, WireWalkforwardResult,
    WireWfWindow,
};
