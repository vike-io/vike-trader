//! One connection's life: the handshake, then the verb loop that classifies each connection by its
//! first post-auth request and hands it to the class that serves it.
//!
//! # Decode versus drop
//!
//! A frame is read as raw bytes under `GW_UP_FRAME_CEILING_BYTES` and decoded separately, so a
//! well-framed body that is not a known `Request` (a verb from a newer client) is a bad REQUEST
//! answered with `Response::Error` on a connection that survives; only a framing fault (a length over
//! the ceiling, a torn frame, a timeout) closes it, because the stream is then desynced. A timeout is
//! never recovered into a resumed read.
//!
//! # Scope before dispatch
//!
//! `vike_gatewayhub_client::proto::required_scope` is the ONE authority for which verb needs which
//! scope (an exhaustive match in the crate both ends share); the check runs before any verb's own code,
//! so a `Scope::Read` key can never open a credentialed lane, nor walk a venue's history through a
//! window it names, however the verb is spelled. A refusal is answered and the loop CONTINUES: an
//! over-scoped read is a bad request, not a bad connection.
//!
//! # The idle read timeout
//!
//! An `rpc` connection that sends nothing for `ServerTimings::idle_read_timeout` is closed (a
//! half-open peer must not pin a thread). A `stream` or `feed` connection is not subject to it: its
//! client is silent by design, and the daemon proves liveness with heartbeats instead
//! (`crate::server::stream`).
//!
//! The idle timeout bounds the GAP between frames only. A frame that has BEGUN (its first byte is read)
//! must finish within `ServerTimings::frame_read_timeout`, the bound the fetch monitor and the stream
//! reader already use: an authenticated peer that sends a length prefix and stalls would otherwise hold
//! a connection thread for the whole idle bound, 300 s, per read.

use std::collections::VecDeque;
use std::io;
use std::net::TcpStream;
use std::sync::Arc;

use vike_gatewayhub_client::proto::{
    Request, Response, Surface, VerbScope, request_kind, required_scope, scope_admits, surface_of,
};
use vike_hub_proto::auth::Scope;
use vike_hub_proto::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;

use super::ctl::ConnCtl;
use super::feed;
use super::frame::{Ready, read_client_body, wait_readable};
use super::handshake::{HandshakeOutcome, run_handshake};
use super::rpc;
use super::shared::{After, Conn, Shared};
use super::stream;
use crate::session::ConnectionScope;

/// The refusal text for a verb outside the connection's scope.
fn scope_refusal(kind: &str, needed: VerbScope, have: Scope) -> String {
    match needed {
        VerbScope::Handshake => {
            format!("{kind} is a handshake frame; this connection is already authenticated")
        }
        VerbScope::Read => format!(
            "{kind} requires the {} scope; this connection authenticated as {}",
            Scope::Read.name(),
            have.name()
        ),
        VerbScope::Trade => format!(
            "{kind} requires the {} scope; this connection authenticated as {}",
            Scope::Trade.name(),
            have.name()
        ),
        VerbScope::Write => format!(
            "{kind} requires the {} scope; this connection authenticated as {}. A credentialed \
             fetch spends a credential the daemon holds, and a klines, funding or ticks fetch \
             walks a venue's history through a window the caller names: both are {}-only",
            Scope::Write.name(),
            have.name(),
            Scope::Write.name()
        ),
    }
}

