//! The BIND POSTURE both localhost daemons share — is this listen address reachable off-box, and
//! may this process open it?
//!
//! ⚠ **It lives in this LIGHT crate, below both servers, because two daemons must answer it
//! identically.** Ruling 7 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split one served surface
//! into `vike-backend datahub` (the DATA plane, crate `vike-datahub`) and
//! `vike-backend backtest --addr` (the COMPUTE plane, crate `vike-backtest`), speaking one protocol
//! over one handshake, and `vike-backtest` cannot name `vike-datahub` (layers are down-only). A
//! copy of this policy in each would be the shape the workspace's rule refuses (*when two sides
//! must not disagree, the cure is a shared crate BELOW both*), and the first divergence would be a
//! daemon quietly binding `0.0.0.0` unauthenticated.
//!
//! `vike_tradehub::server` keeps its own twin, deliberately: that node server cannot be key-less
//! (its `serve` takes `keys: NodeKeys` by value and `start_observe_server` opens no listener
//! without an observe key), so the [`ServerAuth`] half of this policy has no case to compute over
//! there. The ADDRESS half ([`bind_exposure`], [`BindExposure`]) stays mirrored name-for-name.
//!
//! Nothing here does I/O: the caller performs the `to_socket_addrs()` — the same resolution
//! `TcpListener::bind` performs — and hands the resolved addresses in, so the whole policy is a
//! pure function a unit test drives without a socket and without DNS.

use std::net::SocketAddr;

use vike_node_proto::auth::NodeKeys;
/// How exposed a resolved bind target is — the input to the bin's non-loopback refusal. Mirrors
/// `vike_tradehub::server`'s `BindExposure` (same variants, same classification rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindExposure {
    /// Every resolved address is a loopback address: reachable only from this host.
    Loopback,
    /// At least one resolved address is NOT loopback (a specific interface, or the wildcard
    /// binds). Carries the first such address, for the operator-facing message.
    Public(SocketAddr),
    /// The address resolved to nothing at all — `bind` would fail anyway; classified separately so
    /// a caller never has to read "not loopback" as "publicly exposed".
    Unresolvable,
}

/// Classify an already-RESOLVED bind target.
///
/// [`BindExposure::Loopback`] only when EVERY resolved address is loopback — a hostname resolving
/// to both a loopback and a LAN address is exposed on the LAN. `0.0.0.0` and `::` (the wildcard
/// binds, for which `IpAddr::is_loopback` is false) classify as [`BindExposure::Public`]: they
/// listen on every interface, the most common way this surface gets accidentally exposed.
pub fn bind_exposure(resolved: &[SocketAddr]) -> BindExposure {
    match resolved.iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => BindExposure::Public(*a),
        None if resolved.is_empty() => BindExposure::Unresolvable,
        None => BindExposure::Loopback,
    }
}

/// Whether the server about to open this listener CAN authenticate a connection — the third input
/// to [`bind_decision`], and what makes it a POSTURE decision rather than an address one.
///
/// A named enum rather than a second `bool`: [`bind_decision`] already takes `allow_public`, and
/// two adjacent `bool`s TRANSPOSE SILENTLY at a call site, which here would restore exactly the
/// defect [`BindDecision::RefuseUnauthenticated`] removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAuth {
    /// `vike_datahub::server`'s `serve_authed` (or its compute twin,
    /// `vike_backtest::compute_server`'s) is about to be handed `Some(keys)`: every connection must
    /// complete the handshake before any verb is answered, and each verb is then checked against
    /// [`crate::proto::required_scope`].
    Keyed,
    /// `serve_authed` is about to be handed `None`: no handshake is required, and every verb but
    /// one — the history reads, the `Run*` verbs that COMPILE CLIENT-SUPPLIED RHAI, and (in a
    /// `backfill-serve` build) the `Backfill` store WRITE — is served to whoever can reach the
    /// socket. The bind posture is then the entire barrier, which is why it may not be waived.
    ///
    /// The exception is [`crate::proto::Request::DeleteSeries`]:
    /// `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` answers
    /// `KEYLESS_DELETE_REFUSAL` on this arm, and `served_features` withholds the advertisement.
    /// `Backfill` stays served on purpose: both are [`vike_node_proto::auth::Scope::Write`] on a
    /// keyed server, but a backfill writes rows a re-fetch restores and a removal takes the only
    /// copy (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`).
    Keyless,
}

