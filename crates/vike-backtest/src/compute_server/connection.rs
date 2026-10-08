//! One connection: the keyed pre-auth handshake, then the read / plane / scope / answer loop.

use std::io;
use std::net::{SocketAddr, TcpStream};

use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{
    Plane, Request, Response, VerbScope, plane_of, read_frame_raw, read_frame_raw_capped,
    request_kind, required_scope, scope_admits, write_frame, wrong_plane_message,
};
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope, fresh_nonce};

use super::dispatch::{handle_request, served_features};
use super::{
    HANDSHAKE_DEADLINE, HANDSHAKE_MAX_FRAME_LEN, IDLE_READ_TIMEOUT, StoreHandle, StudioRunTable,
    StudyRunFn,
};
use crate::named_run::NamedRunLane;

/// The result of the pre-auth phase on a KEYED server.
enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed with it as the ceiling.
    Authed(Scope),
    /// Refused (or a transport error) — the connection must close.
    Closed,
}

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server. Mirrors
/// `vike_datahub::server`'s `run_handshake`, including its refusal ordering: a first frame that is
/// not `Hello` is an unauthenticated VERB and the socket is dropped; a scope this server holds no
/// key for is refused WITHOUT consulting the key; the mac is verified in constant time under the
/// DATAHUB domain separator, so a tag minted for the tradehub node never verifies here.
fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    features: &[String],
    peer: Option<SocketAddr>,
) -> HandshakeOutcome {
    // Frame 1 — it MUST be Hello. Anything else is an unauthenticated verb: refuse it here.
    let body = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { proto_version }) => proto_version,
        Ok(other) => {
            tracing::info!(
                ?peer,
                verb = request_kind(&other),
                "vike-backtest serve: verb refused — connection is not authenticated"
            );
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

    // The challenge. A client on another protocol version cannot forge a valid mac anyway (the
    // version is SIGNED), so a skew fails cleanly at verify rather than needing a branch here.
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
        tracing::debug!(
            ?peer,
            client_version,
            server_version = PROTO_VERSION,
            "vike-backtest serve: client protocol version differs; auth will fail on the signed \
             version"
        );
    }

    // Frame 2 — the Auth answer. Still PRE-AUTH, same small ceiling.
    let body2 = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
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
    // shape, and how an observe-only deployment declines control outright.
    if !keys.has(scope) {
        tracing::info!(
            ?peer,
            ?scope,
            "vike-backtest serve: auth refused — no key configured for this scope on this server"
        );
        let _ = write_frame(
            stream,
            // Deliberately the SAME coarse reason a bad mac gets: distinguishing "no key for that
            // scope" from "wrong key for that scope" tells an unauthenticated peer which scopes
            // this server offers.
            &Response::AuthDenied { reason: "bad mac".into() },
        );
        return HandshakeOutcome::Closed;
    }

    if auth::verify(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope, &mac) {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        tracing::info!(?peer, ?scope, "vike-backtest serve: authenticated");
        HandshakeOutcome::Authed(scope)
    } else {
        tracing::info!(?peer, ?scope, "vike-backtest serve: auth denied (bad mac)");
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Framing and decoding are separate, so a well-framed but
/// undecodable request is answered with [`Response::Error`] and the loop CONTINUES — the connection
/// survives a bad request. A read timeout CLOSES the connection and is never recovered into a
/// resumed loop, which would desync the stream.
pub(super) fn handle_connection(
    mut stream: TcpStream,
    store: StoreHandle,
    studio: Option<&StudioRunTable>,
    study: Option<&StudyRunFn>,
    keys: Option<&NodeKeys>,
    named_run: NamedRunLane,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-backtest serve: connection opened");
    // Nagle OFF before the first frame. Why, and why a refusal is only logged:
    // `vike_node_proto::frame::configure_node_stream`.
    if let Err(e) = vike_node_proto::frame::configure_node_stream(&stream) {
        tracing::warn!(?peer, error = %e, "vike-backtest serve: TCP_NODELAY refused; frames may wait for an ACK");
    }

    let features = served_features(studio.is_some(), study.is_some(), keys.is_some());

    let authed: Option<Scope> = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(HANDSHAKE_DEADLINE)) {
                tracing::warn!(?peer, error = %e, "vike-backtest serve: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer) {
                HandshakeOutcome::Authed(scope) => Some(scope),
                HandshakeOutcome::Closed => {
                    tracing::info!(
                        ?peer,
                        "vike-backtest serve: connection closed (handshake refused)"
                    );
                    return;
                }
            }
        }
        None => None,
    };

    if let Err(e) = stream.set_read_timeout(Some(IDLE_READ_TIMEOUT)) {
        tracing::warn!(?peer, error = %e, "vike-backtest serve: could not set read timeout, closing connection");
        return;
    }

    loop {
        let body = match read_frame_raw(&mut stream) {
            Ok(body) => body,
            Err(e) => {
                match e.kind() {
                    // The normal way a connection ends — no log.
                    io::ErrorKind::UnexpectedEof => {}
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(
                            ?peer,
                            "vike-backtest serve: idle read timeout, closing connection"
                        );
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "vike-backtest serve: read fault, closing connection");
                    }
                }
                break;
            }
        };

        let response = match serde_json::from_slice::<Request>(&body) {
            // ⚠ THE PLANE CHECK COMES BEFORE THE SCOPE CHECK — the mirror image of the guard in
            // `vike_datahub::server`'s connection loop, and for the same reason: a verb this daemon
            // does not serve at all is not a scope question, and answering "that requires Control"
            // about a `LoadBars` would send the reader looking for a key when what they need is a
            // different address.
            Ok(request) if plane_of(&request) == Plane::Data => {
                let kind = request_kind(&request);
                tracing::info!(
                    ?peer,
                    verb = kind,
                    "vike-backtest serve: verb refused — it belongs to the data daemon"
                );
                Response::Error(wrong_plane_message(kind, Plane::Compute, Plane::Data))
            }
            Ok(request) => match authed {
                Some(scope) => {
                    let needed = required_scope(&request);
                    if scope_admits(scope, needed) {
                        handle_request(request, &store, studio, study, named_run)
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-backtest serve: verb refused — outside this connection's scope"
                        );
                        Response::Error(match needed {
                            VerbScope::Handshake => format!(
                                "{kind} is a handshake frame; this connection is already \
                                 authenticated"
                            ),
                            // ⚠ This said "Every Run* verb is Control-only" until 2026-09-16 and
                            // it is now one verb too wide in the direction that MATTERS to the
                            // reader: `RunNamed` is `VerbScope::Read`
                            // (`docs/decisions/0064-a-named-run-carries-no-source.md`), so an
                            // Observe peer told "every Run* verb is Control" would conclude it
                            // needs a Control key when the verb it wants is already reachable.
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Run* verbs that carry SOURCE — a profile's \
                                 `[strategy.params].src`, a WireSpec — are Control-only because \
                                 they COMPILE CLIENT-SUPPLIED RHAI on this server, and RunStudy is \
                                 Control because it WRITES a run directory. The Observe half of \
                                 this daemon is Ping, ListStrategies, NamedStrategies and RunNamed \
                                 — the last of which runs one strategy this server already holds, \
                                 over one bounded window, and is what an Observe client wants"
                            ),
                        })
                    }
                }
                None => handle_request(request, &store, studio, study, named_run),
            },
            Err(e) => Response::Error(format!("unrecognized/undecodable request: {e}")),
        };

        if let Err(e) = write_frame(&mut stream, &response) {
            tracing::warn!(?peer, error = %e, "vike-backtest serve: write fault, closing connection");
            break;
        }
    }
    tracing::info!(?peer, "vike-backtest serve: connection closed");
}