impl Conn {
    /// The next request frame's bytes: a stashed one first, then the socket. `Ok(None)` is a clean end
    /// (the peer closed, or the daemon is stopping); `Err` is a fault the caller logs.
    fn next_body(&mut self) -> io::Result<Option<Vec<u8>>> {
        if let Some(body) = self.stash.pop_front() {
            return Ok(Some(body));
        }
        if self.ctl.is_stopping() {
            return Ok(None);
        }
        let timings = self.shared.config.timings;
        // The GAP between frames is the idle timeout's: wait for a frame to BEGIN without consuming a
        // byte. Once its first byte is here the REST of it is `frame_read_timeout`'s, so a peer that
        // sends a length prefix and stalls holds this thread for seconds and not for the idle bound.
        match wait_readable(&self.stream, timings.idle_read_timeout)? {
            Ready::Timeout => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no request frame began within the idle read timeout",
                ));
            }
            Ready::Eof => return Ok(None),
            Ready::Data => {}
        }
        match read_client_body(&mut self.stream, timings.frame_read_timeout) {
            Ok(body) => Ok(Some(body)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Serve one accepted connection to its end. Never panics the caller: a fault is logged and the socket
/// dropped.
pub fn handle_connection(shared: &Arc<Shared>, stream: TcpStream, ctl: Arc<ConnCtl>) {
    let mut stream = stream;
    let peer = stream.peer_addr().ok();
    tracing::debug!(?peer, "vike-gatewayhub: connection opened");
    if let Err(e) = vike_hub_proto::frame::configure_hub_stream(&stream) {
        tracing::warn!(?peer, error = %e, "vike-gatewayhub: TCP_NODELAY refused; frames may wait for an ACK");
    }
    let timings = shared.config.timings;
    if let Err(e) = stream
        .set_read_timeout(Some(timings.handshake_deadline))
        .and_then(|()| stream.set_write_timeout(Some(timings.write_timeout)))
    {
        tracing::warn!(?peer, error = %e, "vike-gatewayhub: could not arm the socket timeouts; closing");
        return;
    }
    let (scope, key_id) = match run_handshake(
        &mut stream,
        &shared.keys,
        &shared.features,
        peer,
        SERVER_HANDSHAKE_MAX_FRAME_LEN,
        timings.handshake_deadline,
    ) {
        HandshakeOutcome::Authed { scope, key_id } => (scope, key_id),
        HandshakeOutcome::Closed => {
            tracing::debug!(?peer, "vike-gatewayhub: connection closed (handshake refused)");
            return;
        }
    };
    let mut conn = Conn {
        shared: Arc::clone(shared),
        stream,
        peer,
        scope,
        key_id,
        memo: Arc::new(ConnectionScope::new()),
        ctl,
        stash: VecDeque::new(),
    };
    serve_requests(&mut conn);
    tracing::debug!(?peer, "vike-gatewayhub: connection closed");
}

/// The verb loop.
fn serve_requests(conn: &mut Conn) {
    loop {
        let body = match conn.next_body() {
            Ok(Some(body)) => body,
            Ok(None) => break,
            Err(e) => {
                match e.kind() {
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(peer = ?conn.peer, "vike-gatewayhub: idle read timeout; closing");
                    }
                    _ => {
                        tracing::warn!(peer = ?conn.peer, error = %e, "vike-gatewayhub: read fault; closing");
                    }
                }
                break;
            }
        };
        let request = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => request,
            Err(e) => {
                if conn
                    .reply(&Response::Error(format!("unrecognized/undecodable request: {e}")))
                    .is_err()
                {
                    break;
                }
                continue;
            }
        };
        let kind = request_kind(&request);
        let needed = required_scope(&request);
        if !scope_admits(conn.scope, needed) {
            tracing::info!(
                peer = ?conn.peer,
                scope = ?conn.scope,
                ?needed,
                verb = kind,
                "vike-gatewayhub: verb refused - outside this connection's scope"
            );
            if conn.reply_error(scope_refusal(kind, needed, conn.scope)).is_err() {
                break;
            }
            continue;
        }
        let after = match surface_of(&request) {
            Surface::Rpc => rpc::serve(conn, request),
            Surface::Stream => match request {
                Request::DataOpen { venue, network } => {
                    stream::serve(conn, &venue, network.as_deref())
                }
                other => refuse_surface(conn, &other),
            },
            Surface::Feed => match request {
                Request::OptionsFeedOpen { feed: spec } => feed::serve(conn, &spec),
                other => refuse_surface(conn, &other),
            },
            // The trade lease is the trade lane's: `TradeOpen` is the mode switch, served in a `trade` build
            // whose lane is mounted. In any other build `scope_is_served` refuses `Scope::Trade` at the
            // handshake, so a `TradeOpen` is unreachable and answered as a surface this daemon does not
            // serve if it is ever reached.
            Surface::Trade => match request {
                #[cfg(feature = "trade")]
                Request::TradeOpen { client_id, venue, account, tier, resume, takeover } => {
                    super::trade::serve(
                        conn,
                        super::trade::OpenArgs {
                            client_id,
                            venue,
                            account,
                            tier,
                            resume,
                            takeover,
                        },
                    )
                }
                other => refuse_surface(conn, &other),
            },
            // Unreachable: `scope_admits` refuses a handshake verb above. Answered all the same.
            Surface::Handshake => refuse_surface(conn, &request),
        };
        if after == After::Close {
            break;
        }
    }
}

/// A verb whose class cannot be served here: answered, connection kept.
fn refuse_surface(conn: &mut Conn, request: &Request) -> After {
    let text = format!("gatewayhub: {} cannot be served on this connection", request_kind(request));
    match conn.reply_error(text) {
        Ok(()) => After::Continue,
        Err(_) => After::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_refusal_names_the_verb_and_both_scopes() {
        let text = scope_refusal("FetchCredentialed", VerbScope::Write, Scope::Read);
        assert!(
            text.contains("FetchCredentialed")
                && text.contains("control")
                && text.contains("observe"),
            "{text}"
        );
        let text = scope_refusal("Hello", VerbScope::Handshake, Scope::Write);
        assert!(text.contains("already authenticated"), "{text}");
    }
}
