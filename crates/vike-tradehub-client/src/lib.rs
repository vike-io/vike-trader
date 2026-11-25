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
//!   [`auth::sign`]/[`auth::verify`] under the tradehub domain (no key names: a person signs with its
//!   own device key, decision 0288).
//! - [`liveness`] — the link-liveness constants both ends must agree on; the server in
//!   `vike-tradehub` consumes them and never redefines them.
//! - [`wire`] — serde mirrors of the rendered core snapshot ([`wire::CoreSnapshotWire`]) and the
//!   order-write vocabulary ([`wire::CommandWire`]), kept
//!   independent of `vike-core` so this stays the LIGHT,
//!   DataFusion-free thin-client wire crate. A wire type that IS its `vike-model` domain type is
//!   that type itself, not a mirror (the owner's rulings 14, E8 and 15, 2026-11-11; decision 0091's
//!   last section): `vike-model` is serde vocabulary, so the crate stays as light.
//! - [`tradehub_handle`] — [`tradehub_handle::TradehubCoreHandle`], the read-only push-fed observer
//!   ([`vike_hub_protocol::auth::Scope::Read`]), plus the one-shot reads. It returns `Arc<CoreSnapshotWire>`
//!   (NOT `Arc<CoreSnapshot>`) precisely so this crate never depends on `vike-core`. The adapter back to a
//!   `CoreSnapshot` (`vike-execution`'s read model) is [`observe_decode::wire_to_core`], which the caller
//!   runs on its own thread.
//! - [`observe_decode`] — the observe wire's REVERSE codec, [`observe_decode::wire_to_core`] and the `map_*`
//!   family, and [`node_words`], what a frame says that `CoreSnapshot` has no field for. [`command_lift`] —
//!   [`command_lift::wire_from_command`], a live-core `Command` lifted to its [`wire::CommandWire`]. All three
//!   moved here from `vike-app-core` (D56, the owner's 2026-11-17 ruling; decision 0091's section of
//!   2026-11-18), so the desktop, `vike-mcp` and `vike-telegram` share ONE decoder of the live stream. They
//!   name `vike-execution`'s read model and command vocabulary, taken with `default-features = false`: no
//!   async runtime, no `vike-core`.
//! - [`observe_stream`] — what the observe stream carries beyond a plain frame, for a subscriber that asked for it:
//!   a compressed body, and the patched frames [`observe_stream::ObserveReceiver`] rebuilds the snapshot from
//!   ([`observe_patch`] is the patch). The node's half is `crates/vike-tradehub/src/server/observer_push.rs`.
//! - [`fold_guards`] — what a decoded snapshot says a fold of stored fills cannot read: its off-dollar ledgers
//!   and off-default contracts (decisions 0209 and 0243), for the Account window's Summary and `vike-cli report`.
//! - [`tradehub_control`] — [`tradehub_control::TradehubControlHandle`], the write-only twin
//!   ([`vike_hub_protocol::auth::Scope::Write`], never subscribes): a non-blocking `try_command` over a worker thread,
//!   a crate-local [`tradehub_control::ControlRejected`], and a [`tradehub_control::CommandTicket`] that
//!   [`tradehub_control::TradehubControlHandle::await_outcome`] resolves to THAT command's
//!   [`tradehub_control::CommandOutcome`] (the surface for reporting a result;
//!   [`tradehub_control::TradehubControlHandle::last_error`] is a separate latched status-strip view).
//! - [`order_checks`] — the client-side order checks before a node sees an order: [`order_checks::OrderLimits`]
//!   (this machine's caps) and [`order_checks::validate_with_value`], refused with an
//!   [`order_checks::OrderLimitsError`]. PURE over `vike-model` types.
//! - [`venue_routing`] — the client half of the node's routing gate: [`venue_routing::may_send_to_backend`]
//!   refuses a venue a node without [`proto::FEATURE_VENUE_ROUTING`] would apply to its primary engine. PURE.
//!
//! # The auth model in one line
//!
//! The ONE hub handshake (`vike_hub_protocol::handshake`, decision 0288): `Hello` → a fresh per-connection
//! `nonce` → an HMAC over it bound to the [`vike_hub_protocol::auth::Scope`] and protocol version →
//! constant-time verify against that scope's key, or a device's Ed25519 signature over the same bytes checked
//! against its access row. A wrong key, wrong scope, version skew or replayed nonce fails, so a captured
//! transcript is useless and an observe-only key can never forge a [`vike_hub_protocol::auth::Scope::Write`]
//! handshake.

pub mod auth;
pub mod command_lift;
pub mod fold_guards;
pub mod liveness;
pub mod node_words;
pub mod observe_decode;
pub mod observe_patch;
pub mod observe_stream;
pub mod order_checks;
pub mod proto;
pub mod refusals;
/// The negotiation suites' scripted node (`test-support` only: a default build, which opens no
/// listener, never compiles it).
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod tradehub_control;
pub mod tradehub_handle;
pub mod venue_routing;
pub mod wire;

// Crate-private: this client's side of the ONE hub handshake (plus the one-shot verbs' refusal and
// reply helpers) shared by `tradehub_handle` and `tradehub_control`. Its one public verb, the invitation's
// registration, is reached at the crate root below.
mod handshake;

// An invitation's registration of this device (decision 0288), the one verb a guest's app sends before it
// holds an access row.
pub use handshake::register_device;

// The handles and verbs a caller reaches for first. The wire vocabulary (`proto::Request`,
// `proto::FEATURE_*`, `wire::CommandWire`, …) and `auth::sign`/`auth::verify` are named at their
// module only: one spelling, so callers cannot split.
pub use tradehub_control::{
    CommandOutcome, CommandRefusal, ControlRejected, TradehubControlHandle, UnansweredCommand,
    preview_command, set_setting,
};
pub use tradehub_handle::{
    FrameAge, TradehubCoreHandle, directory, exchange_once, settings_show, snapshot_once,
    strategy_status, strategy_status_with_features, venue_read,
};
