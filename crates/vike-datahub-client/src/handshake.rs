//! The KEYED pre-auth handshake both data-plane daemons run — the datahub and the backtesthub:
//! `Hello` -> `Welcome{nonce}` -> `Auth` -> verify, spelled ONCE (the refusal order, every reason on
//! the wire, the DATAHUB domain separator), as `bind` is the bind posture they share. Each daemon
//! keeps its own LOG lines — its words and its `tracing` target, which subscribers filter on — so the
//! steps worth a line reach the caller as [`HandshakeNote`]s, at the moment each happens.
//!
//! Both frames are read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB
//! ceiling — an unauthenticated peer must not be able to name a large allocation — and the caller
//! arms the handshake deadline (`vike_hub_proto::limits::SERVER_HANDSHAKE_DEADLINE` in production) as
//! the read timeout first, so a peer that opens a socket and says nothing frees its thread in
//! seconds rather than minutes.

use std::io::{Read, Write};

use vike_hub_proto::auth::{DATAHUB_DOMAIN, HubKeys, Scope, fresh_nonce};
use vike_hub_proto::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;

use crate::PROTO_VERSION;
use crate::proto::{Request, Response, read_frame_raw_capped, request_kind, write_frame};

/// The result of the pre-auth phase on a KEYED server.
#[derive(Debug)]
pub enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed to the session with it as the ceiling.
    Authed(Scope),
    /// Refused (or a transport error) — the connection must close.
    Closed,
}

/// A step of [`run_keyed_handshake`] a daemon logs, in its own words, when it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeNote {
    /// The first frame was a VERB (its `request_kind`), not `Hello`: refused, the socket dropped.
    VerbRefused(&'static str),
    /// The client's `Hello` named this other protocol version; auth will fail on the signed one.
    VersionDiffers(u32),
    /// `Auth` asked for a scope this server holds no key for: refused without consulting a key.
    NoKey(Scope),
    /// The mac verified and `AuthOk` went out.
    Authenticated(Scope),
    /// The mac did not verify.
    BadMac(Scope),
}

/// Run `Hello` -> `Welcome{nonce}` -> `Auth` -> verify on a KEYED server, telling `note` each step a
/// daemon logs. The mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is
/// refused WITHOUT consulting it (the closed-gate shape), and otherwise `HubKeys::verify` decides in
/// constant time under [`DATAHUB_DOMAIN`], so a tag minted for the tradehub never verifies here.
pub fn run_keyed_handshake<S: Read + Write>(
    stream: &mut S,
    keys: &HubKeys,
    features: &[String],
    mut note: impl FnMut(HandshakeNote),
) -> HandshakeOutcome {
    // Frame 1 — it MUST be Hello. Anything else is an unauthenticated verb: refuse it here. This is
    // where an unauthenticated LoadBars / Backfill / RunSlice is denied and the socket dropped.
    let body = match read_frame_raw_capped(stream, SERVER_HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { proto_version }) => proto_version,
        Ok(other) => {
            note(HandshakeNote::VerbRefused(request_kind(&other)));
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Hello (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // The challenge. We always answer Hello with our version + a fresh nonce; a client on another
    // protocol version cannot forge a valid mac anyway (the version is SIGNED), so a skew fails
    // cleanly at verify rather than needing a branch here.
    let nonce = fresh_nonce();
    if write_frame(
        stream,
        &Response::Welcome {
            proto_version: PROTO_VERSION,
            features: features.to_vec(),
            nonce: Some(nonce),
        },
    )
    .is_err()
    {
        return HandshakeOutcome::Closed;
    }
    if client_version != PROTO_VERSION {
        note(HandshakeNote::VersionDiffers(client_version));
    }

    // Frame 2 — the Auth answer. Still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_frame_raw_capped(stream, SERVER_HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let (scope, mac) = match serde_json::from_slice::<Request>(&body2) {
        Ok(Request::Auth { scope, mac }) => (scope, mac),
        Ok(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth after Welcome".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // A scope this server holds NO key for is refused without consulting the key — the closed-gate
    // shape. It is also how an observe-only daemon declines control outright rather than letting
    // an empty key decide it by accident.
    if !keys.has(scope) {
        note(HandshakeNote::NoKey(scope));
        let _ = write_frame(
            stream,
            // Deliberately the SAME coarse reason a bad mac gets: distinguishing "no key for that
            // scope" from "wrong key for that scope" tells an unauthenticated peer which scopes
            // this server offers. The daemon's log line carries the real answer.
            &Response::AuthDenied { reason: "bad mac".into() },
        );
        return HandshakeOutcome::Closed;
    }

    // Constant-time verify against the REQUESTED scope's key, under the DATAHUB domain separator —
    // so a tag minted for the tradehub (which may hold the same key bytes) never verifies here.
    if keys.verify(DATAHUB_DOMAIN, scope, &nonce, PROTO_VERSION, &mac) {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        note(HandshakeNote::Authenticated(scope));
        HandshakeOutcome::Authed(scope)
    } else {
        note(HandshakeNote::BadMac(scope));
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

#[path = "handshake_tests.rs"]
#[cfg(test)]
mod handshake_tests;
