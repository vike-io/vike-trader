//! One connection: the keyed pre-auth handshake, then the read / plane / scope / answer loop, and
//! the peer-gone watch that stops a run whose client has left.

use std::io;
use std::net::{SocketAddr, TcpStream};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use vike_backtesthub_client::msgpack_body::write_reply;
use vike_backtesthub_client::proto::{
    PROTO_VERSION, Request, Response, request_kind, required_scope,
};
use vike_data::HistoryStore;
use vike_hub_protocol::access::{ACCESS_DENIED, Person, recheck};
use vike_hub_protocol::auth::{BACKTESTHUB_DOMAIN, HubKeys, Scope, VerbScope, scope_admits};
use vike_hub_protocol::frame::{
    BodyCodec, MAX_FRAME_LEN, decode_body, read_frame_raw, write_reply_as,
};
use vike_hub_protocol::handshake::{
    HandshakeFrame, HandshakeNote, HandshakeOutcome, HandshakeReply, HandshakeSpec,
    already_authenticated, handshake_frame_of, read_end_words, run_keyed_handshake,
};
use vike_hub_protocol::limits::SERVER_HANDSHAKE_DEADLINE;
#[cfg(doc)]
use vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT;
use vike_hub_protocol::plane::{Plane, foreign_verb, wrong_plane_message};

use super::StudyDirs;
use super::dispatch::{handle_request, served_features};
use crate::named_run::NamedRunLane;

/// A strategy-file save's refusal on a daemon with no hub keys (the connection loop's `None` arm).
const SAVE_NEEDS_KEYS: &str = "SaveStrategyFile refused: this backtesthub has no hub keys, so it \
     authenticates nothing and a saved strategy file would be open to any local process. Configure \
     its hub keys (the `hub_key` rows `vike-cli backend mint-missing` files) and save over a \
     connection that authenticated with the Control key.";

/// The answer to an `Auth` on a daemon with no hub keys: nothing verified the client's mac, and it is told so.
const AUTH_NEEDS_KEYS: &str = "this backtest server has no hub keys configured and authenticates \
     nothing; connect without Auth";

