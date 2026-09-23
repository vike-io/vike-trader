//! The COMPUTE daemon: `vike-backend backtest --addr`, the blocking thread-per-connection server
//! for the verbs that RUN something (and for the two roster verbs that say what it would run).
//!
//! # What this is, and why it is in THIS crate
//!
//! Ruling 7 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` split one
//! served surface into two daemons. `RunBacktest`, `RunSlice`, `RunParamscan`, `RunWalkforward`,
//! `RunParamscanProfile`, `RunWalkforwardProfile` and `ListStrategies` left `vike-datahub` and are
//! served here; the store verbs stayed there. The owner's argument was responsibility —
//! *"за бэктест и форвард-тест отвечает крейт бэктеста"*, the BACKTEST CRATE is responsible — and
//! this file is that sentence in code: the socket that answers a backtest is opened by the crate
//! that runs one.
//!
//! **No engine moved.** `vike-datahub` never held one; it FORWARDED to `vike_backtest::harness`.
//! What moved is which socket answers, which is why this file is a dispatcher and a handshake and
//! nothing else — the three profile-shaped verbs below are the same `harness::run_backtest` /
//! `run_paramscan` / `run_walkforward` calls, character for character, that the data server used to
//! make.
//!
//! ⚠ **The layer rule is what settled the location, and it also created the one seam here.** The
//! four profile-shaped/roster verbs are pure `vike_backtest::harness` and are served directly. The
//! three STUDIO verbs (`RunSlice`/`RunParamscan`/`RunWalkforward`) run `vike_studio_core`'s slice
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
//! server's.** Every SOURCE-CARRYING `Run*` verb reaches a Rhai compiler —
//! `RunBacktest`/`RunParamscanProfile`/`RunWalkforwardProfile` through a profile's
//! `[strategy.params].src` and `harness::registry`'s `"rhai"` arm, the Studio three through
//! `to_strategy_spec`'s `WireSpec::Rhai` arm — so every one of them is `Scope::Write`, and a
//! key-less non-loopback bind is REFUSED (`vike_datahub_client::bind`). The user-INDICATOR install
//! the datahub used to perform came with them too — but it landed in
//! `crates/vike-backtest/src/backtest_cli.rs`'s `run`, which already did it for the one-shot path;
//! the note above `run_backtest` below carries why, and the ⚠⚠ hazard it inherits.
//!
//! ⚠ **`WireSpec` grew a THIRD variant, `Plugin`, and its scope is a decision rather than a copy of
//! Rhai's.** Read the predicate in `required_scope`'s own doc literally: a verb is `Scope::Write`
//! for the SOURCE its fields can carry, not for the act of running. `WireSpec::Rhai` earns it
//! because it IS source; `WireSpec::Plugin` names an already-built artifact by sha256 and carries
//! no code at all — `to_strategy_spec`'s `WireSpec::Plugin` arm reads no `src` key and resolves
//! through no compiler (see that arm's own comment). Read in isolation, 0064's argument
//! (`docs/decisions/0064-a-named-run-carries-no-source.md`) would let a Plugin-only verb earn
//! `VerbScope::Read`, the way `Request::RunNamed` does for a compiled-in name.
//!
//! **That argument does not reach `RunSlice`/`RunSweep`/`RunWalkforward`, and the reason is
//! structural rather than a policy choice: `required_scope` classifies by REQUEST TYPE, not by
//! which `WireSpec` variant a given frame happens to carry** (`vike_datahub_client::proto`'s
//! `required_scope` matches on `Request::RunSlice { .. }` etc., never on the `spec` field inside
//! it — see that function). Each of these three verbs' `spec: WireSpec` field can STILL be
//! `WireSpec::Rhai` — adding `Plugin` as one more option beside it does not remove that option — so
//! the verb structurally CAN carry source regardless of which variant any one instance sends, and
//! 0064's own fence (a verb resolves only through crates that cannot name `vike-script`) is not
//! satisfied by a `WireSpec` enum whose sibling variant reaches the Rhai compiler. **So these three
//! verbs stay `Scope::Write`, unconditionally, and a `WireSpec::Plugin` frame rides that scope
//! exactly like a `WireSpec::Native` one does — carrying no code is necessary for a lower scope,
//! and this shows it is not sufficient.** A Plugin-only path that earns `VerbScope::Read` would
//! need its OWN verb, structurally unable to carry `WireSpec::Rhai` — the shape `Request::RunNamed`
//! already is — which is out of this track's scope and is not what `WireSpec::Plugin` is.
//!
//! ⚠ **That sentence read "Every `Run*` verb" until 2026-09-16 and it no longer holds in either
//! direction.** `RunStudy` compiles nothing and is Control because it WRITES a run directory
//! (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 6), and `RunNamed` — added by
//! that record — is `VerbScope::Read`: it carries `NamedParam`, which has no variant a script
//! could occupy, and resolves through `vike_user_strategies::named_run::resolve`, whose crate
//! cannot name `vike-script`. **The bind guard is unchanged and must stay unchanged**: this daemon
//! still holds a compiler for its Control verbs, so a key-less non-loopback bind is still refused.
//! What changed is which credential reaches which verb, not what the process can be made to do.
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
    FEATURE_AUTH, FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_STUDY,
    FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, Plane, Request, Response, VerbScope, WireSearch,
    WireStudy, plane_of, read_frame_raw, read_frame_raw_capped, request_kind, required_scope,
    scope_admits, write_frame, wrong_plane_message,
};
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunError, WireRunResult, WireSlice,
    WireSpec, WireWalkforward, WireWalkforwardResult,
};

