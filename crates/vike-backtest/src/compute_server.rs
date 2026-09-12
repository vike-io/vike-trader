//! The COMPUTE daemon: `vike-backend backtest --addr`, the blocking thread-per-connection server
//! for the seven verbs that RUN something.
//!
//! # What this is, and why it is in THIS crate
//!
//! Ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split one
//! served surface into two daemons. `RunBacktest`, `RunSlice`, `RunSweep`, `RunWalkforward`,
//! `RunSweepProfile`, `RunWalkforwardProfile` and `ListStrategies` left `vike-datahub` and are
//! served here; the store verbs stayed there. The owner's argument was responsibility —
//! *"за бэктест и форвард-тест отвечает крейт бэктеста"*, the BACKTEST CRATE is responsible — and
//! this file is that sentence in code: the socket that answers a backtest is opened by the crate
//! that runs one.
//!
//! **No engine moved.** `vike-datahub` never held one; it FORWARDED to `vike_backtest::harness`.
//! What moved is which socket answers, which is why this file is a dispatcher and a handshake and
//! nothing else — the three profile-shaped verbs below are the same `harness::run_backtest` /
//! `run_sweep` / `run_walkforward` calls, character for character, that the data server used to
//! make.
//!
//! ⚠ **The layer rule is what settled the location, and it also created the one seam here.** The
//! four profile-shaped/roster verbs are pure `vike_backtest::harness` and are served directly. The
//! three STUDIO verbs (`RunSlice`/`RunSweep`/`RunWalkforward`) run `vike_studio_core`'s slice
//! runners — and `vike-studio-core` is layer 55 while this crate is layer 50, so naming it here is
//! a dependency-direction violation the layer gate would refuse. It is not an accident of numbering
//! either: `vike-studio-core` DEPENDS on this crate, so the edge could never point the other way.
//!
//! The answer is the seam this protocol already uses for exactly this shape —
//! `vike_datahub::backfill`'s `BackfillTable`: a dispatch table whose TYPE names only the wire DTOs
//! ([`StudioRunTable`], `Box<dyn Fn>` over `Wire*`), so this file compiles on every build and
//! references no studio type, while the constructor that fills it with real runners lives UP the
//! graph in `vike_studio_core::wire_run`'s `studio_run_table` and is mounted by the composition
//! root. A daemon with no table mounted answers those three with a clean, named error and
//! advertises none of them — the same three-legged capability negotiation `FEATURE_BACKFILL` uses
//! (advertise only what is mounted, refuse what is sent anyway, and let the client check the
//! advertisement first).
//!
//! # Transport, handshake and posture — the SAME protocol the data daemon speaks
//!
//! Length-prefixed `serde_json` frames over blocking [`std::net`], thread-per-connection, the
//! `Hello` → `Welcome{nonce}` → `Auth` → `AuthOk` handshake, `vike_datahub_client::proto`'s
//! [`Scope`] rules, and `vike_datahub_client::bind`'s bind guard. One schema, one client library,
//! two served surfaces — so `vike_datahub_client::DatahubClient` dials this daemon unchanged and
//! only the ADDRESS differs.
//!
//! ⚠ **What is SHARED and what is MIRRORED, stated because the difference is the whole of the
//! workspace's "two sides must not disagree" rule.** Every CLASSIFICATION lives in
//! `vike-datahub-client`, the crate below both daemons: which plane a verb belongs to
//! (`plane_of`), which scope it needs (`required_scope`/`scope_admits`), what its name is
//! (`request_kind`), what a wrong-plane refusal says (`wrong_plane_message`), and whether an
//! address may be bound (`bind_decision`). Those are the facts a second copy would eventually
//! contradict, and there is exactly one copy of each.
//!
//! The CONNECTION MECHANICS below — the read timeouts, the pre-auth frame cap, the handshake
//! transcript — are a MIRROR of `vike_datahub::server`'s rather than a shared function, which is the
//! same relationship `vike_tradehub::server` has had with that file since the node protocol existed.
//! The reason is cost: the shared home would be `vike-datahub-client`, whose whole identity is that
//! the GUI links it without weight, and hoisting the transcript there would put `tracing` (the
//! per-connection log line) into it for two callers. The transcript is proven per-daemon by its own
//! roundtrip test against its own socket, so a divergence is a red test rather than a silent
//! disagreement about a verb. Keep them in step by hand; if a third daemon appears, hoist it.
//!
//! ⚠ **The NONCE is the exception, and it was not a choice.** `fresh_nonce` DID move down to
//! `vike_datahub_client::node_auth` — because `crates/vike-ops/tests/clock_pin.rs`'s
//! `no_scoped_crate_declares_an_rng_dependency` forbids THIS crate from naming an RNG at all: it is
//! one of the two sides `tests/r7_gate.rs` compares bit for bit, and randomness in the fold breaks
//! replay exactly as a wall clock does. The gate is right, and what it forced is better than what it
//! refused: one generator, two servers.
//!
//! ⚠ **This daemon COMPILES CLIENT-SUPPLIED RHAI, and that is now ITS posture rather than the data
//! server's.** Every `Run*` verb reaches a Rhai compiler — `RunBacktest`/`RunSweepProfile`/
//! `RunWalkforwardProfile` through a profile's `[strategy.params].src` and `harness::registry`'s
//! `"rhai"` arm, the Studio three through `to_strategy_spec`'s `WireSpec::Rhai` arm — so every one
//! of them is `Scope::Control`, and a key-less non-loopback bind is REFUSED
//! (`vike_datahub_client::bind`). The user-INDICATOR install the datahub used to perform came with
//! them too — but it landed in `crates/vike-backtest/src/backtest_cli.rs`'s `run`, which already did
//! it for the one-shot path; the note above `run_backtest` below carries why, and the ⚠⚠ hazard it
//! inherits.
//!
//! Logging is at CONNECTION boundaries only (open/close/fault, plus the auth verdict), never per
//! frame.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::HistStore;
use vike_datahub_client::node_auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope, fresh_nonce};
use vike_datahub_client::proto::{
    FEATURE_AUTH, PROTO_VERSION, Plane, Request, Response, VerbScope, plane_of, read_frame_raw,
    read_frame_raw_capped, request_kind, required_scope, scope_admits, write_frame,
    wrong_plane_message,
};
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireRunError, WireRunResult, WireSlice, WireSpec, WireSweep, WireSweepResult,
    WireWalkforward, WireWalkforwardResult,
};

