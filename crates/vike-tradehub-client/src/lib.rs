//! vike-tradehub-client — the thin-client side of the headless live core ("tradehub"): the node
//! wire contract, the authentication handshake that gates it, and the handles and one-shot calls
//! that dial a node.
//!
//! It has NO server (the listener, observe publisher and control server live in the daemon crate
//! `vike-tradehub`); its only network I/O is the `std::net` TCP stream that
//! `crates/vike-tradehub-client/src/handshake.rs`'s `hub_handshake` opens to a node. The property
//! to keep: no server, no async runtime and no bridge transport stack (ureq/tungstenite/rustls), so
//! a thin CLI or GUI links this crate without any of them.
//!
//! Modules (each top-level path IS the public API; consumers name `proto::X`, `wire::X`, …):
//!
//! - [`proto`] — the request/response schema, [`proto::NODE_PROTO_VERSION`], the `FEATURE_*`
//!   capability strings (an additive verb is a feature string, never a version bump),
//!   [`vike_hub_protocol::auth::Scope`] and [`proto::Topic`], over the frame codec of `vike_hub_protocol::frame`.
//! - [`auth`] — this service's binding of the shared HMAC-SHA256 nonce-challenge:
//!   [`auth::sign`]/[`auth::verify`] under the tradehub domain, the key names, [`vike_hub_protocol::auth::HubKeys`].
//! - [`liveness`] — the link-liveness constants both ends must agree on; the server in
//!   `vike-tradehub` consumes them and never redefines them.
//! - [`wire`] — serde mirrors of the rendered core snapshot ([`wire::CoreSnapshotWire`]) and the
//!   order-write vocabulary ([`wire::CommandWire`]), kept
//!   independent of `vike-core`/`vike-execution` so this stays the LIGHT,
//!   DataFusion-free thin-client wire crate. A wire type that IS its `vike-model` domain type is
//!   that type itself, not a mirror (the owner's rulings 14, E8 and 15, 2026-11-11; decision 0091's
//!   last section): `vike-model` is serde vocabulary, so the crate stays as light.
//! - [`tradehub_handle`] — [`tradehub_handle::TradehubCoreHandle`], the read-only push-fed observer
//!   ([`vike_hub_protocol::auth::Scope::Read`]), plus the one-shot reads. It returns `Arc<CoreSnapshotWire>`
//!   (NOT `Arc<CoreSnapshot>`) precisely so this crate never depends on `vike-core` — nor on
//!   `vike-execution`, where `CoreSnapshot` lives. The adapter back to a `CoreSnapshot` is
//!   `crates/vike-app-core/src/backend/observe_bridge.rs`'s `wire_to_core`.
//! - [`tradehub_control`] — [`tradehub_control::TradehubControlHandle`], the write-only twin
//!   ([`vike_hub_protocol::auth::Scope::Write`], never subscribes): a non-blocking `try_command` over a worker thread,
//!   a crate-local [`tradehub_control::ControlRejected`], and a [`tradehub_control::CommandTicket`] that
//!   [`tradehub_control::TradehubControlHandle::await_outcome`] resolves to THAT command's
//!   [`tradehub_control::CommandOutcome`] (the surface for reporting a result;
//!   [`tradehub_control::TradehubControlHandle::last_error`] is a separate latched status-strip view).
//!
//! # The auth model in one line
//!
//! [`proto::Request::Hello`] → a fresh per-connection `nonce` → an HMAC over it bound to the
//! [`vike_hub_protocol::auth::Scope`] and protocol version → constant-time verify against that scope's key. A wrong
//! key, wrong scope, version skew or replayed nonce fails, so a captured transcript is useless and an
//! observe-only key can never forge a [`vike_hub_protocol::auth::Scope::Write`] handshake.

pub mod auth;
pub mod liveness;
pub mod proto;
/// The negotiation suites' scripted node (`test-support` only: a default build, which opens no
/// listener, never compiles it).
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod tradehub_control;
pub mod tradehub_handle;
pub mod wire;

// Crate-private: the ONE Hello → Welcome → Auth sequence (plus the one-shot verbs' refusal and
// reply helpers) shared by `tradehub_handle` and `tradehub_control`.
mod handshake;

// The handles and verbs a caller reaches for first. The wire vocabulary (`proto::Request`,
// `proto::FEATURE_*`, `wire::CommandWire`, …) and `auth::sign`/`auth::verify` are named at their
// module only: one spelling, so callers cannot split.
pub use tradehub_control::{
    CommandOutcome, ControlRejected, TradehubControlHandle, preview_command, set_setting,
    set_setting_outcome_unknown,
};
pub use tradehub_handle::{
    FrameAge, TradehubCoreHandle, directory, exchange_once, settings_show, snapshot_once,
    strategy_status, strategy_status_with_features, tearsheet, venue_read,
};