/// A Studio Build's refusal on a daemon with no hub keys (the connection loop's `None` arm).
const BUILD_NEEDS_KEYS: &str = "BuildStrategyFile refused: this backtesthub has no hub keys, so it \
     authenticates nothing and would hand any local process's source to the strategy builder. Configure \
     its hub keys (the `hub_key` rows `vike-cli backend mint-missing` files) and build over a \
     connection that authenticated with the Control key.";

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server: the steps — and the refusal
/// ordering — are `vike_hub_protocol::handshake`'s, the one spelling the datahub shares: a first
/// frame that is not `Hello` is an unauthenticated VERB and the socket is dropped; a scope this
/// server holds no key for is refused WITHOUT consulting the key; the mac (or a device's signature) is
/// verified under this daemon's OWN separator, `BACKTESTHUB_DOMAIN`, so a login signed for the datahub
/// or the tradehub never verifies here (decision 0288's amendment).
/// The whole phase runs under ONE [`SERVER_HANDSHAKE_DEADLINE`], so a peer trickling a byte inside
/// every read timeout is cut off at it too. Each step is logged here, under this module's target, in
/// the words it always had.
///
/// Beside the outcome it returns WHY a refused handshake was refused, in words, for the one
/// `connection closed (handshake refused)` line — the datahub's `run_handshake` does the same. A peer
/// that only hangs up writes no step line, which left the CI box's refusals of 2026-11-16 with no reason in
/// the log at all.
fn run_handshake(
    stream: &mut TcpStream,
    keys: &HubKeys,
    features: &[String],
    peer: Option<SocketAddr>,
) -> (HandshakeOutcome, String) {
    let mut why =
        String::from("an undecodable handshake frame, or a reply that could not be written");
    let spec = HandshakeSpec {
        domain: BACKTESTHUB_DOMAIN,
        proto_version: PROTO_VERSION,
        plane: Some(Plane::Backtest),
        features,
        deadline: SERVER_HANDSHAKE_DEADLINE,
        reply_ceiling: MAX_FRAME_LEN,
    };
    let outcome = run_keyed_handshake(
        stream,
        keys,
        spec,
        // As the datahub: every scope the keys hold except Account, from anyone.
        |scope| if scope == Scope::Account { Err("bad mac") } else { Ok(()) },
        // The registration door is the tradehub's alone.
        None,
        request_kind,
        |note| match note {
            HandshakeNote::VerbRefused(verb) => {
                why = format!("the verb {verb} before authenticating");
                tracing::info!(
                    ?peer,
                    verb,
                    "backtesthub: verb refused — connection is not authenticated"
                )
            }
            HandshakeNote::VersionDiffers(client_version) => tracing::debug!(
                ?peer,
                client_version,
                server_version = PROTO_VERSION,
                "backtesthub: client protocol version differs; auth will fail on the signed \
             version"
            ),
            HandshakeNote::NoKey(scope) | HandshakeNote::Refused(scope, _) => {
                why = format!("no key for {scope:?} on this server");
                tracing::info!(
                    ?peer,
                    ?scope,
                    "backtesthub: auth refused — no key configured for this scope on this server"
                )
            }
            HandshakeNote::PersonDenied(scope, refusal) => {
                why = format!(
                    "the access list refused the device at {scope:?}: {}",
                    refusal.as_str()
                );
                tracing::info!(
                    ?peer,
                    ?scope,
                    why = refusal.as_str(),
                    "backtesthub: a device's auth refused (access denied)"
                )
            }
            HandshakeNote::ReadEnded(kind) => why = read_end_words(kind),
            HandshakeNote::Authenticated(scope) => {
                tracing::info!(?peer, ?scope, "backtesthub: authenticated")
            }
            HandshakeNote::BadMac(scope) => {
                why = format!("bad mac at {scope:?}");
                tracing::info!(?peer, ?scope, "backtesthub: auth denied (bad mac)")
            }
        },
    );
    (outcome, why)
}

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Framing and decoding are separate, so a well-framed but
/// undecodable request is answered with [`Response::Error`] and the loop CONTINUES — the connection
/// survives a bad request. A read timeout CLOSES the connection and is never recovered into a
/// resumed loop, which would desync the stream.
///
/// `idle_read_timeout` is the read timeout of every frame after the (keyed) handshake —
/// [`SERVER_IDLE_READ_TIMEOUT`] unless the box wrote `config.node_idle_read_timeout_ms`; the
/// handshake itself stays under [`SERVER_HANDSHAKE_DEADLINE`].
pub(super) fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    keys: Option<&HubKeys>,
    named_run: NamedRunLane,
    idle_read_timeout: Duration,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "backtesthub: connection opened");
    // Nagle OFF before the first frame. Why, and why a refusal is only logged:
    // `vike_hub_protocol::frame::configure_hub_stream`.
    if let Err(e) = vike_hub_protocol::frame::configure_hub_stream(&stream) {
        tracing::warn!(?peer, error = %e, "backtesthub: TCP_NODELAY refused; frames may wait for an ACK");
    }

    let features = served_features(keys.is_some());

    // `person`: who, when a device key signed (decision 0288); its row is re-read before every write.
    let (authed, person): (Option<Scope>, Option<Person>) = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(SERVER_HANDSHAKE_DEADLINE)) {
                tracing::warn!(?peer, error = %e, "backtesthub: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer) {
                (HandshakeOutcome::Authed { scope, person, .. }, _) => (Some(scope), person),
                (HandshakeOutcome::Closed, why) => {
                    tracing::info!(
                        ?peer,
                        why = why.as_str(),
                        "backtesthub: connection closed (handshake refused)"
                    );
                    return;
                }
            }
        }
        None => (None, None),
    };

    if let Err(e) = stream.set_read_timeout(Some(idle_read_timeout)) {
        tracing::warn!(?peer, error = %e, "backtesthub: could not set read timeout, closing connection");
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
                        tracing::info!(?peer, "backtesthub: idle read timeout, closing connection");
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "backtesthub: read fault, closing connection");
                    }
                }
                break;
            }
        };

        // `keep_open` is false only after a watched verb: its client is gone, or the socket could
        // not be put back the way the loop reads it (`answer`'s doc). `retention` is the saved-run
        // retention an answered request leaves for after its answer is written ([`write_answer`]). The
        // request's codec is its reply's (`vike_hub_protocol::frame::BodyCodec`): a client that never
        // asked for msgpack is answered JSON.
        let codec = BodyCodec::of(&body);
        // A HANDSHAKE frame (`vike_hub_protocol::handshake`'s; no verb of this enum) after the handshake: an
        // authenticated connection refuses it by name and is kept. A KEY-LESS daemon (a test's: production is
        // always keyed) runs no handshake, so it answers `Hello` with its `Welcome`, which carries no nonce and no
        // `auth`, and an answer to a challenge it never made with the refusal that says so. JSON only
        // (`handshake_frame_of`): a verb's msgpack or LZ4 body is decoded once, below.
        if let Some(frame) = handshake_frame_of(&body) {
            let written = match (authed, frame) {
                (Some(scope), frame) => {
                    let kind = frame.kind();
                    tracing::info!(
                        ?peer,
                        ?scope,
                        verb = kind,
                        "backtesthub: a {kind} refused — this connection is already authenticated and its \
                         scope is never re-negotiated"
                    );
                    write_reply(&mut stream, codec, &Response::Error(already_authenticated(&frame)))
                }
                (None, HandshakeFrame::Hello { .. }) => write_reply_as(
                    &mut stream,
                    codec,
                    &HandshakeReply::Welcome {
                        proto_version: PROTO_VERSION,
                        features: features.clone(),
                        nonce: None,
                    },
                ),
                (None, _) => write_reply_as(
                    &mut stream,
                    codec,
                    &HandshakeReply::AuthDenied { reason: AUTH_NEEDS_KEYS.to_string() },
                ),
            };
            if let Err(e) = written {
                tracing::warn!(?peer, error = %e, "backtesthub: write fault, closing connection");
                break;
            }
            continue;
        }
        let (response, keep_open, retention) = match decode_body::<Request>(&body) {
            Ok(request) => match authed {
                Some(scope) => {
                    let needed = required_scope(&request);
                    let admitted = scope_admits(scope, needed);
                    // An admitted WRITE re-reads the device's row (decision 0288 item 4); a read never does.
                    let revoked = admitted
                        && needed == VerbScope::Write
                        && recheck(keys.and_then(HubKeys::access), person.as_ref(), Scope::Write)
                            .inspect_err(|why| {
                                tracing::warn!(
                                    ?peer, person = ?person, why = why.as_str(),
                                    verb = request_kind(&request),
                                    "backtesthub: a write REFUSED — the device's access was re-read"
                                );
                            })
                            .is_err();
                    if revoked {
                        (Response::Error(ACCESS_DENIED.to_string()), true, None)
                    } else if admitted {
                        answer_caught(request, &stream, peer, &store, study, named_run)
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "backtesthub: verb refused — outside this connection's scope"
                        );
                        // ⚠ This said "Every Run* verb is Control-only" until 2026-09-16 and
                        // it is now one verb too wide in the direction that MATTERS to the
                        // reader: `RunNamed` is `VerbScope::Read`
                        // (`docs/decisions/0064-a-named-run-carries-no-source.md`), so an
                        // Observe peer told "every Run* verb is Control" would conclude it
                        // needs a Control key when the verb it wants is already reachable.
                        let refusal = Response::Error(format!(
                            "{kind} requires the Control scope; this connection authenticated \
                             as Observe. The Run* verbs that carry SOURCE — a profile's \
                             `[strategy.params].src`, a StrategySpecWire — are Control-only because \
                             they COMPILE CLIENT-SUPPLIED RHAI on this server, and RunStudy is \
                             Control because it WRITES a run directory. The Observe half of \
                             this daemon is Ping, ListStrategies, NamedStrategies and RunNamed \
                             — the last of which runs one strategy this server already holds, \
                             over one bounded window, and is what an Observe client wants"
                        ));
                        (refusal, true, None)
                    }
                }
                // ⚠ A daemon with NO hub keys authenticates nothing, so every write would be open to
                // any local process: a strategy FILE (code a live mount may run at its next start) is
                // written only over a connection that proved the Control key.
                None if matches!(request, Request::SaveStrategyFile { .. }) => {
                    tracing::info!(?peer, "backtesthub: strategy file save refused — no hub keys");
                    (Response::Error(SAVE_NEEDS_KEYS.to_string()), true, None)
                }
                // ...and a Build, whose builder no person's key reaches: forwarding one from a
                // connection that proved nothing would open the compiler to any local process.
                None if matches!(request, Request::BuildStrategyFile { .. }) => {
                    tracing::info!(?peer, "backtesthub: strategy file build refused — no hub keys");
                    (Response::Error(BUILD_NEEDS_KEYS.to_string()), true, None)
                }
                None => answer_caught(request, &stream, peer, &store, study, named_run),
            },
            // ⚠ A DATA VERB DOES NOT DECODE HERE, and the operator who dialled the wrong port is still
            // owed the address (`vike_hub_protocol::plane`'s module doc). Asked on this arm, it also comes
            // before the scope check, the mirror image of `vike_datahub::server`'s connection loop and
            // for the same reason: a verb this daemon does not serve at all is not a scope question, and
            // answering "that requires Control" about a `LoadBars` would send the reader looking for a
            // key when what they need is a different address.
            Err(e) => match foreign_verb(&body, Plane::Backtest) {
                Some(kind) => {
                    tracing::info!(
                        ?peer,
                        verb = kind,
                        "backtesthub: verb refused — it belongs to the data daemon"
                    );
                    let refusal = wrong_plane_message(kind, Plane::Backtest, Plane::Data);
                    (Response::Error(refusal), true, None)
                }
                None => {
                    (Response::Error(format!("unrecognized/undecodable request: {e}")), true, None)
                }
            },
        };

        if let Err(e) = write_answer(&mut stream, codec, &response, retention, &study.runs_root) {
            if keep_open {
                tracing::warn!(?peer, error = %e, "backtesthub: write fault, closing connection");
            }
            break;
        }
        if !keep_open {
            break;
        }
    }
    tracing::info!(?peer, "backtesthub: connection closed");
}