use crate::harness::{self, BacktestProfile, BacktestReport};

/// The store handle this server serves over — the `vike-data` trait seam, exactly as the data
/// daemon holds it, so the same [`serve`] runs over `DataFusionHist` in production and over the
/// in-memory `MemHistStore` double in tests.
pub type StoreHandle = Arc<dyn HistStore + Send + Sync>;

/// A generous per-connection read timeout so a half-open or idle peer cannot park a connection
/// thread in `read` for the process lifetime. Mirrors `vike_datahub::server`'s `IDLE_READ_TIMEOUT`
/// (see this module's ⚠ on what is shared and what is mirrored), and for the same reason: a real
/// request body is kilobytes and arrives in milliseconds once its length prefix is seen, so a
/// timeout realistically only fires BETWEEN requests, which the loop then closes to free the
/// thread.
///
/// ⚠ It bounds the gap between FRAMES, never the length of a RUN. A sweep can hold this thread for
/// minutes inside one request and no timeout here can fire while it does — the read that would
/// time out has not been issued yet. That is deliberate: a compute daemon whose long answers were
/// clipped by an idle timeout would be useless, and it is why this value is about the CLIENT going
/// quiet rather than about the work taking a while.
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends
/// to a KEYED server. Mirrors `vike_datahub::server`'s `HANDSHAKE_MAX_FRAME_LEN`, byte for byte and
/// for the same reason: the shared `MAX_FRAME_LEN` is 64 MiB because a legitimate ANSWER can be
/// large, and accepting 64 MiB of handshake means anyone who can reach the socket makes this server
/// allocate 64 MiB per connection by sending FOUR BYTES. Post-auth frames keep the full ceiling — a
/// profile TOML or a Rhai source legitimately runs to kilobytes.
///
/// ⚠ It applies ONLY on a KEYED server. A key-less one has no pre-auth phase at all.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How long a KEYED server waits for the whole two-frame handshake before closing the connection.
/// Mirrors `vike_datahub::server`'s `HANDSHAKE_DEADLINE`: an unauthenticated peer has nothing to
/// think about, so a handshake that has not completed in this window is a socket somebody opened
/// and said nothing on. Reset to [`IDLE_READ_TIMEOUT`] once `AuthOk` is written.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