use crate::data_plan;
use crate::harness::search_select;
use crate::harness::{self, BacktestProfile, BacktestReport};
use crate::named_run::NamedRunLane;

/// The store handle this server serves over — the `vike-data` trait seam, exactly as the data
/// daemon holds it, so the same [`serve`] runs over `DataFusionHist` in production and over the
/// in-memory `MemHistStore` double in tests.
pub type StoreHandle = Arc<dyn HistStore + Send + Sync>;

/// How a `data.explain` plan names the store on THIS route.
///
/// ⚠ It names no path and no rung, and that is the honest answer rather than a gap. This process
/// was handed an already-open [`StoreHandle`] by [`serve`]'s caller; it did not resolve a root, so
/// it does not know which rung chose one. A path printed here would be the most confidently wrong
/// field in the document — and the operator asking "which store answered" has a real place to look,
/// which the sentence names. `crate::backtest_cli`'s local door DOES resolve one and passes both.
const REMOTE_STORE_LABEL: &str = "the store this compute daemon has open (this process resolved no root — the daemon's own \
     --store / VIKE_HIST_STORE is the authority)";

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
pub type StudioParamscanFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            &WireParamscan,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireParamscanResult, WireRunError>
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
    paramscan: StudioParamscanFn,
    walkforward: StudioWalkforwardFn,
}

impl StudioRunTable {
    /// A table from three explicit runners — the ONLY constructor, so a mount is always a
    /// deliberate act by a composition root that could name the real ones.
    pub fn new(
        slice: StudioSliceFn,
        paramscan: StudioParamscanFn,
        walkforward: StudioWalkforwardFn,
    ) -> Self {
        Self { slice, paramscan, walkforward }
    }

    /// The `RunSlice` runner.
    pub fn slice(&self) -> &StudioSliceFn {
        &self.slice
    }

    /// The `RunParamscan` runner.
    pub fn paramscan(&self) -> &StudioParamscanFn {
        &self.paramscan
    }

    /// The `RunWalkforward` runner.
    pub fn walkforward(&self) -> &StudioWalkforwardFn {
        &self.walkforward
    }
}