/// The verbs that run under a [`PeerWatch`]: the three profile-shaped RUNS, each of which threads
/// the peer-gone flag into its harness path and stops at its next point, series load or window
/// (`super::verbs`'s `run_backtest`, `run_paramscan_profile`, `run_walkforward_profile`).
///
/// Deliberately NOT watched, and why — a watch on a verb that cannot read the flag would only add a
/// thread and turn a finished answer into a cancellation:
///
/// * `RunSlice`, `RunParamscan`, `RunWalkforward` and `RunStudy` run `vike-studio-core`'s runners
///   (`run_slice_local`, `run_paramscan_local`, `run_walkforward_local`, `run_study_local`), whose
///   signatures carry no stop flag. Giving them one is a change to that crate's runners, not
///   something this file can do alone. No MCP tool dials them.
/// * `RunNamed` is BOUNDED by construction (`vike_backtesthub_client::named_run::NAMED_RUN_MAX_BARS`
///   bars, one strategy, a slot table), so it ends in seconds whoever is waiting.
/// * Everything else answers from memory or refuses.
///
/// A `matches!` and not an exhaustive match on purpose: a NEW verb is unwatched until someone
/// threads the flag through it, which is the safe default.
///
/// ⚠ **Every request whose strategy is a `.rs` FILE this daemon hands a sandboxed child is watched,
/// whatever its verb** (`crate::strategy_sandbox`): a gone client KILLS that child, so the flag needs no
/// runner to read it.
fn is_watched(request: &Request, study: &StudyDirs) -> bool {
    // A run asked fresh is watched as the run it wraps.
    if let Request::Fresh(inner) = request {
        return is_watched(inner, study);
    }
    // A summarized run is watched as the run it wraps: `vike-cli backtest run`'s default
    // `Summarized(RunBacktest)` stops with its client as the plain verb does.
    let wrapped = match request {
        Request::Summarized(inner) => &**inner,
        request => request,
    };
    matches!(
        wrapped,
        Request::RunBacktest(_)
            | Request::RunParamscanProfile { .. }
            | Request::RunWalkforwardProfile { .. }
    ) || crate::strategy_sandbox::names_rust_file(request, &study.strategies)
}

