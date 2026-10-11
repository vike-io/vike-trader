//! vike-tradehub-client — the thin-client side of the headless live core ("tradehub"): the node
//! wire contract, the authentication handshake that gates it, and the handles and one-shot calls
//! that dial a node.
//!
//! It has NO server (the listener, observe publisher and control server live in the daemon crate
//! `vike-tradehub`); its only network I/O is the `std::net` TCP stream that
//! `crates/vike-tradehub-client/src/handshake.rs`'s `node_handshake` opens to a node. The property
//! to keep: no server, no async runtime and no bridge transport stack (ureq/tungstenite/rustls), so
//! a thin CLI or GUI links this crate without any of them.
//!
//! Modules (each top-level path IS the public API; consumers name `proto::X`, `wire::X`, …):
//!
//! - [`proto`] — the request/response schema, [`proto::NODE_PROTO_VERSION`], the `FEATURE_*`
//!   capability strings (an additive verb is a feature string, never a version bump),
//!   [`proto::Scope`] and [`proto::Topic`], over the frame codec of `vike_node_proto::frame`.
//! - [`auth`] — this service's binding of the shared HMAC-SHA256 nonce-challenge:
//!   [`auth::sign`]/[`auth::verify`] under the tradehub domain, the key names, [`auth::NodeKeys`].
//! - [`liveness`] — the link-liveness constants both ends must agree on; the server in
//!   `vike-tradehub` consumes them and never redefines them.
//! - [`wire`] — serde mirrors of the rendered core snapshot ([`wire::WireSnapshot`]) and the
//!   order-write vocabulary ([`wire::WireCommand`]), kept
//!   independent of `vike-core`/`vike-exec`/`vike-model` so this stays the LIGHT,
//!   DataFusion-free thin-client wire crate.
//! - [`remote_handle`] — [`remote_handle::RemoteCoreHandle`], the read-only push-fed observer
//!   ([`proto::Scope::Read`]), plus the one-shot reads. It returns `Arc<WireSnapshot>`
//!   (NOT `Arc<CoreSnapshot>`) precisely so this crate never depends on `vike-core` — nor on
//!   `vike-exec`, where `CoreSnapshot` lives. The adapter back to a `CoreSnapshot` is
//!   `crates/vike-app-core/src/backend/observe_bridge.rs`'s `wire_to_core`.
//! - [`remote_control`] — [`remote_control::RemoteControlHandle`], the write-only twin
//!   ([`proto::Scope::Write`], never subscribes): a non-blocking `try_command` over a worker thread,
//!   a crate-local [`remote_control::ControlRejected`], and a [`remote_control::CommandTicket`] that
//!   [`remote_control::RemoteControlHandle::await_outcome`] resolves to THAT command's
//!   [`remote_control::CommandOutcome`] (the surface for reporting a result;
//!   [`remote_control::RemoteControlHandle::last_error`] is a separate latched status-strip view).
//!
//! # The auth model in one line
//!
//! [`proto::Request::Hello`] → a fresh per-connection `nonce` → an HMAC over it bound to the
//! [`proto::Scope`] and protocol version → constant-time verify against that scope's key. A wrong
//! key, wrong scope, version skew or replayed nonce fails, so a captured transcript is useless and an
//! observe-only key can never forge a [`proto::Scope::Write`] handshake.

pub mod auth;
pub mod liveness;
pub mod proto;
pub mod remote_control;
pub mod remote_handle;
pub mod wire;

// Crate-private: the ONE Hello → Welcome → Auth sequence (plus the one-shot verbs' refusal and
// reply helpers) shared by `remote_handle` and `remote_control`.
mod handshake;

// The keys, handles and verbs a caller reaches for first. The wire vocabulary (`proto::Request`,
// `wire::WireCommand`, …) is named at its module only: one spelling, so callers cannot split.
pub use auth::{NodeKeys, sign, verify};
pub use proto::{
    FEATURE_ACCOUNT_VERBS, FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW,
    FEATURE_SETTINGS_WRITE, FEATURE_TEARSHEET, MAX_FRAME_LEN, Topic,
};
pub use remote_control::{
    CommandOutcome, CommandTicket, ControlRejected, RemoteControlHandle, preview_command,
    set_setting,
};
pub use remote_handle::{
    RemoteCoreHandle, directory, settings_show, snapshot_once, strategy_params, strategy_status,
    strategy_status_with_features, tearsheet,
};
pub use wire::{
    AccountRequest, AccountVerb, WireAccountList, WireAccountRow, WireAccountWritten, WireMountRow,
    WireSettingsRow, WireSettingsShow, WireStrategyStatus,
};
