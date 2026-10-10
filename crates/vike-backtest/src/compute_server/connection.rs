//! One connection: the keyed pre-auth handshake, then the read / plane / scope / answer loop, and
//! the peer-gone watch that stops a run whose client has left.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

        // `keep_open` is false only after a watched verb: its client is gone, or the socket could
        // not be put back the way the loop reads it (`answer`'s doc).
        let (response, keep_open) = match serde_json::from_slice::<Request>(&body) {
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
                (Response::Error(wrong_plane_message(kind, Plane::Compute, Plane::Data)), true)
            }
            Ok(request) => match authed {
                Some(scope) => {
                    let needed = required_scope(&request);
                    if scope_admits(scope, needed) {
                        answer(request, &stream, peer, &store, studio, study, named_run)
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-backtest serve: verb refused — outside this connection's scope"
                        );
                        let refusal = Response::Error(match needed {
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
                        });
                        (refusal, true)
                    }
                }
                None => answer(request, &stream, peer, &store, studio, study, named_run),
            },
            Err(e) => (Response::Error(format!("unrecognized/undecodable request: {e}")), true),
        };

        if let Err(e) = write_frame(&mut stream, &response) {
            if keep_open {
                tracing::warn!(?peer, error = %e, "vike-backtest serve: write fault, closing connection");
            }
            break;
        }
        if !keep_open {
            break;
        }
    }
    tracing::info!(?peer, "vike-backtest serve: connection closed");
}

/// The verbs that run under a [`PeerWatch`]: the three profile-shaped RUNS, each of which threads
/// the peer-gone flag into its harness path and stops at its next point, series load or window
/// (`super::verbs`'s `run_backtest`, `run_paramscan_profile`, `run_walkforward_profile`).
///
/// Deliberately NOT watched, and why — a watch on a verb that cannot read the flag would only add a
/// thread and turn a finished answer into a cancellation:
///
/// * `RunSlice`, `RunParamscan`, `RunWalkforward` and `RunStudy` run through runners INJECTED from
///   `vike-studio-core` ([`super::StudioRunTable`], [`super::StudyRunFn`]), whose signatures carry no
///   stop flag. Giving them one is a coordinated change to that crate's runners, above this one in
///   the layer graph, not something this file can do alone. No MCP tool dials them.
/// * `RunNamed` is BOUNDED by construction (`vike_datahub_client::named_run::NAMED_RUN_MAX_BARS`
///   bars, one strategy, a slot table), so it ends in seconds whoever is waiting.
/// * Everything else answers from memory or refuses.
///
/// A `matches!` and not an exhaustive match on purpose: a NEW verb is unwatched until someone
/// threads the flag through it, which is the safe default.
fn is_watched(request: &Request) -> bool {
    matches!(
        request,
        Request::RunBacktest(_)
            | Request::RunParamscanProfile { .. }
            | Request::RunWalkforwardProfile { .. }
    )
}

/// Answer one ADMITTED request: `(response, keep_open)`. A verb [`is_watched`] rejects is
/// [`handle_request`] and keeps the connection open; a watched one runs under a [`PeerWatch`], so a
/// client that closes its socket mid-run stops the run instead of leaving it to burn the box's
/// cores for nobody.
///
/// `keep_open` is false when the client is GONE (the cancellation answer is still written, for a
/// client that only half-closed, and then the connection ends without another read) or when the
/// socket could not be put back into blocking mode — the loop's reads would then fail with
/// `WouldBlock` and read as an idle timeout, so ending the connection on purpose is the honest
/// form of the same outcome.
fn answer(
    request: Request,
    stream: &TcpStream,
    peer: Option<SocketAddr>,
    store: &StoreHandle,
    studio: Option<&StudioRunTable>,
    study: Option<&StudyRunFn>,
    named_run: NamedRunLane,
) -> (Response, bool) {
    if !is_watched(&request) {
        return (handle_request(request, store, studio, study, named_run, None), true);
    }
    let verb = request_kind(&request);
    let watch = match PeerWatch::arm(stream) {
        Ok(w) => w,
        Err(e) => {
            // The run still goes ahead, unwatched — exactly what it did before the watch existed.
            tracing::warn!(?peer, verb, error = %e, "vike-backtest serve: could not watch the client; this run goes to completion even if it leaves");
            let response = handle_request(request, store, studio, study, named_run, None);
            return (response, restore_blocking(stream, peer));
        }
    };
    let response = handle_request(request, store, studio, study, named_run, Some(watch.gone()));
    let (gone, restored) = watch.finish(stream, peer);
    if gone {
        tracing::info!(?peer, verb, "vike-backtest serve: client gone mid-run; run cancelled");
    }
    (response, restored && !gone)
}

/// Put `stream` back into the blocking mode the connection loop reads in; `false` (logged) if the
/// socket refused.
fn restore_blocking(stream: &TcpStream, peer: Option<SocketAddr>) -> bool {
    match stream.set_nonblocking(false) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(?peer, error = %e, "vike-backtest serve: could not restore blocking mode, closing connection");
            false
        }
    }
}

/// How often [`PeerWatch`] looks at the socket: how long a gone client's run keeps starting points,
/// series loads or windows, at worst. It costs a FINISHED run nothing — [`PeerWatch::finish`]
/// unparks the watcher rather than waiting out the interval.
const PEER_POLL: Duration = Duration::from_millis(200);