/// Answer one ADMITTED request: `(response, keep_open, retention)`. A verb [`is_watched`] rejects is
/// [`handle_request`] and keeps the connection open; a watched one runs under a [`PeerWatch`], so a
/// client that closes its socket mid-run stops the run instead of leaving it to burn the box's
/// cores for nobody.
///
/// `keep_open` is false when the client is GONE (the cancellation answer is still written, for a
/// client that only half-closed, and then the connection ends without another read) or when the
/// socket could not be put back into blocking mode — the loop's reads would then fail with
/// `WouldBlock` and read as an idle timeout, so ending the connection on purpose is the honest
/// form of the same outcome. `retention` is [`handle_request`]'s, for [`write_answer`].
fn answer<'a>(
    request: Request,
    stream: &TcpStream,
    peer: Option<SocketAddr>,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &'a StudyDirs,
    named_run: NamedRunLane,
) -> (Response, bool, Option<&'a vike_runs::RunRetention>) {
    if !is_watched(&request, study) {
        let (response, retention) = handle_request(request, store, study, named_run, None);
        return (response, true, retention);
    }
    let verb = request_kind(&request);
    let watch = match PeerWatch::arm(stream) {
        Ok(w) => w,
        Err(e) => {
            // The run still goes ahead, unwatched — exactly what it did before the watch existed.
            tracing::warn!(?peer, verb, error = %e, "backtesthub: could not watch the client; this run goes to completion even if it leaves");
            let (response, retention) = handle_request(request, store, study, named_run, None);
            return (response, restore_blocking(stream, peer), retention);
        }
    };
    let (response, retention) =
        handle_request(request, store, study, named_run, Some(watch.gone()));
    let (gone, restored) = watch.finish(stream, peer);
    if gone {
        tracing::info!(?peer, verb, "backtesthub: client gone mid-run; run cancelled");
    }
    (response, restored && !gone, retention)
}