// Compile-time bounds on the two pre-auth limits, the tradehub `confirm.rs` idiom: a RANGE, so a
// deliberate tweak stays free while "the bound was effectively removed" and "the bound refuses
// every legitimate handshake" both fail to compile.
const _: () = assert!(
    HANDSHAKE_MAX_FRAME_LEN > 0 && HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);
const _: () = assert!(
    HANDSHAKE_DEADLINE.as_secs() > 0 && HANDSHAKE_DEADLINE.as_secs() <= 120,
    "HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — it exists to stop an unauthenticated \
     peer parking a connection thread, which a 0 (refuse everything) or a multi-minute value both \
     defeat"
);

/// The `RunSlice` runner, injected. `(spec, slice, params, store) -> the rendered answer`.
///
/// Every parameter and both result types are `Wire*` DTOs from `vike-datahub-client`, which is the
/// property that makes the whole seam work: this crate can name them (layer 30 < 50) while it
/// cannot name `vike_studio_core::DataSlice`/`StrategySpec`/`RunError` (layer 55 > 50). The real
/// implementation is `vike_studio_core::wire_run`'s `run_slice_local` — the SAME entry its parity
/// test drives, so "run locally" and "run over this wire" stay one computation.
pub type StudioSliceFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireRunResult, WireRunError>
        + Send
        + Sync,
>;

/// The `RunSweep` runner, injected — [`StudioSliceFn`] plus the parameter grid.
pub type StudioSweepFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            &WireSweep,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireSweepResult, WireRunError>
        + Send
        + Sync,
>;

/// The `RunWalkforward` runner, injected — [`StudioSliceFn`] plus the split count.
pub type StudioWalkforwardFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            &WireWalkforward,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireWalkforwardResult, WireRunError>
        + Send
        + Sync,
>;

/// The three STUDIO runners, mounted together or not at all.
///
/// Together, because they are one capability from a client's point of view: a Studio pointed at a
/// daemon that could run a slice but not a sweep would have to discover the difference one verb at
/// a time. `vike_studio_core::wire_run`'s `studio_run_table` is the production constructor and the
/// only place the three real runners are named; a test may build one from its own closures, which
/// is how the roundtrip suite drives the verbs without linking the studio tree.
pub struct StudioRunTable {
    slice: StudioSliceFn,
    sweep: StudioSweepFn,
    walkforward: StudioWalkforwardFn,
}

impl StudioRunTable {
    /// A table from three explicit runners — the ONLY constructor, so a mount is always a
    /// deliberate act by a composition root that could name the real ones.
    pub fn new(
        slice: StudioSliceFn,
        sweep: StudioSweepFn,
        walkforward: StudioWalkforwardFn,
    ) -> Self {
        Self { slice, sweep, walkforward }
    }

    /// The `RunSlice` runner.
    pub fn slice(&self) -> &StudioSliceFn {
        &self.slice
    }

    /// The `RunSweep` runner.
    pub fn sweep(&self) -> &StudioSweepFn {
        &self.sweep
    }

    /// The `RunWalkforward` runner.
    pub fn walkforward(&self) -> &StudioWalkforwardFn {
        &self.walkforward
    }
}

