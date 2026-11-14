//! The pre-auth phase: `Hello` -> `Welcome{features, nonce}` -> `Auth{scope, mac}` -> `AuthOk` or the
//! coarse `AuthDenied`. ALWAYS keyed: there is no key-less mode (decision 0164, verdict 6), so
//! `crate::server::accept::serve` refuses to start without a key and this module never asks "is auth
//! on".
//!
//! The steps are `vike_hub_protocol::handshake::run_keyed_handshake`'s, the one spelling the datahub and
//! the backtesthub share, under the gatewayhub's terms: its domain (`GATEWAYHUB_DOMAIN`, signed
//! together with `GATEWAYHUB_PROTO_VERSION` and the scope), no data plane (a frame this daemon cannot
//! decode is undecodable, whatever its tag), both pre-auth frames read under the 64 KiB pre-auth cap
//! and every pre-auth reply written under the caller's `ceiling`, and the WHOLE phase under ONE
//! deadline (`run_handshake`'s `deadline`, turned into an `Instant` when the phase starts; that
//! module's doc says why a per-read timeout is not a deadline). The log lines are this daemon's own.
//!
//! # Refusals are coarse
//!
//! A scope this server holds NO key for, a scope it does not serve at all and a wrong mac all answer
//! the same `bad mac`: distinguishing them tells an unauthenticated peer which scopes the daemon
//! offers. The operator's log line carries the real answer. This daemon serves `Scope::Read` and
//! `Scope::Write`; `Scope::Account` admits nothing here even when the key set carries an
//! admin key, and `Scope::Trade` (signing with the Trade key, a row of the `hub_key` table read by name; no
//! environment variable carries it) is served exactly when the
//! trade lane is (the `Welcome` advertises `gateway_trade`) and is otherwise refused as a scope this server
//! does not serve: [`scope_is_served`] is the one list.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use vike_gatewayhub_client::proto::{
    FEATURE_GATEWAY_TRADE, GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, request_kind,
};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::handshake::{
    HandshakeNote, HandshakeOutcome, HandshakeSpec, run_keyed_handshake,
};

/// The scopes this daemon serves: the data surface's `Read` and `Write` always, and `Trade` exactly when the
/// trade lane is served (`trade_served`: the daemon advertises `gateway_trade`, which it does only with a lane and
/// a Trade key). `Account` is refused (the same coarse `bad mac` as a scope with no key).
pub fn scope_is_served(scope: Scope, trade_served: bool) -> bool {
    match scope {
        Scope::Read | Scope::Write => true,
        Scope::Trade => trade_served,
        _ => false,
    }
}

/// Run `Hello` -> `Welcome` -> `Auth` on `stream`, advertising `features`.
///
/// Every pre-auth write goes out under `ceiling` (the client reads a handshake frame under its own
/// small cap, and nothing here is large). The whole phase, both reads, shares ONE `deadline`.
pub fn run_handshake(
    stream: &mut TcpStream,
    keys: &HubKeys,
    features: &[String],
    peer: Option<SocketAddr>,
    ceiling: u32,
    deadline: Duration,
) -> HandshakeOutcome {
    let spec = HandshakeSpec {
        domain: GATEWAYHUB_DOMAIN,
        proto_version: GATEWAYHUB_PROTO_VERSION,
        plane: None,
        features,
        deadline,
        reply_ceiling: ceiling,
    };
    let trade_served = features.iter().any(|f| f == FEATURE_GATEWAY_TRADE);
    let outcome = run_keyed_handshake(
        stream,
        keys,
        spec,
        |scope| scope_is_served(scope, trade_served),
        request_kind,
        |note| match note {
            HandshakeNote::VerbRefused(verb) => tracing::info!(
                ?peer,
                verb,
                "vike-gatewayhub: verb refused - connection is not authenticated"
            ),
            HandshakeNote::VersionDiffers(client_version) => tracing::debug!(
                ?peer,
                client_version,
                server_version = GATEWAYHUB_PROTO_VERSION,
                "vike-gatewayhub: client protocol version differs; auth will fail on the signed version"
            ),
            HandshakeNote::NoKey(scope) => tracing::info!(
                ?peer,
                ?scope,
                "vike-gatewayhub: auth refused - this scope is not served, or no key is configured for it"
            ),
            // Logged below, once the outcome carries the key's id.
            HandshakeNote::Authenticated(_) => {}
            HandshakeNote::BadMac(scope) => {
                tracing::info!(?peer, ?scope, "vike-gatewayhub: auth denied (bad mac)")
            }
        },
    );
    if let HandshakeOutcome::Authed { scope, key_id } = &outcome {
        // A control-scope connection spends the daemon's credentials: it is an audit line. A read-scope
        // connection is also every dial-per-call of a remote seam, so it is a debug line.
        if *scope == Scope::Write {
            tracing::info!(
                ?peer,
                ?scope,
                key_id = key_id.as_deref(),
                "vike-gatewayhub: authenticated"
            );
        } else {
            tracing::debug!(
                ?peer,
                ?scope,
                key_id = key_id.as_deref(),
                "vike-gatewayhub: authenticated"
            );
        }
    }
    outcome
}