/// Write `response`, THEN prune the runs root by `retention` (what [`handle_request`] leaves after a request that may
/// have saved; `super::saved_run::prune`): the prune reads every saved run's manifest, so the answer goes out first.
/// The connection reads its next frame only after the prune, so its next request sees the pruned root. A write that
/// fails still prunes, as the prune before the write did; the error is the write's. The answer is in `codec`, the
/// request's own (`vike_backtesthub_client::msgpack_body::write_reply`, a whole report as its tree under LZ4).
fn write_answer(
    stream: &mut impl io::Write,
    codec: BodyCodec,
    response: &Response,
    retention: Option<&vike_runs::RunRetention>,
    runs_root: &Path,
) -> io::Result<()> {
    let written = write_reply(stream, codec, response);
    if let Some(retention) = retention {
        super::saved_run::prune(retention, runs_root);
    }
    written
}

/// [`answer`], with a request that PANICS answered in words: before this, the panic unwound the
/// connection thread, the client read a closed socket and no reason, and the daemon's only trace was
/// the panic line. The connection stays open (the panic is this request's, not the stream's), and the
/// socket is put back into blocking mode, which an unwinding [`PeerWatch`] does not do. A panicked request
/// saved no run, so it leaves no retention to apply after its answer.
fn answer_caught<'a>(
    request: Request,
    stream: &TcpStream,
    peer: Option<SocketAddr>,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &'a StudyDirs,
    named_run: NamedRunLane,
) -> (Response, bool, Option<&'a vike_runs::RunRetention>) {
    let verb = request_kind(&request);
    match catch_unwind(AssertUnwindSafe(|| answer(request, stream, peer, store, study, named_run)))
    {
        Ok(answered) => answered,
        Err(payload) => {
            let text: String =
                vike_utility::panic::payload_text(&*payload).chars().take(400).collect();
            tracing::error!(?peer, verb, panic = text.as_str(), "backtesthub: a request panicked");
            let response = Response::Error(format!(
                "{verb} failed inside the backtesthub, which is a defect of the server, not of the \
                 request: it panicked with \"{text}\" and produced no result. The connection is \
                 still open"
            ));
            (response, restore_blocking(stream, peer), None)
        }
    }
}

/// Put `stream` back into the blocking mode the connection loop reads in; `false` (logged) if the
/// socket refused.
fn restore_blocking(stream: &TcpStream, peer: Option<SocketAddr>) -> bool {
    match stream.set_nonblocking(false) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(?peer, error = %e, "backtesthub: could not restore blocking mode, closing connection");
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
/// its run goes to completion (`crates/vike-backtesthub/tests/backtesthub_cancel.rs`'s
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
/// `vike_hub_protocol::frame::MAX_FRAME_LEN` long? — in front of every read after a watched verb, to
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

    /// The flag the run reads (`SearchEvaluatorStore::with_cancel`, `run::is_cancelled`).
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

/// The refusal line's reason, through a real socket: a key-less dial that leaves after the `Welcome`
/// (the shape of every refusal the CI box logged without a reason on 2026-11-16) is named as a hang-up.
#[cfg(test)]
mod refusal_reason_tests {
    use super::*;
    use std::net::TcpListener;
    use vike_hub_protocol::frame::write_frame;

    #[test]
    fn a_dial_that_hangs_up_after_the_welcome_is_refused_with_its_reason() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = thread::spawn(move || {
            let mut s = TcpStream::connect(addr).expect("dial");
            write_frame(&mut s, &HandshakeFrame::Hello { proto_version: PROTO_VERSION })
                .expect("hello");
            // The Welcome is read, then the dial leaves with no key to answer it.
            let _ = read_frame_raw(&mut s);
        });
        let (mut stream, peer) = listener.accept().expect("accept");
        stream.set_read_timeout(Some(SERVER_HANDSHAKE_DEADLINE)).expect("deadline");
        let keys = HubKeys::new(b"observe".to_vec(), b"control".to_vec());
        let (outcome, why) = run_handshake(&mut stream, &keys, &served_features(true), Some(peer));
        client.join().expect("client");
        assert!(matches!(outcome, HandshakeOutcome::Closed), "refused");
        assert!(why.contains("hung up before authenticating"), "{why}");
    }
}