/// Accept connections forever, handling each on its own thread over the shared `store`. No STUDIO
/// table and no keys — the three Studio verbs answer a clean refusal and every connection is
/// unauthenticated, which is the ordinary developer configuration behind the loopback bind guard.
pub fn serve(listener: TcpListener, store: StoreHandle) -> io::Result<()> {
    serve_with_studio(listener, store, None)
}

/// [`serve`] with the three STUDIO runners mounted (or not) — see [`StudioRunTable`].
pub fn serve_with_studio(
    listener: TcpListener,
    store: StoreHandle,
    studio: Option<StudioRunTable>,
) -> io::Result<()> {
    serve_authed(listener, store, studio, None)
}

/// [`serve_with_studio`] with OPTIONAL `NodeKeys` authentication — the full entry, and the one the
/// `backtest --addr` daemon arm calls.
///
/// `keys`:
/// - **`None`** — key-LESS. No handshake is required, `Hello` is optional and informs rather than
///   gates, and `Welcome` carries no nonce and does not advertise [`FEATURE_AUTH`]. The loopback
///   bind guard is then the entire barrier — which for THIS daemon is the barrier in front of a
///   Rhai compiler, so `vike_datahub_client::bind`'s `bind_decision` refuses a key-less
///   non-loopback bind outright, opt-in or no opt-in.
/// - **`Some(keys)`** — every connection MUST complete `Hello` → `Welcome{nonce}` → `Auth` →
///   `AuthOk` before any verb is answered, and each verb is then checked against
///   `vike_datahub_client::proto`'s `required_scope`.
///
/// ⚠ **The keys are the SAME `VIKE_DATAHUB_OBSERVE_KEY`/`VIKE_DATAHUB_CONTROL_KEY` pair the data
/// daemon uses, under the SAME `DATAHUB_DOMAIN` separator, and that is a decision rather than an
/// oversight.** The alternative — a second key pair and a second domain — would have made ruling
/// 7's split, which is an internal reorganisation of OUR served surface, a credential migration for
/// every keyed deployment: an operator would have to mint two more keys to get back what one pair
/// gave them the day before. The two daemons run on one box, over one tunnel, against one store,
/// for one operator; the trust boundary the domain separator exists to draw is between the DATA
/// protocol and the TRADEHUB node protocol, and that separation is unchanged. The residual, stated
/// so it is not discovered: a peer holding the control key can reach both the store write and this
/// daemon's Rhai compiler, exactly as it could before the split.
pub fn serve_authed(
    listener: TcpListener,
    store: StoreHandle,
    studio: Option<StudioRunTable>,
    keys: Option<NodeKeys>,
) -> io::Result<()> {
    let studio = studio.map(Arc::new);
    let keys = keys.map(Arc::new);
    match keys.as_deref() {
        Some(k) => tracing::info!(
            keys = ?k,
            "vike-backtest serve: AUTHENTICATION REQUIRED — every connection must complete the \
             NodeKeys handshake before any verb is answered (the `Debug` above reports key \
             PRESENCE only)"
        ),
        None => tracing::info!(
            "vike-backtest serve: no datahub node keys configured — serving UNAUTHENTICATED (the \
             loopback bind guard is the whole barrier, and what it guards here is a Rhai \
             compiler). Set VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY in the credential \
             store to require authentication"
        ),
    }
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let store = Arc::clone(&store);
                let studio = studio.clone();
                let keys = keys.clone();
                thread::spawn(move || {
                    handle_connection(stream, store, studio.as_deref(), keys.as_deref())
                });
            }
            Err(e) => {
                // A failed accept is per-connection; keep serving.
                tracing::warn!(error = %e, "vike-backtest serve: accept failed, continuing");
            }
        }
    }
    Ok(())
}

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

    if node_auth::verify(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope, &mac) {
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
fn handle_connection(
    mut stream: TcpStream,
    store: StoreHandle,
    studio: Option<&StudioRunTable>,
    keys: Option<&NodeKeys>,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-backtest serve: connection opened");

    let features = served_features(studio.is_some(), keys.is_some());

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
                        handle_request(request, &store, studio)
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
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. Every Run* verb is Control-only because they COMPILE \
                                 CLIENT-SUPPLIED RHAI on this server; ListStrategies and Ping are \
                                 the Observe half of this daemon"
                            ),
                        })
                    }
                }
                None => handle_request(request, &store, studio),
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

