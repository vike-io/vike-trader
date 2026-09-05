//! vike-tradehub-client — the Layer-2 security primitive for the headless live-core ("tradehub")
//! thin-client architecture (PR-10 of the two-layer plan).
//!
//! This crate is built in ISOLATION, before anything listens: it defines the node wire contract and
//! the authentication handshake that will gate a future headless live core, with NO server and NO
//! network I/O of its own. What ships here:
//!
//! - [`proto`] — the node request/response schema (`Hello`/`Auth`/`Subscribe`/`Snapshot`/`Command`/
//!   `Preview`/`Ping`/`StrategyStatus` in, `Welcome`/`AuthOk`/`AuthDenied`/`SnapshotFrame`/`Ack`/
//!   `Preview`/`Error`/`Pong`/`StrategyStatus` out; `Preview` is the v3 server-authoritative
//!   dry-run, the strategy verbs are split-plane B4 — feature-negotiated via
//!   [`proto::FEATURE_STRATEGY_VERBS`], never a version bump), riding the
//!   SAME length-prefixed `serde_json` framing as the datahub service (re-exported from
//!   `vike_datahub_client::proto`, not re-implemented), plus [`proto::NODE_PROTO_VERSION`],
//!   [`proto::Scope`], and [`proto::Topic`].
//! - [`auth`] — the HMAC-SHA256 nonce-challenge handshake: [`auth::sign`]/[`auth::verify`] over a
//!   domain-separated message binding `(key, nonce, proto_version, scope)`, and [`auth::NodeKeys`],
//!   the scoped key store (redacting `Debug`, `.env`-loaded). Verification is constant-time via the
//!   `hmac` `Mac` trait — no `subtle`, no byte-wise `==`, zero new external crypto deps.
//! - [`liveness`] — the protocol's LINK-LIVENESS facts, in ONE place because both ends must agree
//!   on them: what an authenticated connection experiences when it is idle
//!   ([`liveness::AUTHED_IDLE_TIMEOUT`] — nothing, now that the node stops applying its
//!   unauthenticated handshake bound after `AuthOk`), how a quiet observe stream proves it is
//!   alive ([`liveness::OBSERVE_HEARTBEAT`], consumed by the SERVER in `vike-tradehub`) and how
//!   long a client waits before calling that silence a dead link
//!   ([`liveness::OBSERVE_READ_TIMEOUT`], armed only against a node advertising
//!   [`proto::FEATURE_OBSERVE_HEARTBEAT`]). The layer rule is what puts them here rather than in
//!   the daemon: this crate sits BELOW it, so the server consumes and never redefines.
//! - [`wire`] — standalone serde mirrors of the rendered core snapshot ([`wire::WireSnapshot`]) and
//!   the order-write command vocabulary ([`wire::WireCommand`]), kept independent of `vike-core`/
//!   `vike-exec`/`vike-model` so this stays the LIGHT, DataFusion-free thin-client wire crate.
//! - [`remote_handle`] (PR-11) — [`remote_handle::RemoteCoreHandle`], the read-only, push-fed thin
//!   client that connects to a headless node's observe server, authenticates under
//!   [`proto::Scope::Observe`], subscribes, and exposes the latest pushed [`wire::WireSnapshot`] off a
//!   local arc-swap cell. It returns `Arc<WireSnapshot>` (NOT `Arc<CoreSnapshot>`) precisely so this
//!   crate never depends on `vike-core` — the GUI-facing "looks like a `CoreHandle`" adapter is a
//!   DEFERRED `vike-app --observe` follow-up. The observe SERVER + publisher live in the daemon crate
//!   `vike-tradehub`; this is only the client half.
//! - [`remote_control`] (PR-12/13) — [`remote_control::RemoteControlHandle`], the WRITE-only twin of
//!   `RemoteCoreHandle`: it authenticates under [`proto::Scope::Control`], NEVER subscribes (control
//!   stays a request/response connection), and offers a non-blocking `try_command` over a background
//!   worker thread — the wire twin of `CoreHandle::try_command`. Its [`remote_control::ControlRejected`]
//!   is CRATE-LOCAL (not `vike_core::CommandRejected`), keeping the crate vike-core-free. std only.
//!   Enqueueing returns a [`remote_control::CommandTicket`] that
//!   [`remote_control::RemoteControlHandle::await_outcome`] resolves to THAT command's
//!   [`remote_control::CommandOutcome`] — the surface any caller reporting a result must use;
//!   [`remote_control::RemoteControlHandle::last_error`] is a separate LATCHED status-strip view.
//!
//! # The auth model in one line
//!
//! A client [`proto::Request::Hello`]s, the server replies with a fresh per-connection `nonce`, the
//! client returns an HMAC over that nonce (bound to its [`proto::Scope`] and the protocol version),
//! and the server constant-time-verifies it against the scope's key. A wrong key, wrong scope,
//! version skew, or replayed nonce all fail — so a captured transcript is useless against a later
//! connection, and an observe-only key can never forge an order-control ([`proto::Scope::Control`])
//! handshake.

pub mod auth;
pub mod liveness;
pub mod proto;
pub mod remote_control;
pub mod remote_handle;
pub mod wire;

// Crate-private: the ONE Hello → Welcome → Auth handshake sequence shared by the observe and
// control connection paths (`remote_handle` / `remote_control`) — only the `Scope` differs between
// callers. Not public: the handles are the surface, this is an implementation seam.
mod handshake;

// Ergonomic top-level re-exports — the handshake surface a caller reaches for first.
pub use auth::{NodeKeys, sign, verify};
pub use proto::{
    FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_VERBS, MAX_FRAME_LEN, NODE_PROTO_VERSION, Request, Response, Scope, Topic,
    read_frame, write_frame,
};
pub use remote_control::{
    CommandOutcome, CommandTicket, ControlRejected, RemoteControlHandle, preview_command,
    set_setting,
};
pub use remote_handle::{RemoteCoreHandle, settings_show, strategy_status};
pub use wire::{
    WireCommand, WireMountRow, WireOrderRequest, WireSettingsRow, WireSettingsShow, WireSnapshot,
    WireStrategyStatus, WireTradingState,
};