/// The saved-run retention runs AFTER the answer is written ([`write_answer`]), and saving and pruning leave the
/// answer's bytes those of a process that saves nothing.
#[cfg(test)]
mod answer_before_prune_tests {
    use super::*;
    use vike_backtesthub_client::wire_studio::StrategySpecWire;
    use vike_data::HistoryStoreMemory;
    use vike_hub_protocol::frame::write_frame;

    /// A socket that counts the saved runs under `runs_root` when the answer's first byte reaches it.
    struct Recorder<'a> {
        runs_root: &'a Path,
        runs_when_written: Option<usize>,
        bytes: Vec<u8>,
    }

    impl io::Write for Recorder<'_> {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let root = self.runs_root;
            self.runs_when_written.get_or_insert_with(|| vike_runs::scan_runs(root).runs.len());
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Three saved runs of one Studio editor buffer under a retention of 2: each answer reaches the socket while
    /// every saved run is still there, each is the very bytes the unsaved run answers, and the prune comes after.
    #[test]
    fn a_saved_run_is_answered_before_the_retention_prunes_and_in_an_unsaved_runs_bytes() {
        let store = HistoryStoreMemory::new();
        let bars =
            vike_studio_core::test_support::walk::random_walk_bars(400, 60_000, Some("BTCUSDT"));
        store.append_bars("binance", "BTCUSDT", "1m", &bars, None).expect("seed");
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
        let root = tempfile::tempdir().expect("user data");
        let runs = root.path().join("runs");
        let request = || Request::RunSlice {
            spec: StrategySpecWire::Rhai(
                vike_studio_core::test_support::cross_script::LONG_CROSS_SCRIPT.to_string(),
            ),
            slice: Box::new(vike_backtesthub_client::test_support::btcusdt_1m_bars()),
            params: None,
        };
        let saving = StudyDirs {
            runs_root: runs.clone(),
            save_backtest_runs: true,
            run_retention: Some(vike_runs::RunRetention {
                keep_per_strategy: 2,
                marks_root: root.path().join("marks"),
                referencing_documents: Vec::new(),
            }),
            ..StudyDirs::working_directory()
        };
        let plain = StudyDirs::working_directory();
        let (unsaved, none) =
            handle_request(request(), &store, &plain, NamedRunLane::DISARMED, None);
        assert!(none.is_none(), "a process that saves nothing prunes nothing");
        assert!(matches!(unsaved, Response::RunResult(_)), "the run ran: {unsaved:?}");
        let mut expected = Vec::new();
        write_frame(&mut expected, &unsaved).expect("frame");
        let count = || vike_runs::scan_runs(&runs).runs.len();
        for saved in 1..=3 {
            let (response, retention) =
                handle_request(request(), &store, &saving, NamedRunLane::DISARMED, None);
            assert!(retention.is_some(), "a saved run leaves the retention for after its answer");
            assert_eq!(count(), saved, "nothing is pruned before the answer is written");
            let mut socket =
                Recorder { runs_root: &runs, runs_when_written: None, bytes: Vec::new() };
            write_answer(&mut socket, BodyCodec::Json, &response, retention, &runs)
                .expect("written");
            assert_eq!(socket.runs_when_written, Some(saved), "the answer went out first");
            assert_eq!(socket.bytes, expected, "the bytes of the run that saved nothing");
            assert_eq!(count(), saved.min(2), "then the retention pruned");
        }
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;