/// The STUDY runner, injected. `(request, store) -> the run's own JSON document`.
///
/// ⚠ **Injected for the same reason [`StudioRunTable`] is, and it is the only shape available.**
/// `vike_studio_core::study_dispatch`'s `run_study_plan` sits at layer 55 and DEPENDS on this crate
/// at 50, so this crate can neither call it nor name its types — only a composition root that can
/// see both may hand it down. `crates/vike/src/main.rs`'s `backtest_main` does; the standalone
/// `src/bin/backtest.rs` passes `None` and this daemon then refuses the verb by name
/// ([`study_verb_unmounted`]).
///
/// ⚠ **A SEPARATE seam rather than a fourth runner on [`StudioRunTable`]**, whose own doc argues
/// its three are ONE capability. A study negotiates its own capability string
/// (`vike_datahub_client::proto`'s `FEATURE_STUDY`), belongs to a different plane in the CLI's
/// vocabulary (ruling R1 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`), and can be mounted
/// independently — folding it in would make that string mean "the Studio runners are mounted",
/// which is a sentence nobody could read off the wire.
///
/// The answer is JSON TEXT because [`Response::StudyReport`] is: the run manifest and study outcome
/// are Serialize-only, and one direction is all this verb needs.
pub type StudyRunFn = Box<dyn Fn(&WireStudy, StoreHandle) -> Result<String, String> + Send + Sync>;

/// How a composition root NAMES the study runner without resolving the daemon's own directories.
///
/// ⚠ **A FACTORY rather than a built [`StudyRunFn`], and `crates/vike-ops/tests/multicall_gate.rs`
/// is why.** The `vike-backend` dispatcher may perform exactly ONE `std::env::vars()` sweep and ONE
/// `current_dir()` and then hand over — `the_dispatcher_starts_nothing` fails a PR that calls
/// `state_path::` there at all, because any project-relative resolution in the dispatcher is the
/// second walk in another costume. So the root passes the FUNCTION ITEM
/// (`vike_studio_core::study_run_fn`), and `crate::backtest_cli`'s `--addr` arm — which already
/// owns the walk that resolves this daemon's settings — supplies the runs root and the pinned
/// trainer it resolved.
///
/// A plain `fn` pointer, not a boxed closure: the root captures nothing, and a function item is
/// what makes that visible at the call site.
pub type StudyRunFactory = fn(std::path::PathBuf, Option<std::path::PathBuf>) -> StudyRunFn;

/// Accept connections forever, handling each on its own thread over the shared `store`. No STUDIO
/// table and no keys — the three Studio verbs answer a clean refusal and every connection is
/// unauthenticated, which is the ordinary developer configuration behind the loopback bind guard.
///
/// ⚠ The NAMED-RUN lane is [`NamedRunLane::DISARMED`] here and in [`serve_with_studio`], and that is
/// the default rather than a simplification: `docs/decisions/0064-a-named-run-carries-no-source.md`'s
/// decision 8 makes arming an OPERATOR act, and an entry point that takes no configuration has no
/// operator to have performed it. Both verbs are still ANSWERED — that is the same decision's other
/// half — they just run nothing.
pub fn serve(listener: TcpListener, store: StoreHandle) -> io::Result<()> {
    serve_with_studio(listener, store, None)
}

