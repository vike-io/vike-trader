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
//! runners — and `vike-studio-core` is layer 35 while this crate is layer 30, so naming it here is
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
//! `vike_node_proto::auth` — because `crates/vike-ops/tests/architecture/clock_pin/ratchet.rs`'s
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
use std::net::TcpListener;
#[cfg(test)]
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::HistStore;
#[cfg(doc)]
use vike_datahub_client::FEATURE_AUTH;
#[cfg(test)]
use vike_datahub_client::proto::{Request, Response, write_frame};
use vike_node_proto::auth::NodeKeys;
#[cfg(doc)]
use vike_node_proto::auth::Scope;

use crate::named_run::NamedRunLane;

mod connection;
mod dispatch;
mod studio_table;
mod verbs;

use self::connection::handle_connection;
pub use self::studio_table::{
    StudioParamscanFn, StudioRunTable, StudioSliceFn, StudioWalkforwardFn, StudyRunFactory,
    StudyRunFn,
};

/// The store handle this server serves over — the `vike-data` trait seam, exactly as the data
/// daemon holds it, so the same [`serve`] runs over `DataFusionHist` in production and over the
/// in-memory `MemHistStore` double in tests.
pub type StoreHandle = Arc<dyn HistStore + Send + Sync>;

/// How a `data.explain` plan names the store on THIS route.
///
/// ⚠ It names no address and no rung, and that is the honest answer rather than a gap. This process
/// was handed an already-open [`StoreHandle`] by [`serve`]'s caller, so it cannot say which datahub
/// is behind it. The operator asking "which store answered" has a real place to look, which the
/// sentence names: the address the daemon printed at startup.
///
/// ⚠ It named the daemon's own `--store` / `VIKE_HIST_STORE` as the authority until 2026-09-25. That
/// stopped being true the day the local READ door closed (decision 0084): the daemon refuses
/// `--store` and reads every byte of history from a datahub, so the old sentence sent the operator to
/// a flag that is refused and a variable nothing on this route reads.
const REMOTE_STORE_LABEL: &str = "the datahub this compute daemon reads history from (its \
     `config.datahub_addr`, printed at its startup — this process was handed an open handle and names \
     no address)";

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
    // (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8). The switch is the
    // daemon's `--named-run` argument, parsed by `crate::named_run::NamedRunLane::from_args` at the
    // composition root.
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
                thread::Builder::new().name("bt-conn".into()).spawn(move || {
                    handle_connection(
                        stream,
                        store,
                        studio.as_deref(),
                        study.as_deref(),
                        keys.as_deref(),
                        named_run,
                    )
                })?;
            }
            Err(e) => {
                // A failed accept is per-connection; keep serving.
                tracing::warn!(error = %e, "vike-backtest serve: accept failed, continuing");
            }
        }
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    /// The accepted socket carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`),
    /// as every node server's does. Read off the socket itself rather than timed, so it holds on any
    /// OS: a clone of the accepted stream is the same socket, and once a `Pong` has come back
    /// `handle_connection` is past the line that arms it. (`tests/compute_plane.rs`'s
    /// `back_to_back_answers_cost_no_delayed_ack` pins what it is FOR, on Linux.)
    #[test]
    fn the_accepted_socket_has_nagle_off() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let mut client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (accepted, _) = listener.accept().expect("accept");
        let clone = accepted.try_clone().expect("clone the accepted socket");
        assert!(!clone.nodelay().expect("read TCP_NODELAY"), "guard: a fresh socket has Nagle on");
        let store: StoreHandle = Arc::new(vike_data::MemHistStore::new());
        thread::spawn(move || {
            handle_connection(accepted, store, None, None, None, NamedRunLane::DISARMED)
        });
        write_frame(&mut client, &Request::Ping).expect("ping");
        let answer = vike_node_proto::frame::read_frame::<_, Response>(&mut client).expect("pong");
        assert_matches!(answer, Response::Pong, "expected Pong, got {answer:?}");
        assert!(clone.nodelay().expect("read TCP_NODELAY"), "the accepted socket has Nagle on");
    }
}