impl ServerAuth {
    /// Classify from the very value the caller is about to hand `serve_authed`, so the bind guard
    /// and the server cannot disagree about whether this process authenticates.
    ///
    /// `Some(_)` is [`ServerAuth::Keyed`] even when only ONE scope has a key: `run_handshake`
    /// refuses a scope this server holds no key for (`if !keys.has(scope)`) BEFORE the key is
    /// consulted. ⚠ Cite `has`, NOT `NodeKeys::key_for`: `vike_node_proto::auth`'s `verify` builds
    /// `HmacSha256::new_from_slice(key)`, which accepts a zero-length key, so an empty key is an
    /// ordinary HMAC, not a closed gate.
    ///
    /// ⚠ The key-less shape is a strict SUBSET, not a superset: `handle_request`'s `keyed = false`
    /// call refuses `Request::DeleteSeries` (`KEYLESS_DELETE_REFUSAL`), while a Control peer on a
    /// keyed server (`handle_request(.., true)`) is served every non-handshake verb.
    pub fn of(keys: Option<&NodeKeys>) -> Self {
        match keys {
            Some(_) => ServerAuth::Keyed,
            None => ServerAuth::Keyless,
        }
    }
}

/// What the bin should DO about a bind target — the policy, kept in the library so it is
/// unit-testable and a change to it is a change to a named function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// Loopback (or unresolvable — `bind` will report that itself): bind, say nothing special.
    Proceed,
    /// Non-loopback, WITH the operator's explicit opt-in, on a server that CAN authenticate: bind,
    /// but announce what is now reachable. ⚠ Reachable only under [`ServerAuth::Keyed`] — the
    /// key-less half of this arm is [`BindDecision::RefuseUnauthenticated`] below.
    ProceedExposed(SocketAddr),
    /// Non-loopback with NO opt-in: do not bind. What "do not bind" means is the caller's — the
    /// tradehub daemon keeps trading headless, while this server's bin EXITS, because serving is
    /// its only job.
    Refuse(SocketAddr),
    /// Non-loopback WITH the opt-in, on a [`ServerAuth::Keyless`] server: do not bind either. A
    /// SEPARATE variant rather than a reason on [`BindDecision::Refuse`] because the two refusals
    /// have different FIXES — "keep the address on loopback" versus "configure the node keys".
    RefuseUnauthenticated(SocketAddr),
}

/// Decide whether to open the listener. `allow_public` is the operator's explicit opt-in — for
/// this server `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, environment-only (its tradehub twin also has a
/// `flags.tradehub_allow_public_bind` key). `auth` is the value about to be handed
/// `serve_authed`, classified by [`ServerAuth::of`].
///
/// ⚠ The refusal is a DEFAULT, not a prohibition. The handshake authenticates the CONNECTION over
/// plaintext, not each frame, so under either posture the tunnel supplies confidentiality and
/// integrity (0025's "honest limits of B"): the design answer is a loopback listener plus an SSH
/// tunnel, which the shipped unit cannot enforce (its `EnvironmentFile=` beats its own bind
/// default). A trusted LAN or VPN is a real deployment, so the escape hatch is NAMED and separate:
/// typing the address is the mistake, so the address cannot be its own consent.
///
/// ⚠ **The escape hatch has a floor, and `auth` puts it there.** `allow_public` consents to being
/// REACHABLE, the node keys make what is reached AUTHENTICATED, and a non-loopback bind needs both
/// ([`BindDecision::RefuseUnauthenticated`]): an opt-in warning in front of an unauthenticated
/// write verb is "the exact state this record exists to prevent"
/// (`docs/decisions/0025-datahub-remote-posture.md`). LOOPBACK ignores `auth` either way (a
/// key-less loopback datahub is the ordinary developer configuration), and an UNRESOLVABLE target
/// proceeds, left to `TcpListener::bind`'s better message.
///
/// ⚠ The tradehub twin takes no `auth`: that server cannot be key-less (the module doc).
pub fn bind_decision(
    resolved: &[SocketAddr],
    allow_public: bool,
    auth: ServerAuth,
) -> BindDecision {
    match bind_exposure(resolved) {
        BindExposure::Loopback | BindExposure::Unresolvable => BindDecision::Proceed,
        // No opt-in at all: the first thing to say is still "keep it on loopback", whether or not
        // keys happen to be configured.
        BindExposure::Public(a) if !allow_public => BindDecision::Refuse(a),
        BindExposure::Public(a) => match auth {
            ServerAuth::Keyed => BindDecision::ProceedExposed(a),
            ServerAuth::Keyless => BindDecision::RefuseUnauthenticated(a),
        },
    }
}
/// The bind posture's tests. The ADDRESS half mirrors
/// `crates/vike-tradehub/src/server/tests/exposure.rs` name-for-name (minus its handshake-cap and
/// connection-slot tests, which police limits this module does not own); the AUTH half has no
/// tradehub twin (the module doc says why).
#[path = "bind_tests.rs"]
#[cfg(test)]
mod bind_tests;
