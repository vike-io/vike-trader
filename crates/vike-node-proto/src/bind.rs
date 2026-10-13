//! The bind ADDRESS classification every node server shares: is this listen address reachable
//! off-box?
//!
//! It lives here, below both protocols, because the datahub's servers
//! (`crates/vike-datahub-client/src/bind.rs`'s `bind_decision`, which `vike-datahub` and
//! `vike-backtest`'s backtesthub both call) and the tradehub node
//! (`crates/vike-tradehub/src/server.rs`'s `bind_decision`) must classify an address identically:
//! a server that read the wildcard as loopback would quietly listen on every interface. Each server
//! keeps its own DECISION over this classification — the two postures differ (only the datahub
//! can be key-less, so only its decision weighs `ServerAuth`), and the decision is where that
//! difference is argued.
//!
//! Nothing here does I/O: the caller performs the `to_socket_addrs()` — the same resolution
//! `TcpListener::bind` performs — and hands the resolved addresses in, so the classification is a
//! pure function a unit test drives without a socket and without DNS.

use std::net::SocketAddr;

/// How exposed a resolved bind target is — the input to a server's non-loopback refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindExposure {
    /// Every resolved address is a loopback address: reachable only from this host.
    Loopback,
    /// At least one resolved address is NOT loopback (a specific interface, or the `0.0.0.0` /
    /// `::` wildcard). Carries the first such address, for the operator-facing message.
    Public(SocketAddr),
    /// The address resolved to nothing at all — `bind` would fail anyway; classified separately so
    /// a caller never has to read "not loopback" as "publicly exposed".
    Unresolvable,
}

/// Classify an already-RESOLVED bind target.
///
/// A target is [`BindExposure::Loopback`] only when EVERY resolved address is loopback — a hostname
/// resolving to both `127.0.0.1` and a LAN address is exposed on the LAN, so the strictest answer
/// is the true one. `0.0.0.0` and `::` are the wildcard binds, which `IpAddr::is_loopback` reports
/// false for, so they classify as [`BindExposure::Public`] — correctly: they listen on every
/// interface, which is the single most common way this surface gets accidentally exposed.
pub fn bind_exposure(resolved: &[SocketAddr]) -> BindExposure {
    match resolved.iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => BindExposure::Public(*a),
        None if resolved.is_empty() => BindExposure::Unresolvable,
        None => BindExposure::Loopback,
    }
}

#[path = "bind_tests.rs"]
#[cfg(test)]
mod bind_tests;