/// The refusal a DATA verb gets here — the exact mirror of `vike_datahub::server`'s
/// `compute_verb_moved`, through the same one-spelling helper with the planes swapped.
fn data_verb_elsewhere(verb: &'static str) -> Response {
    Response::Error(wrong_plane_message(verb, Plane::Compute, Plane::Data))
}

/// The refusal the three STUDIO verbs get when no [`StudioRunTable`] is mounted. Names the verb and
/// the reason — a build fact, not a request fact — the way `vike_datahub::server`'s `backfill_verb`
/// names its missing feature rather than answering with an empty result.
fn studio_verb_unmounted(verb: &'static str) -> Response {
    Response::Error(format!(
        "{verb} is served only by a build that mounts the Studio runners, and this one did not. \
         The shipped `vike-backend backtest --addr` mounts them; a bare `cargo run -p vike-backtest \
         --bin backtest` cannot, because those runners live in `vike-studio-core`, which sits ABOVE \
         this crate in the layer graph and can only be handed in by a composition root. The \
         profile-shaped verbs (RunBacktest, RunSweepProfile, RunWalkforwardProfile) and \
         ListStrategies are served on every build"
    ))
}

/// Map one request to its response. Pure dispatch: the profile-shaped runs go straight to
/// `crate::harness`, the Studio three through the injected [`StudioRunTable`], and every DATA verb
/// is answered by [`data_verb_elsewhere`].
///
/// ⚠ The DATA arms are written out one by one rather than as an `other =>` catch-all, for the
/// reason `plane_of`'s own match spells out: a catch-all would silently swallow the NEXT verb
/// somebody adds and tell its author it belongs to the data daemon.
fn handle_request(
    request: Request,
    store: &StoreHandle,
    studio: Option<&StudioRunTable>,
) -> Response {
    match request {
        // The OPTIONAL version handshake: answer with THIS server's `PROTO_VERSION` and the verbs
        // it serves. Only reachable on a KEY-LESS server — a keyed one answers `Hello` inside
        // `run_handshake` and never returns here, which is why `requires_auth` is `false`.
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-backtest serve: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                features: served_features(studio.is_some(), false),
                nonce: None,
            }
        }
        // Only reachable on a KEY-LESS server. There is nothing to verify it against, so say so
        // rather than pretending: a client that signed a mac deserves to learn its key was never
        // checked, not to be told "ok".
        Request::Auth { .. } => Response::AuthDenied {
            reason: "this backtest server has no node keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
        Request::Ping => Response::Pong,

        // The compute-to-data run: the profile crosses the wire, the history never does.
        Request::RunBacktest(profile_toml) => run_backtest(&profile_toml, store),
        Request::RunSweepProfile { profile_toml, rank_by } => {
            run_sweep_profile(&profile_toml, rank_by.as_deref(), store)
        }
        Request::RunWalkforwardProfile { profile_toml } => {
            run_walkforward_profile(&profile_toml, store)
        }
        // The strategy-roster verb. Store-independent (the roster is a compile-time `&[&str]`
        // const) and, since ruling 7, served beside the `--list` flag of this very binary, which reads
        // the same constant — one constant, two transports, one crate.
        Request::ListStrategies => {
            Response::Strategies(harness::STRATEGIES.iter().map(|s| s.to_string()).collect())
        }

        // The STUDIO three, through the injected table. `slice` is boxed in each variant (enum-size
        // hygiene); unbox it for the runner.
        Request::RunSlice { spec, slice, params } => match studio {
            Some(table) => match (table.slice())(&spec, &slice, params.as_ref(), Arc::clone(store))
            {
                Ok(result) => Response::RunResult(result),
                Err(e) => Response::Error(e.to_error_string()),
            },
            None => studio_verb_unmounted("RunSlice"),
        },
        Request::RunSweep { spec, slice, sweep, params } => match studio {
            Some(table) => {
                match (table.sweep())(&spec, &slice, &sweep, params.as_ref(), Arc::clone(store)) {
                    Ok(result) => Response::SweepResult(result),
                    Err(e) => Response::Error(e.to_error_string()),
                }
            }
            None => studio_verb_unmounted("RunSweep"),
        },
        Request::RunWalkforward { spec, slice, walkforward, params } => match studio {
            Some(table) => match (table.walkforward())(
                &spec,
                &slice,
                &walkforward,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::WalkforwardResult(result),
                Err(e) => Response::Error(e.to_error_string()),
            },
            None => studio_verb_unmounted("RunWalkforward"),
        },

        // The DATA plane — `vike-backend datahub`'s, and refused here by name. `handle_connection`
        // refuses them one step earlier, before the scope check; these arms are the belt to that
        // braces.
        Request::LoadBars { .. } => data_verb_elsewhere("LoadBars"),
        Request::ScanQuotes { .. } => data_verb_elsewhere("ScanQuotes"),
        Request::ScanTrades { .. } => data_verb_elsewhere("ScanTrades"),
        Request::PropertiesAsOf { .. } => data_verb_elsewhere("PropertiesAsOf"),
        Request::ListSeries => data_verb_elsewhere("ListSeries"),
        Request::Inventory => data_verb_elsewhere("Inventory"),
        Request::SeriesGaps { .. } => data_verb_elsewhere("SeriesGaps"),
        Request::Coverage => data_verb_elsewhere("Coverage"),
        Request::Backfill { .. } => data_verb_elsewhere("Backfill"),
        Request::DeleteSeries { .. } => data_verb_elsewhere("DeleteSeries"),
        // The MARKET-DATA push lane — likewise the data daemon's. ⚠ These two arms are why this
        // file is in the wire's diff at all: `Request` is ONE schema for two daemons, so a verb
        // added for the datahub reddens this exhaustive match until it is classified. A desktop
        // that dialled `config.backtest_addr` by mistake therefore gets a NAMED refusal that says
        // which daemon serves its DOM, and keeps its connection.
        Request::MdSubscribe { .. } => data_verb_elsewhere("MdSubscribe"),
        Request::MdUpdate { .. } => data_verb_elsewhere("MdUpdate"),
    }
}

