//! The pre-auth phase: `Hello` -> `Welcome{features, nonce}` -> `Auth{scope, mac}` -> `AuthOk` or the
//! coarse `AuthDenied`. ALWAYS keyed: there is no key-less mode (decision 0164, verdict 6), so
//! `crate::server::accept::serve` refuses to start without a key and this module never asks "is auth
//! on".
//!
//! The shape is `crates/vike-datahub/src/server/handshake.rs`'s `run_handshake`, in the gatewayhub's
//! domain (`GATEWAYHUB_DOMAIN`, signed together with `GATEWAYHUB_PROTO_VERSION` and the scope) with
//! the gatewayhub's constants. Both pre-auth frames are read under the 64 KiB pre-auth cap and the
//! WHOLE phase under ONE deadline (`run_handshake`'s `deadline`, turned into an `Instant` when
//! the phase starts): a peer that opens a socket and says nothing frees its thread in seconds, and so does one that
//! trickles a byte just inside every read timeout. A socket read timeout alone bounds each `read`, not
//! the phase, and a peer sending 1 byte per timeout would hold the thread (and one of the connection
//! slots) for 65540 timeouts; `ByDeadline` re-arms the timeout to the time that is LEFT before every
//! read.
//!
//! # Refusals are coarse
//!
//! A scope this server holds NO key for, a scope it does not serve at all and a wrong mac all answer
//! the same `bad mac`: distinguishing them tells an unauthenticated peer which scopes the daemon
//! offers. The operator's log line carries the real answer. Wave 1 serves `Scope::Read` and
//! `Scope::Write` only: `Scope::Account` admits nothing here even when the key set carries an admin
//! key, and `Scope::Trade` does not exist yet.

use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::proto::{
    GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, Request, Response, read_frame_raw_capped,
    request_kind,
};
use vike_hub_proto::auth::{HubKeys, Scope, fresh_nonce};
use vike_hub_proto::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;
use vike_market_stream::frame::write_capped;

/// The result of the pre-auth phase.
pub enum HandshakeOutcome {
    /// The client authenticated under `scope`; `key_id` is the non-secret fingerprint of the key that
    /// did it.
    Authed {
        /// The connection's capability ceiling.
        scope: Scope,
        /// The key's `nk-` fingerprint, for the audit trail.
        key_id: Option<String>,
    },
    /// Refused or a transport fault: the connection must close.
    Closed,
}

/// The scopes wave 1 serves.
pub fn scope_is_served(scope: Scope) -> bool {
    matches!(scope, Scope::Read | Scope::Write)
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
    let by = Instant::now() + deadline;
    let body = match read_by(stream, by) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { proto_version }) => proto_version,
        Ok(other) => {
            tracing::info!(
                ?peer,
                verb = request_kind(&other),
                "vike-gatewayhub: verb refused - connection is not authenticated"
            );
            deny(stream, "not authenticated: expected Hello first", ceiling);
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            deny(stream, "expected Hello (undecodable request)", ceiling);
            return HandshakeOutcome::Closed;
        }
    };

    // The challenge. A client on another protocol version cannot forge a valid mac anyway (the
    // version is SIGNED), so a skew fails cleanly at verify rather than needing a branch here; the
    // CLIENT checks the version first and names both numbers.
    let nonce = fresh_nonce();
    let welcome = Response::Welcome {
        proto_version: GATEWAYHUB_PROTO_VERSION,
        features: features.to_vec(),
        nonce,
    };
    if write_capped(stream, &welcome, ceiling).is_err() {
        return HandshakeOutcome::Closed;
    }
    if client_version != GATEWAYHUB_PROTO_VERSION {
        tracing::debug!(
            ?peer,
            client_version,
            server_version = GATEWAYHUB_PROTO_VERSION,
            "vike-gatewayhub: client protocol version differs; auth will fail on the signed version"
        );
    }

    let body2 = match read_by(stream, by) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let (scope, mac) = match serde_json::from_slice::<Request>(&body2) {
        Ok(Request::Auth { scope, mac }) => (scope, mac),
        Ok(_) => {
            deny(stream, "expected Auth after Welcome", ceiling);
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            deny(stream, "expected Auth (undecodable request)", ceiling);
            return HandshakeOutcome::Closed;
        }
    };

    // A scope this daemon does not serve, or holds NO key for, is refused WITHOUT consulting a key:
    // the closed-gate shape. The reply is the same coarse one a bad mac gets.
    if !scope_is_served(scope) || !keys.has(scope) {
        tracing::info!(
            ?peer,
            ?scope,
            "vike-gatewayhub: auth refused - this scope is not served, or no key is configured for it"
        );
        deny(stream, "bad mac", ceiling);
        return HandshakeOutcome::Closed;
    }

    if keys.verify(GATEWAYHUB_DOMAIN, scope, &nonce, GATEWAYHUB_PROTO_VERSION, &mac) {
        if write_capped(stream, &Response::AuthOk { scope }, ceiling).is_err() {
            return HandshakeOutcome::Closed;
        }
        let key_id = keys.key_id(scope);
        // A control-scope connection spends the daemon's credentials: it is an audit line. A read-scope
        // connection is also every dial-per-call of a remote seam, so it is a debug line.
        if scope == Scope::Write {
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
        HandshakeOutcome::Authed { scope, key_id }
    } else {
        tracing::info!(?peer, ?scope, "vike-gatewayhub: auth denied (bad mac)");
        deny(stream, "bad mac", ceiling);
        HandshakeOutcome::Closed
    }
}

/// Write the coarse refusal; a failed write changes nothing (the connection closes either way).
fn deny(stream: &mut TcpStream, reason: &str, ceiling: u32) {
    let _ = write_capped(stream, &Response::AuthDenied { reason: reason.to_string() }, ceiling);
}

/// One pre-auth frame, read under the phase's `by` and the pre-auth frame cap.
fn read_by(stream: &mut TcpStream, by: Instant) -> io::Result<Vec<u8>> {
    read_frame_raw_capped(&mut ByDeadline { stream, by }, SERVER_HANDSHAKE_MAX_FRAME_LEN)
}

/// A `Read` over the handshake socket that arms the socket's read timeout to the time LEFT before
/// `by` ahead of every read, and fails with `TimedOut` once `by` has passed. The frame codec reads a
/// length prefix and a body with `read_exact`, i.e. in as many `read`s as the peer cares to split them
/// into, so a per-`read` timeout fixed at the start bounds a silent peer and not a slow one.
struct ByDeadline<'a> {
    stream: &'a mut TcpStream,
    by: Instant,
}

impl Read for ByDeadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the handshake deadline passed before the frame was complete",
            ));
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}
