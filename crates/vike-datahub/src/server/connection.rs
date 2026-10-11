//! One connection's loop: the handshake, the plane and scope checks, and the write of each step.

use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use vike_data::HistStore;
use vike_datahub_client::proto::{
    Plane, Request, Response, VerbScope, plane_of, read_frame_raw, request_kind, required_scope,
    scope_admits, write_frame, wrong_plane_message,
};
use vike_node_proto::auth::{NodeKeys, Scope};

use crate::backfill::BackfillTable;
use crate::catalog::CatalogLane;
use crate::import::ImportLane;
use crate::md::MdHub;
use crate::seed::SeedLane;

use super::features::served_features;
use super::handshake::{HandshakeOutcome, run_handshake};
use super::limits::{IDLE_READ_TIMEOUT, ReadCeilings};
use super::market_data::run_market_writer;
use super::route::dispatch;
use super::step::{Step, StopProbe};

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Never panics the caller — a fault logs and returns, dropping the
/// socket.
///
/// Framing and decoding are separate (PR-2): the body bytes are read with [`read_frame_raw`] and
/// then decoded, so a well-framed but undecodable request is answered with [`Response::Error`] and
/// the loop CONTINUES — the connection survives a bad request. A read timeout (an idle/half-open
/// peer) or any other read fault CLOSES the connection; a timeout is never recovered into a resumed
/// loop, which would desync the stream.
///
/// `handshake_deadline` is the pre-auth read timeout on a KEYED server — [`HANDSHAKE_DEADLINE`]
/// from every production entry.
// and the handshake deadline
pub(super) fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<Arc<BackfillTable>>,
    keys: Option<&NodeKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
    ceilings: ReadCeilings,
    handshake_deadline: Duration,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");
    // Nagle OFF before the first frame, on every connection — positional and, after a mode switch,
    // the market-data stream alike (`run_market_writer` inherits it). Why, and why a refusal is
    // only logged: `vike_node_proto::frame::configure_node_stream`.
    if let Err(e) = vike_node_proto::frame::configure_node_stream(&stream) {
        tracing::warn!(?peer, error = %e, "vike-datahub: TCP_NODELAY refused; frames may wait for an ACK");
    }

    let features = served_features(
        backfill.is_some(),
        backfill.as_deref().is_some_and(BackfillTable::has_funding),
        keys.is_some(),
        md.as_deref(),
        seed.is_some(),
        catalog.is_some(),
        import.as_deref(),
    );

    // On a KEYED server the PRE-AUTH phase runs first, under a far shorter deadline than the idle
    // timeout below: an unauthenticated peer has nothing to think about, so a handshake that has not
    // completed in seconds is a socket somebody opened and said nothing on.
    //
    // `None` — the key-less server — takes NEITHER branch: no handshake, no deadline, no scope. That
    // is what makes the key-less path byte-identical to the pre-auth protocol rather than merely
    // similar to it.
    let authed: Option<Scope> = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(handshake_deadline)) {
                tracing::warn!(?peer, error = %e, "vike-datahub: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer) {
                HandshakeOutcome::Authed(scope) => Some(scope),
                HandshakeOutcome::Closed => {
                    tracing::info!(?peer, "vike-datahub: connection closed (handshake refused)");
                    return;
                }
            }
        }
        None => None,
    };

    // Bound how long a single read may block, so a half-open peer cannot pin this thread forever. If
    // even setting the timeout fails, close rather than risk an unbounded park. On the keyed path
    // this also RESETS the short handshake deadline — an authenticated client may legitimately idle
    // between requests, exactly like an unauthenticated one always could.
    if let Err(e) = stream.set_read_timeout(Some(IDLE_READ_TIMEOUT)) {
        tracing::warn!(?peer, error = %e, "vike-datahub: could not set read timeout, closing connection");
        return;
    }

    loop {
        // Read the next frame's BODY BYTES only — decode is deliberately separate (below).
        let body = match read_frame_raw(&mut stream) {
            Ok(body) => body,
            Err(e) => {
                match e.kind() {
                    // The normal way a connection ends — no log.
                    io::ErrorKind::UnexpectedEof => {}
                    // No (further) bytes arrived within the window: an idle or half-open connection.
                    // Close it to free the thread; a live client simply reconnects for its next
                    // request. We never RESUME after a timeout — a mid-frame timeout would otherwise
                    // leave the stream desynced.
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(
                            ?peer,
                            "vike-datahub: idle read timeout, closing connection"
                        );
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "vike-datahub: read fault, closing connection");
                    }
                }
                break;
            }
        };

        // THIS REQUEST'S STOP PROBE, over a SHARED borrow of the stream: from here until `dispatch`
        // returns, this loop does not read the socket, so a peek is the only thing looking at it.
        // Only `backfill_verb` and the archive import ever ask it — every other verb leaves it
        // unasked, which costs them nothing and changes nothing. See `StopProbe`.
        let probe = StopProbe::new(&stream, peer);

        // Decode the body. A well-framed body that is not a known `Request` (an unknown/incompatible
        // verb from a mismatched client) is answered with `Response::Error` and the loop CONTINUES —
        // one bad request never drops the connection.
        //
        // ⚠ **THE STEP IS A `ControlFlow`-SHAPED VALUE RATHER THAN A `Response`, and §8 item 10's
        // "ONE arm" understates it.** `MdSubscribe` is this wire's one MODE SWITCH: it must run
        // AFTER the scope check on BOTH the keyed and the key-less arms, and then RETURN without
        // falling through to the unconditional write below — because from that point the socket
        // belongs to `run_market_writer` and the reply it writes is the LAST positional frame.
        let step = match serde_json::from_slice::<Request>(&body) {
            // ⚠ THE PLANE CHECK COMES BEFORE THE SCOPE CHECK, and the order is the whole point of
            // it (ruling 7). A verb this daemon does not serve AT ALL is not a scope question: an
            // Observe connection sending `RunBacktest` would otherwise be told "that requires the
            // Control scope", which is true of the OTHER daemon and useless here — it sends the
            // reader looking for a key when what they need is a different address. Asked first, the
            // answer names where the verb went.
            //
            // It applies on BOTH auth arms (key-less and keyed) because it is a property of this
            // build's served surface, not of the connection.
            Ok(request) if plane_of(&request) == Plane::Compute => {
                let kind = request_kind(&request);
                tracing::info!(
                    ?peer,
                    verb = kind,
                    "vike-datahub: verb refused — it moved to the compute daemon"
                );
                Step::reply(Response::Error(wrong_plane_message(kind, Plane::Data, Plane::Compute)))
            }
            Ok(request) => match authed {
                // KEYED: check the verb against the connection's authenticated ceiling BEFORE it
                // reaches `handle_request`. A refusal is answered and the loop CONTINUES — an
                // Observe client asking for a Control verb has made a bad *request*, not opened a
                // bad *connection*, and dropping it would make an over-scoped read indistinguishable
                // from a transport fault. (An UNAUTHENTICATED verb is a different matter and is
                // refused with a closed socket, in `run_handshake`.)
                Some(scope) => {
                    let needed = required_scope(&request);
                    if scope_admits(scope, needed) {
                        dispatch(
                            request,
                            &store,
                            backfill.as_deref(),
                            true,
                            md.as_ref(),
                            seed.as_deref(),
                            catalog.as_deref(),
                            import.as_deref(),
                            ceilings,
                            &probe,
                        )
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-datahub: verb refused — outside this connection's scope"
                        );
                        Step::reply(Response::Error(match needed {
                            VerbScope::Handshake => format!(
                                "{kind} is a handshake frame; this connection is already \
                                 authenticated"
                            ),
                            // ⚠ The `Run*` verbs are NOT named here any more — they are not served
                            // by this daemon at all since ruling 7, and the guard above answers
                            // them before this arm is reached. `Backfill`, `ImportArchive` and
                            // `DeleteSeries` are what is left behind the Control scope on the data
                            // plane (`ImportArchive` since `docs/decisions/0100`'s verdict 1).
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Backfill store WRITE, the ImportArchive store \
                                 WRITE and the DeleteSeries store REMOVAL are Control-only"
                            ),
                        }))
                    }
                }
                // KEY-LESS: no scope to check, and unchanged for every verb that predates the
                // keys. ⚠ NOT "every verb", which is what this comment claimed until 2026-09-07:
                // the `false` below IS the `keyed` argument `delete_series_verb` refuses on, so
                // the code this line annotates is what makes the old claim false.
                None => dispatch(
                    request,
                    &store,
                    backfill.as_deref(),
                    false,
                    md.as_ref(),
                    seed.as_deref(),
                    catalog.as_deref(),
                    import.as_deref(),
                    ceilings,
                    &probe,
                ),
            },
            Err(e) => {
                Step::reply(Response::Error(format!("unrecognized/undecodable request: {e}")))
            }
        };
        // Read before the stream is borrowed mutably to write: the probe's borrow ends here.
        let mode_lost = probe.mode_lost();

        match step {
            Step::Reply(response) => {
                if let Err(e) = write_frame(&mut stream, &*response) {
                    tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
                    break;
                }
                // A peek that could not put the socket back into blocking mode leaves it in a mode
                // nobody can vouch for: the reply is written, and then the connection closes rather
                // than reading the next frame from a socket that may answer `WouldBlock` mid-frame.
                if mode_lost {
                    tracing::warn!(
                        ?peer,
                        "vike-datahub: a backfill's peer check could not restore blocking mode on \
                         this socket; closing the connection after its reply"
                    );
                    break;
                }
            }
            // THE CLOSE. The request ran for a client that has gone; nobody would read a reply, so
            // none is written — which is also what keeps a misleading "write fault" out of the log.
            // `backfill_verb` has logged the request's own line already.
            Step::Close => break,
            // ⚠ THE MODE SWITCH. Everything after this point on this socket is a pushed
            // `Response::Md`, and nothing reads this direction again.
            Step::ModeSwitch(guard, specs) => {
                let hub = md.expect("ModeSwitch is produced only where a hub is mounted");
                run_market_writer(stream, hub, guard, specs, peer);
                return;
            }
        }
    }
    tracing::info!(?peer, "vike-datahub: connection closed");
}