/// The verbs THIS daemon answers, advertised in the [`Response::Welcome`] handshake — the compute
/// half of the split, and the exact complement of `vike_datahub::server`'s `served_features`.
///
/// `has_studio` is a RUNTIME fact, not a cfg, for the same reason `FEATURE_BACKFILL` is: "this
/// build can name the studio runners" and "this process was handed them" are different questions,
/// and a `serve()` entry with no table mounted must not advertise what it will refuse.
///
/// ⚠ The strings are the ones the data daemon used to advertise, unchanged — `backtest`,
/// `list_strategies`, `run_sweep_profile`, `run_walkforward_profile`, `run_slice`, `run_sweep`,
/// `run_walkforward`. A client's feature check is therefore the SAME string against a different
/// address, which is what makes ruling 7 a change of address for a client rather than a change of
/// protocol.
fn served_features(has_studio: bool, requires_auth: bool) -> Vec<String> {
    let mut features = vec![
        "backtest".to_string(),
        "list_strategies".to_string(),
        "run_sweep_profile".to_string(),
        "run_walkforward_profile".to_string(),
    ];
    if has_studio {
        features.push("run_slice".to_string());
        features.push("run_sweep".to_string());
        features.push("run_walkforward".to_string());
    }
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
    }
    features
}

// ⚠ THE USER-INDICATOR INSTALL IS NOT DUPLICATED HERE, and the first draft of this file did
// duplicate it. `vike_datahub::datahub_cli` had its own `install_user_indicators`, and the obvious
// move was to bring the function along with the verbs that need it — which produced a `pub fn`
// nobody called, beside a LIVE install `crates/vike-backtest/src/backtest_cli.rs`'s `run` already
// performs a few lines above the `--addr` arm, out of the same `VIKE_USER_DATA_DIR` and through the
// same `vike_script::load_and_install_user_indicators`. Two spellings of one act, one of them dead.
//
// The live one wins because it is on the path EVERY invocation of this binary takes — a one-shot
// `--profile` run compiles a Rhai strategy exactly as this daemon does — and `install` is
// once-per-process, so a second call could only be a no-op or a disagreement about which directory
// answered.
//
// ⚠⚠ **THE SERVER'S SET WINS, AND THE CLIENT CANNOT SEE IT** — the hazard the datahub's copy
// carried, restated here because it is now THIS daemon's. A client script calling `my_ind()` used to
// fail server-side with an unknown-function compile error, loud and unambiguous. It now binds to
// whatever `<server project>/user_data/indicators/my_ind.rhai` contains. `vike-cli backtest` ships
// the SCRIPT and never the indicator files, and no hash or version is exchanged, so the same script
// on two servers with different indicator directories returns different numbers with nothing in the
// response saying so. The trade is deliberate — the alternative is that a user's own indicators
// simply do not work on the path they run backtests through — but it is only defensible because a
// divergence is DIAGNOSABLE afterwards: `backtest_cli` reports every rejected file on stderr, and
// shipping the set with the request is the real fix and is a protocol change, not a startup one.