/// [`serve`] with the three STUDIO runners mounted (or not) — see [`StudioRunTable`].
///
/// ⚠ It mounts NO STUDY runner: that is a separate, separately-negotiated capability
/// ([`StudyRunFn`]), and [`serve_authed`] is the entry that can take one.
pub fn serve_with_studio(
    listener: TcpListener,
    store: StoreHandle,
    studio: Option<StudioRunTable>,
) -> io::Result<()> {
    serve_authed(listener, store, studio, None, None, NamedRunLane::DISARMED)
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
    study: Option<StudyRunFn>,
    keys: Option<NodeKeys>,
    // ⚠ The NAMED-RUN lane, INJECTED rather than read here
    // (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8). The switch is an
    // environment variable, and `crate::backtest_cli`'s `run` already owns the one
    // `std::env::vars()` sweep this binary performs — so the parse is
    // `crate::named_run::NamedRunLane::from_vars` over that map and this stays a `Layer::Injected`
    // row rather than joining `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`, which
    // is a ratchet that may only shrink.
    //
    // It is NOT an `Option`, unlike `studio` and `study` above, and the difference is real: those
    // two are MOUNTS that may be absent, so their verbs answer a refusal naming what is missing.
    // This lane is always present and always answers; `armed` says whether it RUNS.
    named_run: NamedRunLane,
) -> io::Result<()> {
    let studio = studio.map(Arc::new);
    let study = study.map(Arc::new);
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
                let study = study.clone();
                let keys = keys.clone();
                thread::spawn(move || {
                    handle_connection(
                        stream,
                        store,
                        studio.as_deref(),
                        study.as_deref(),
                        keys.as_deref(),
                        named_run,
                    )
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
    study: Option<&StudyRunFn>,
    keys: Option<&NodeKeys>,
    named_run: NamedRunLane,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-backtest serve: connection opened");

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
    study: Option<&StudyRunFn>,
    named_run: NamedRunLane,
) -> Response {
    match request {
        // The OPTIONAL version handshake: answer with THIS server's `PROTO_VERSION` and the verbs
        // it serves. Only reachable on a KEY-LESS server — a keyed one answers `Hello` inside
        // `run_handshake` and never returns here, which is why `requires_auth` is `false`.
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-backtest serve: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                features: served_features(studio.is_some(), study.is_some(), false),
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
        Request::RunParamscanProfile { profile_toml, rank_by, search } => {
            run_paramscan_profile(&profile_toml, rank_by.as_deref(), search.as_ref(), store)
        }
        Request::RunWalkforwardProfile { profile_toml } => {
            run_walkforward_profile(&profile_toml, store)
        }
        // The research plane's verb (ruling R1), through the INJECTED runner — see [`StudyRunFn`]
        // for why it cannot be called from this crate directly.
        Request::RunStudy(study_req) => match study {
            Some(run) => match run(&study_req, Arc::clone(store)) {
                Ok(json) => Response::StudyReport(json),
                Err(msg) => Response::Error(msg),
            },
            None => study_verb_unmounted(),
        },
        // The strategy-roster verb. Store-independent (the roster is a compile-time `&[&str]`
        // const) and, since ruling 7, served beside the `--list` flag of this very binary, which reads
        // the same constant — one constant, two transports, one crate.
        Request::ListStrategies => {
            Response::Strategies(harness::STRATEGIES.iter().map(|s| s.to_string()).collect())
        }

        // ⚠ **THE NAMED RUN AND ITS ROSTER — the only Observe-scope `Run*` pair on this wire**
        // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Everything that makes them
        // different from their neighbours above is in `crate::named_run`, deliberately: this arm is
        // a call, so there is no second place where the fence, the bounds or the arming could be
        // re-spelled slightly differently.
        //
        // ⚠ The roster is NOT `harness::STRATEGIES` one arm up, and the difference runs in BOTH
        // directions — that roster carries the simulator-only arms that sit beside the Rhai
        // compiler and therefore outside the named run's closure, and it has never carried the
        // operator's own compiled-in user strategies, which a named run DOES resolve. 0064's
        // decision 7 is the ruling; `crate::named_run::named_roster` is the one implementation.
        Request::NamedStrategies => {
            Response::NamedStrategies(crate::named_run::named_roster(named_run))
        }
        Request::RunNamed(spec) => match crate::named_run::serve_named_run(&spec, store, named_run)
        {
            Ok(outcome) => Response::NamedRun(Box::new(outcome)),
            // A malformed request or a store failure — the two things that are not an OUTCOME of
            // running. An unarmed lane, a full slot table and an unknown name are all `Ok`.
            Err(msg) => Response::Error(msg),
        },

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
        Request::RunParamscan { spec, slice, paramscan, params } => match studio {
            Some(table) => {
                match (table.paramscan())(
                    &spec,
                    &slice,
                    &paramscan,
                    params.as_ref(),
                    Arc::clone(store),
                ) {
                    Ok(result) => Response::ParamscanResult(result),
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
        // ...and the SIX reads `docs/decisions/0084` added beside them. Same plane, same
        // refusal: this daemon holds the COMPUTE store handle, not the data daemon's.
        Request::ScanBookUpdates { .. } => data_verb_elsewhere("ScanBookUpdates"),
        Request::ScanDepth { .. } => data_verb_elsewhere("ScanDepth"),
        Request::ScanCohort { .. } => data_verb_elsewhere("ScanCohort"),
        Request::ScanPerpMetrics { .. } => data_verb_elsewhere("ScanPerpMetrics"),
        Request::ScanEquity { .. } => data_verb_elsewhere("ScanEquity"),
        Request::ScanExecFills { .. } => data_verb_elsewhere("ScanExecFills"),
        Request::PropertiesAsOf { .. } => data_verb_elsewhere("PropertiesAsOf"),
        Request::ListSeries => data_verb_elsewhere("ListSeries"),
        Request::Inventory => data_verb_elsewhere("Inventory"),
        Request::SeriesGaps { .. } => data_verb_elsewhere("SeriesGaps"),
        Request::Coverage => data_verb_elsewhere("Coverage"),
        Request::Backfill { .. } => data_verb_elsewhere("Backfill"),
        // The chart-gap seed. Its SCOPE is unlike its neighbours here (`VerbScope::Read`,
        // `docs/decisions/0057`), and its PLANE is not: the store and the collector table it needs
        // are the data daemon's, so this daemon refuses it exactly as it refuses `Backfill`.
        Request::SeedSeries { .. } => data_verb_elsewhere("SeedSeries"),
        // The venue catalog. Same shape as the seed above and for a sharper reason: this daemon
        // links no venue bridge at all, so it holds no `CatalogProvider` to answer with
        // (`docs/decisions/0062`). Its scope is Observe and its plane is the data daemon's.
        Request::VenueCatalog { .. } => data_verb_elsewhere("VenueCatalog"),
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
///
/// ⚠ **`run_sweep_profile` and `run_sweep` keep the OLD spelling on purpose, and this sentence is
/// not a guard** — the `sweep` -> `paramscan` rename pass rewrote this very paragraph to
/// `run_paramscan_profile`/`run_paramscan` while the code nine lines down still pushed
/// `"run_sweep_profile"`/`"run_sweep"`, so the doc that asserted "unchanged" was the one that
/// changed, and it named two strings that appear on no wire. A capability string is a NEGOTIATED
/// TOKEN, not an identifier: an older client compares it literally, so renaming it would refuse
/// every peer that shipped before the rename. The authority is the code below, never this list;
/// if the two disagree, the code is right. `crates/vike-datahub/src/server.rs`'s `served_features`
/// carries the same frozen spellings in its own removal note.
fn served_features(has_studio: bool, has_study: bool, requires_auth: bool) -> Vec<String> {
    let mut features = vec![
        "backtest".to_string(),
        "list_strategies".to_string(),
        "run_sweep_profile".to_string(),
        "run_walkforward_profile".to_string(),
        // ⚠ A CAPABILITY, not a verb name, and the first entry here that is not one. It says
        // `RunParamscanProfile` can carry a search METHOD (and can rank by the composite objective) —
        // a question a client cannot ask any other way, because a daemon predating the field
        // decodes the frame, drops it and answers a normal report. UNCONDITIONAL, like
        // `FEATURE_COVERAGE` and unlike `FEATURE_BACKFILL`: this whole module is behind
        // `hist-replay`, so a build that has the module has the arm.
        // `vike_datahub_client::proto`'s `FEATURE_SEARCH_METHOD` carries the three legs.
        FEATURE_SEARCH_METHOD.to_string(),
        // ⚠ UNCONDITIONAL, and it is a BUILD fact for the same reason `FEATURE_SEARCH_METHOD` above
        // is one: this whole module rides `hist-replay`, so a build that compiles this file has
        // both named-run arms. It is deliberately NOT conditioned on the ARMING, and that is
        // `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8: an unarmed server
        // must ANSWER, and if the capability rode the arming then an OLD server and an unarmed one
        // would be indistinguishable in `Welcome.features` — one silence for two different
        // sentences an operator needs to tell apart. The arming rides in the ANSWER instead
        // (`NamedRunOutcome::NotArmed`, `NamedRoster::armed`), which makes all three states
        // distinguishable with one string.
        FEATURE_NAMED_RUN.to_string(),
    ];
    if has_studio {
        features.push("run_slice".to_string());
        features.push("run_sweep".to_string());
        features.push("run_walkforward".to_string());
        // ⚠ A CAPABILITY rather than a verb name, like `FEATURE_SEARCH_METHOD` above — and
        // CONDITIONAL where that one is unconditional, which is why both say which they are. That
        // one is a BUILD fact: one `hist-replay`-gated module, nothing injected, so a build with
        // the module has the arm. This one rides `RunWalkforward`, whose runner comes from
        // `vike-studio-core` ABOVE this crate in the layer graph, so it is a MOUNT fact — the same
        // rule `FEATURE_STUDY` below follows, and advertising it unmounted would invite a frame
        // whose only possible answer is a refusal. It says the STUDIO walk-forward can carry a
        // per-window SEARCH: a question a client cannot ask any other way, because a daemon
        // predating the field decodes the frame, drops it, and answers a normal FIXED-walk report.
        // `vike_datahub_client::proto`'s `FEATURE_WALKFORWARD_SEARCH` carries the three legs.
        features.push(FEATURE_WALKFORWARD_SEARCH.to_string());
    }
    if has_study {
        // ⚠ CONDITIONAL, and the OPPOSITE rule to `FEATURE_SEARCH_METHOD` a few lines up — which is
        // why both say which they are. That one is a BUILD fact: one `hist-replay`-gated module,
        // nothing injected, so a build with the module has the arm. This one is a MOUNT fact like
        // `FEATURE_BACKFILL`: the runner comes from `vike-studio-core`, ABOVE this crate in the
        // layer graph, so a build that compiles this file may still have nothing to run. A daemon
        // that advertised it unmounted would invite a frame whose only possible answer is a
        // refusal, which is precisely what a capability string exists to prevent.
        features.push(FEATURE_STUDY.to_string());
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

/// **The `data.explain` door on the REMOTE route**: `Some(response)` when the profile asked for a
/// PLAN instead of a run, `None` when it did not.
///
/// # Why it needs no wire verb of its own, and what that buys
///
/// `data.explain` is a PROFILE key, and only the profile text crosses the wire — so the plan is
/// reachable on this route with no [`Request`] variant, no protocol version bump and no client
/// change. [`Response::Report`] already carries JSON text, and
/// `crates/vike-cli/src/cmd/backtest.rs`'s `Route::Single` arm already pretty-prints whatever JSON
/// came back, so an operator typing `vike-cli backtest run --explain-data` against a remote daemon
/// gets the plan on stdout and exit 0. The alternative — a dedicated verb — would have put the
/// answer behind a protocol bump and a client that has to know the shape, for a document the client
/// never interprets.
///
/// That is also why the document carries its own rendered SENTENCES (see
/// `crate::data_plan::DataPlan::to_json`): this side has the plan and the far side has no renderer
/// for it.
///
/// # ⚠ It is the FIRST thing all three profile arms do, and nothing computes
///
/// Called immediately after `from_toml_str` in each of them, so a planning request never builds an
/// evaluator, never compiles a Rhai strategy and never touches the engine. The profile has still
/// been VALIDATED by then, deliberately: a plan for a profile that could not run is a plan for
/// nothing.
fn explain_instead_of_running(profile: &BacktestProfile, store: &StoreHandle) -> Option<Response> {
    if !profile.data.explain {
        return None;
    }
    let plan = match data_plan::plan_data(profile, store.as_ref(), REMOTE_STORE_LABEL, None) {
        Ok(p) => p,
        Err(e) => return Some(Response::Error(e.to_string())),
    };
    let doc = match data_plan::explain_document(profile, &plan) {
        Ok(d) => d,
        Err(e) => return Some(Response::Error(e.to_string())),
    };
    match serde_json::to_string(&doc) {
        Ok(json) => Some(Response::Report(json)),
        Err(e) => Some(Response::Error(format!("data plan serialize failed: {e}"))),
    }
}

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
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    let result = match harness::run_backtest(&profile, Arc::clone(store)) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    // Stamped on the same terms as the local door (`crates/vike-backtest/src/backtest_cli.rs`'s
    // single-run arm): a `--local` run and an `--addr` run must not answer differently about what
    // the fills cost, and the profile is right here.
    let report = BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    )
    .with_realism(harness::report::realism_stamp(&profile));
    match serde_json::to_string(&report) {
        Ok(json) => Response::Report(json),
        Err(e) => Response::Error(format!("report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML, run its `[paramscan]` grid over `store` with the SELECTED search
/// method, and return the RANKED [`harness::ParamscanReport`] as JSON text (v7).
///
/// One authority for the profile, exactly like [`run_backtest`]: `BacktestProfile::from_toml_str`
/// parses AND validates — the same guard the file path applies — so a remote search and a local
/// `backtest --profile … --rank-by …` are the same computation over the same `[engine]` (fee
/// schedule included).
///
/// The run is `harness::optimize` over the evaluator `search_select::evaluator_for` chose. ⚠ For
/// grid + a classic metric that resolves to `StoreEvaluator::classic` + `GridSearch` — **the
/// identical call `harness::run_paramscan_exec` makes**, which is what keeps
/// `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
/// `profile_sweep_is_byte_identical_local_and_remote` green. Routing everything through the uniform
/// objective path instead would add a `score` key to every row and change `rank_by`'s STRING for
/// three of the four metrics; `search_select::uses_classic_evaluator` carries that argument.
///
/// THE SELECTOR IS SHARED with the engine binary's argv parser (`harness::search_select`), so the
/// ownership rule, the three value parsers and every refusal sentence exist ONCE: a remote
/// `--optimizer grid --trials 8` is refused with the sentence a `--local` one is refused with.
/// RANKING HAPPENS HERE, over the real `BacktestReport`s — so no client re-implements
/// sharpe/return/max_dd — and an unrecognized `rank_by` is a clean [`Response::Error`] naming the
/// valid set, never a silent fallback.
///
/// # The contrast with [`run_walkforward_profile`] one function down
///
/// A search's METHOD has no profile home and can never get one: `BacktestProfile` is
/// `deny_unknown_fields`, so a top-level `optimizer = "tpe"` is a hard parse error, and the
/// parameter-grid table's every key is an AXIS — `[paramscan].method = "tpe"` declares an axis
/// named `method`. There is exactly one place it can live, and that is
/// [`Request::RunParamscanProfile`]'s `search` field.
///
/// Walk-forward's shape is different, and its own doc argues it where it lives: that verb carries
/// only `profile_toml` so there cannot be a second place to say "optimize". What a walked-forward
/// SEARCH should name, and where, is ruling R7's question (walk-forward is a MODIFIER over a run,
/// not a third run kind) for the stage that owns that routing. **Stage 7 widens ONE verb and states
/// that boundary rather than pretending the other verb's shape is settled.**
fn run_paramscan_profile(
    profile_toml: &str,
    rank_by: Option<&str>,
    search: Option<&WireSearch>,
    store: &StoreHandle,
) -> Response {
    // ⚠ ONE selector, shared with the engine binary's argv parser — `harness::search_select`. The
    // ownership rule (`--trials` is tpe-or-genetic, `--euler-depth` euler, `--seed` both stochastic
    // methods), the value parsers and their refusal texts are that module's, so a remote
    // `--optimizer grid --trials 8` reads exactly like a `--local` one. Duplicating the table here
    // is the two-rosters defect stage 7 exists to kill.
    let rank = match search_select::resolve_rank(rank_by) {
        Ok(r) => r,
        Err(e) => return Response::Error(e),
    };
    let empty = WireSearch::default();
    let w = search.unwrap_or(&empty);
    let method = match search_select::resolve(&search_select::SearchSelection {
        optimizer: w.optimizer.as_deref(),
        euler_depth: w.euler_depth.as_deref(),
        trials: w.trials.as_deref(),
        seed: w.seed.as_deref(),
    }) {
        Ok(m) => m,
        Err(e) => return Response::Error(e),
    };
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    // ⚠ BEFORE the grid check, deliberately: a search's data SLICE is the same for every trial, so
    // a coverage problem here is a problem with the whole search — and the plan is exactly what an
    // operator wants before spending a grid on it. The plan's own notes say the `[paramscan]` table
    // is present.
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
    if !profile.is_paramscan() {
        return Response::Error(
            "profile has no [paramscan] table — a parameter search needs a grid, e.g. \
             `[paramscan]\nfast = [5, 10, 15]`"
                .to_string(),
        );
    }
    // ⚠ `objective` is declared BEFORE `eval`: `StoreEvaluator::new` borrows it for the evaluator's
    // whole life and locals drop in reverse declaration order.
    let (objective, label) = search_select::objective_for(rank);
    let exec = harness::ParamscanExec::from_env();
    let eval = match search_select::evaluator_for(
        method,
        rank,
        &profile,
        Arc::clone(store),
        &objective,
        label,
        exec,
    ) {
        Ok(e) => e,
        Err(e) => return Response::Error(e.to_string()),
    };
    // Bound, not inlined: `optimizer_for` returns a `Box<dyn Optimizer>` whose borrow must outlive
    // the call.
    let opt = search_select::optimizer_for(method);
    let report = match harness::optimize(opt.as_ref(), &profile, &eval) {
        Ok(o) => o.report,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::ParamscanReport(json),
        Err(e) => Response::Error(format!("sweep report serialize failed: {e}")),
    }
}

/// The refusal [`Request::RunStudy`] gets when no [`StudyRunFn`] is mounted — the study sibling of
/// [`studio_verb_unmounted`], and a BUILD/composition fact rather than a request fact.
fn study_verb_unmounted() -> Response {
    Response::Error(
        "RunStudy is served only by a build that mounts the study runner, and this one did not. \
         The shipped `vike-backend backtest --addr` mounts it; a bare `cargo run -p vike-backtest \
         --bin backtest` cannot, because that runner lives in `vike-studio-core`, which sits ABOVE \
         this crate in the layer graph and can only be handed in by a composition root. Run the \
         study on the box that holds the store instead: `vike-backend study`"
            .to_string(),
    )
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
    // ⚠ The plan here describes the WHOLE range every window is cut from, not one window — a
    // shortfall at either end lands in the first or last window rather than spreading across all of
    // them, and the plan's notes say so. Nothing slices the plan per window, because the answer an
    // operator needs before a walk is whether the tape reaches both ends of it.
    if let Some(plan) = explain_instead_of_running(&profile, store) {
        return plan;
    }
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