/// **"Is the client still there?" asked while the connection thread is busy computing.**
///
/// One named thread `peek`s a clone of the socket every [`PEER_POLL`] and sets `gone` when the peek
/// answers `Ok(0)` (the client closed: FIN) or a hard error (reset). `Ok(n > 0)` is a byte the client
/// sent ahead of this verb's answer (a pipelined request): it stays unread for the loop and the
/// watch keeps waiting.
///
/// # ⚠ The blind spot: a close BEHIND pipelined bytes is invisible, and std cannot see past them
///
/// A client that sends a second request while the first runs, then closes, is not seen as gone —
/// its run goes to completion (`crates/vike-backtest/tests/compute_cancel.rs`'s
/// `a_close_behind_a_pipelined_request_is_not_seen` pins exactly that). Why no non-consuming probe
/// can fix it:
///
/// * A FIN is not a byte in the receive queue; the kernel reports it as a zero-length receive, and
///   only once the queue is EMPTY. `recv(MSG_PEEK)` — all [`TcpStream::peek`] is — returns the
///   queued bytes first and never consumes them, so every later peek returns the same bytes and the
///   zero-length answer behind them can never arrive. That holds on Linux and on Windows alike.
/// * std exposes nothing else that reads the peer's half of the connection: no `poll` with
///   `POLLRDHUP` (Linux-only anyway), no `TCP_INFO` connection state, no `SIOCINQ`.
///   [`TcpStream::take_error`] reports a RESET (`SO_ERROR`), never a FIN. Reaching any of those
///   means a raw-socket dependency, which this crate does not take for a client nobody ships.
/// * Probing with a WRITE would elicit a reset from a closed peer, but every byte written is a byte
///   of the protocol stream a live client must parse.
///
/// What WOULD work is CONSUMING: read the pipelined bytes off the socket into a buffer the
/// connection loop then reads first, so the watch can see the FIN behind them. It is not done
/// because it puts a carry-over buffer — bounded at what, when one frame may be
/// `vike_node_proto::frame::MAX_FRAME_LEN` long? — in front of every read after a watched verb, to
/// serve a client shape no caller in this workspace has: the MCP server and the CLI send one
/// request and wait for its answer. The day a pipelining client appears, that is the fix, and the
/// test above turns red to say the blind spot is gone.
///
/// ⚠ **The socket is NON-BLOCKING while the watch is armed, and that is safe only because of who
/// touches it.** Blocking mode is a property of the SOCKET, not of the handle, so the clone's
/// `set_nonblocking(true)` flips the connection's own handle too. Nothing reads or writes that
/// handle while the verb runs — the connection thread is the socket's only reader and writer and
/// it is inside the run, and the compute wire writes no progress frames — and
/// [`PeerWatch::finish`] joins the watcher and puts blocking mode back BEFORE the answer is written
/// and the next request read. A verb that wrote to the socket while running would break this.
///
/// Non-blocking rather than a short read timeout (the first design): a timed-out receive leaves a
/// Windows socket "indeterminate" by Microsoft's own documentation, and a receive blocked in a
/// timeout cannot be woken, so every finished run would wait out the rest of a poll interval
/// before its answer was written. A non-blocking peek and `park_timeout` have neither cost.
struct PeerWatch {
    gone: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

impl PeerWatch {
    /// Flip the socket non-blocking and start the watcher. On `Err` the socket may already be
    /// non-blocking: the caller restores it ([`restore_blocking`]).
    fn arm(stream: &TcpStream) -> io::Result<PeerWatch> {
        let probe = stream.try_clone()?;
        probe.set_nonblocking(true)?;
        let gone = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (gone_w, stop_w) = (Arc::clone(&gone), Arc::clone(&stop));
        let watcher = thread::Builder::new()
            .name("bt-peerwatch".into())
            .spawn(move || watch_peer(&probe, &gone_w, &stop_w))?;
        Ok(PeerWatch { gone, stop, watcher: Some(watcher) })
    }

    /// The flag the run reads (`StoreEvaluator::with_cancel`, `harness::run::is_cancelled`).
    fn gone(&self) -> &AtomicBool {
        &self.gone
    }

    /// Stop and join the watcher, then put `stream` back into blocking mode: `(gone, restored)`.
    /// `gone` is read AFTER the join, so a close the watcher saw at its last look is not missed.
    fn finish(mut self, stream: &TcpStream, peer: Option<SocketAddr>) -> (bool, bool) {
        self.halt();
        (self.gone.load(Ordering::Relaxed), restore_blocking(stream, peer))
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(watcher) = self.watcher.take() {
            watcher.thread().unpark();
            let _ = watcher.join();
        }
    }
}

/// A run that PANICS unwinds through here: the watcher is still stopped and joined, so it never
/// outlives the connection it watches.
impl Drop for PeerWatch {
    fn drop(&mut self) {
        self.halt();
    }
}

/// The watcher's loop (see [`PeerWatch`]). `probe` is non-blocking, so `WouldBlock` is "alive, said
/// nothing".
fn watch_peer(probe: &TcpStream, gone: &AtomicBool, stop: &AtomicBool) {
    let mut byte = [0u8; 1];
    while !stop.load(Ordering::Relaxed) {
        match probe.peek(&mut byte) {
            Ok(0) => {
                gone.store(true, Ordering::Relaxed);
                return;
            }
            Ok(_) => {}
            Err(e)
                if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(_) => {
                gone.store(true, Ordering::Relaxed);
                return;
            }
        }
        thread::park_timeout(PEER_POLL);
    }
}