/// Parse the wire profile TOML, run it over `store`, and return the [`BacktestReport`] as JSON text.
///
/// `BacktestProfile::from_toml_str` parses AND validates (bad range, empty slice, cross-venue snap,
/// …) in one step — the exact guard the file path applies — so a malformed profile becomes a clean
/// [`Response::Error`] before the engine is touched. A run failure (missing strategy, data error,
/// resolution build error) or a report-serialize failure likewise becomes `Response::Error`, so the
/// caller always learns the outcome.
fn run_backtest(profile_toml: &str, store: &StoreHandle) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    let result = match harness::run_backtest(&profile, Arc::clone(store)) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    let report = BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    );
    match serde_json::to_string(&report) {
        Ok(json) => Response::Report(json),
        Err(e) => Response::Error(format!("report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML, expand + run its `[sweep]` grid over `store`, and return the RANKED
/// [`harness::SweepReport`] as JSON text (v7).
///
/// One authority for the profile, exactly like [`run_backtest`]: `BacktestProfile::from_toml_str`
/// parses AND validates — the same guard the file path applies — and the run is the EXISTING
/// `harness::run_sweep`, so a remote sweep and a local `backtest --profile … --rank-by …` are the
/// same computation over the same `[engine]` (fee schedule included).
///
/// RANKING HAPPENS HERE, over the real `BacktestReport`s, using the canonical
/// [`harness::RankMetric`] comparator (NaN-last) — so no client re-implements sharpe/return/max_dd.
/// An unrecognized `rank_by` is a clean [`Response::Error`] naming the valid set, never a silent
/// fallback to the default metric.
fn run_sweep_profile(profile_toml: &str, rank_by: Option<&str>, store: &StoreHandle) -> Response {
    let rank_by = match rank_by {
        Some(name) => match harness::RankMetric::from_str_ci(name) {
            Some(m) => m,
            None => {
                return Response::Error(format!(
                    "unknown rank_by {name:?} — expected sharpe|return|max_dd|equity"
                ));
            }
        },
        // The `backtest --rank-by` default, so an omitted field ranks like the shipped bin.
        None => harness::RankMetric::Sharpe,
    };
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    if !profile.is_sweep() {
        return Response::Error(
            "profile has no [sweep] table — a sweep needs a parameter grid, e.g. \
             `[sweep]\nfast = [5, 10, 15]`"
                .to_string(),
        );
    }
    let report = match harness::run_sweep(&profile, Arc::clone(store), rank_by) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::SweepReport(json),
        Err(e) => Response::Error(format!("sweep report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML and walk it forward over its `[walkforward]` out-of-sample windows,
/// returning the stitched `WalkForwardReport` as JSON text (v7). Same one-parser contract as
/// [`run_backtest`]; every failure (missing `[walkforward]`, tick/multi-symbol profile, empty
/// slice) is a clean [`Response::Error`].
///
/// # The verb now serves TWO walk-forward protocols, and the PROFILE picks — not the wire
///
/// `harness::run_walkforward` walks FIXED parameters (were these settings stable out of sample?);
/// `harness::walkforward::run_walkforward_optimized` lets each window search its own TRAINING half
/// and carry only that window's winner onto its validation half (does the procedure of fit-then-
/// trade survive out of sample?). Those are different questions with the same report type, so
/// which one ran is a fact an operator must be able to establish — and the only thing that says it
/// is `[walkforward].search`, resolved here through the profile's own
/// `harness::WalkforwardCfg::window_search`.
///
/// **There is deliberately no wire field for it**, and [`Request::RunWalkforwardProfile`] carries
/// only `profile_toml` precisely so there cannot be one: a second place to say "optimize" is a
/// second place for the two to disagree, and the disagreement would be invisible in the answer
/// (both protocols return a `WalkForwardReport`, and the fixed walk's windows simply carry no
/// `chosen_params`). The `[walkforward]` table is the authority for `mode` and `rank_by` for the
/// same reason; this arm reads neither, because the driver it hands off to reads both.
///
/// ⚠ The control is routed to `run_walkforward` ITSELF rather than to the optimizing driver's
/// `WindowSearch::None` arm. That is a choice of ROUTE, not of answer:
/// `crates/vike-backtest/src/harness/walkforward.rs`'s
/// `the_control_reproduces_the_fixed_parameter_walk_exactly` pins the two bit-identical. It is
/// routed this way so the byte-identity gate over THIS verb —
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_walkforward_is_byte_identical_local_and_remote`, which compares this answer against an
/// in-process `harness::run_walkforward` — keeps comparing the same call, rather than resting on a
/// bit-identity that another crate proves and only in its `datafusion-store` lane.
///
/// ⚠ Declared gap: that gate covers the CONTROL only. Nothing in this crate's tests yet ships a
/// `search = "sweep"` profile over the wire, so the SEARCHED route is proven at the harness level
/// and by inspection here, not end-to-end through a socket. It is named rather than implied because
/// a green run of the existing suite says nothing about it.
fn run_walkforward_profile(profile_toml: &str, store: &StoreHandle) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    // Which protocol this profile asked for. An ABSENT `[walkforward]` table is NOT answered here:
    // it falls through as the control and `run_walkforward`'s own pre-flight raises it, so the
    // "profile has no [walkforward] table" message keeps one spelling in the workspace.
    let search = match profile.walkforward.as_ref() {
        Some(cfg) => match cfg.window_search() {
            Ok(s) => s,
            // Unreachable through `from_toml_str` — `BacktestProfile::validate` resolved this same
            // string at load — but spelled as a clean error rather than an `expect`, because this
            // runs on a connection thread where a panic costs the peer its answer.
            Err(e) => return Response::Error(e.to_string()),
        },
        None => harness::walkforward::WindowSearch::None,
    };
    // Exhaustive, no `_` arm, for the reason [`required_scope`] gives about verbs: a new search
    // mode must be routed by whoever adds it, not inherited from whichever side a wildcard picked.
    let walk = match search {
        harness::walkforward::WindowSearch::None => {
            harness::run_walkforward(&profile, Arc::clone(store))
        }
        harness::walkforward::WindowSearch::Sweep => {
            harness::walkforward::run_walkforward_optimized(&profile, Arc::clone(store))
        }
    };
    let report = match walk {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::WalkforwardReport(json),
        Err(e) => Response::Error(format!("walk-forward report serialize failed: {e}")),
    }
}
