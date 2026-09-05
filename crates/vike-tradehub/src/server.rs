//! `server` — the authenticated node server (headless two-layer plan, Layer 2, PR-11 observe + PR-12
//! control).
//!
//! Binds `VIKE_TRADEHUB_ADDR` (default [`DEFAULT_ADDR`] = `127.0.0.1:7879`), thread-per-connection,
//! and runs the PR-10 node handshake before serving anything: `Hello` -> `Welcome{ nonce }` ->
//! `Auth{ scope, mac }` -> (verify via [`vike_tradehub_client::auth::verify`] against the scope's
//! [`NodeKeys`]) -> `AuthOk`. On `Subscribe` an [`Scope::Observe`] connection registers with the
//! [`crate::publish`] publisher and this thread becomes the connection's WRITER, draining its
//! bounded mailbox and writing pushed frames to the socket.
//!
//! ## The control path (PR-12) — a SEPARATE connection that never subscribes
//!
//! A subscribed connection is a one-way PUSH pipe (after `Subscribe` this thread only writes), so a
//! control client authenticates under [`Scope::Control`] and NEVER subscribes — it stays in the
//! request/response loop, and each [`Request::Command`] is lowered ([`lower_command`]) into the core's
//! real `Command`/`OrderIntent` and handed to the [`vike_core::CommandSink`] the daemon threaded in.
//! Control is DOUBLE-GATED: the node must hold a control key ([`NodeKeys::has`]) AND the daemon must
//! pass `Some(sink)` (gated on `VIKE_TRADEHUB_CONTROL=1`). Absent either, a `Control` auth or a
//! `Command` is refused. An [`Scope::Observe`] peer's `Command` is always refused (read-only). Every
//! ACCEPTED command is audit-logged ([`crate::audit`]). Beyond the double gate, the server edge
//! NOTIONAL- and RATE-limits each command ([`ControlLimits`], built per connection from the
//! [`ControlLimitsConfig`] the daemon BINARY resolves once — the notional ceiling from the POLICY
//! file (`max_notional_per_order` in `<vike home>/policy.toml`; it has NO env layer since Phase 5
//! of the settings-unification design), the rate from `VIKE_TRADEHUB_CONTROL_RATE` — and passes
//! into [`serve`] — audit F13: this module reads no env itself, per the settings-registry rule
//! that libraries take configuration as parameters) —
//! defense-in-depth so a leaked control key can't place an oversized order or flood the core, on
//! top of the core `RiskGate` every command still passes through.
//!
//! A [`Request::Command`] may carry an optional operator/agent RATIONALE (v4) beside the command.
//! It is recorded in the audit trail and NOWHERE else: this arm runs it through
//! [`audit::sanitize_reason`] (control characters stripped, length capped — it is remote free text
//! landing in a structured JSON log line) and hands the result to [`audit::record`]. It is never
//! part of [`lower_command`]'s input, so it can never reach `OrderRequest`, the core fold, the
//! journal, or a venue.
//!
//! A `Control` peer may also `Preview` a command (v3): the server runs its edge gate and answers the
//! verdict WITHOUT lowering, executing, or audit-logging it — a server-authoritative dry-run. It is
//! read-only (no order placed, no rate token consumed), scope-gated exactly like `Command` (an
//! `Observe` peer is refused), and advertised in `Welcome.features` (`"preview"`) precisely because
//! it changes nothing.
//!
//! ## The STRATEGY verbs (split-plane B4) — feature-negotiated, never version-bumped
//!
//! Two strategy-level verbs ride `Welcome.features` (`FEATURE_STRATEGY_VERBS`) rather than a
//! `NODE_PROTO_VERSION` bump (the version is folded into the signed auth MAC — a bump breaks the
//! handshake against every running node; a client that does not see the string refuses CLIENT-side):
//!
//! - **`Request::StrategyStatus`** (read, EITHER scope — Observe suffices): identity + rendered
//!   effective params + one `WireMountRow` per mount, answered from the publisher's process-static
//!   identity block ([`crate::publish::PublisherHandle::identity`]).
//! - **`WireCommand::UpdateParams`** (write, `Scope::Control`): lowered by [`lower_command`] into
//!   the core's real `Command::UpdateParams` — the wire carries the core's own `StrategyParams`
//!   serde JSON, an undecodable payload is refused as `Response::Error` — and it flows through the
//!   SAME [`accept_command`] path as every order verb: rate token, audit record (`"update_params"`),
//!   single-writer lane. The notional cap deliberately does not apply (a params update is not an
//!   order; the decision is documented at the `notional_reason` arm).
//!
//! ## [`accept_command`] — the ONE acceptance path, shared by every control SURFACE
//!
//! The TCP `Request::Command` arm below is no longer the only way a command reaches the core: the
//! opt-in `telegram` channel (behind the crate feature of the same name, so a default build has no
//! such surface) is a SECOND remote write surface. Rather than let it grow its
//! own gating (which would drift the moment either side is edited), the whole acceptance body was
//! factored into [`accept_command`] — `ControlLimits::vet` (rate token + notional cap) ->
//! [`audit::sanitize_reason`] -> [`lower_command`] -> `CommandSink::try_command` ->
//! [`audit::record`], in that order — and BOTH surfaces call it. Identical gating by CONSTRUCTION,
//! not by review. The TCP arm keeps its own scope/sink gates (they are connection properties) and
//! renders [`AcceptError`] back onto the exact `Response::Error` strings it always sent.
//!
//! [`AcceptError`] is an ENUM rather than the plain `String` a first sketch had, for one behavioral
//! reason: a `CommandRejected::Gone` (the core is shutting down) must CLOSE the connection, while a
//! `Busy`/refusal must not. Collapsing both into one string would silently drop that distinction on
//! the security-sensitive write path, so the variant carries it ([`AcceptError::is_fatal`]) and
//! [`AcceptError::message`] carries the operator-visible text both surfaces render.
//!
//! ## Reachability is the OUTER barrier, and it is not optional
//!
//! Everything above is authorization. NONE of it is confidentiality or integrity: the handshake is
//! **plaintext** (the challenge nonce and the mac both go on the wire in the clear, so a passive
//! observer gets an offline cracking target for the node key), and it authenticates the
//! CONNECTION, not each frame — after `AuthOk` an on-path attacker can inject a `Command` frame
//! into the stream and this server will lower it into the core. There is no TLS here and
//! deliberately so: the design (spec §II.5) is that the listener stays on loopback and an SSH
//! tunnel (`ssh -L 7879:localhost:7879 the CI box`) provides both, exactly as `vike-datahub` is reached.
//!
//! Three things in this module hold that up, and each states its own reasoning:
//! [`bind_exposure`] (the daemon REFUSES a non-loopback bind unless explicitly opted in),
//! [`MAX_CONNECTIONS`] (an unauthenticated peer cannot spawn threads without bound) and
//! [`HANDSHAKE_MAX_FRAME_LEN`] (nor name a 64 MiB allocation before authenticating).
//!
//! ## Link liveness: what an idle connection experiences (and how a quiet one proves it is alive)
//!
//! Two numbers govern it and NEITHER is defined here: they are the server's half of a contract with
//! the client, so they live one layer down in `vike_tradehub_client::liveness` and this module
//! consumes them ([`LinkPolicy::PRODUCTION`] is where they land).
//!
//! - **An AUTHENTICATED connection is never closed for being quiet.** [`handle_connection`] REPLACES
//!   the unauthenticated [`HANDSHAKE_READ_TIMEOUT`] with
//!   [`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`] (`None`) the instant a scope is
//!   granted. It used not to, and the consequence was a control link — which never subscribes and
//!   so stays in the read loop — closed after five quiet minutes, with the client reporting its
//!   next command as "outcome UNKNOWN, may have executed" for a command the node had stopped
//!   reading before it was written. The unauthenticated bound is untouched.
//! - **A SUBSCRIBED stream carries a heartbeat.** [`run_push_writer`] writes one `Response::Pong`
//!   per [`vike_tradehub_client::liveness::OBSERVE_HEARTBEAT`] of silence, advertised as
//!   [`FEATURE_OBSERVE_HEARTBEAT`] so a client may deadline its read and stop mistaking a dead link
//!   for a quiet one. No wire change (the variant is as old as the protocol) and no cost on a busy
//!   node (a real frame resets the timer).
//!
//! ## The hot-path guarantee
//!
//! Nothing in this module touches the vike-core fold. The publisher (which this server feeds) reads
//! only the arc-swap snapshot cell; per-connection writer threads do socket I/O in isolation. A
//! stalled client blocks only ITS OWN writer thread (parked in `write_all` on a full send buffer),
//! never the publisher and never another client — the mailbox drop-oldest contract guarantees it.
//! This is why the p99 core-hop latency gate is unaffected (the PR-11 merge condition).
//!
//! ## vike-app `--observe` — BUILT (PR-13)
//!
//! This server + [`crate::publish`] + `vike_tradehub_client::{RemoteCoreHandle, RemoteControlHandle}`
//! are the CI-testable core. `vike-app --observe <ADDR>` drives the GUI panels from a
//! `RemoteCoreHandle` (watch) and, with the control gate/key, its order buttons from a
//! `RemoteControlHandle` (trade). That GUI wiring is local-verify (vike-app is compile-checked but
//! never TESTED in CI); the wire + server + limits here are covered by `tests/control_roundtrip.rs`
//! and the unit tests below.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)
use vike_core::{CommandRejected, CommandSink};
use vike_exec::{Command, MountSpec, OrderIntent, ParamsUpdate, TradingState};
use vike_model::{OrderRequest, StrategyParams};
use vike_tradehub_client::auth::{self, NodeKeys};
use vike_tradehub_client::liveness;
use vike_tradehub_client::proto::{
    FEATURE_MOUNT_VERBS, FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_VERBS, NODE_PROTO_VERSION, Request, Response, Scope, datahub_feature,
    read_frame_raw, read_frame_raw_capped, write_frame,
};
use vike_tradehub_client::wire::{
    WireCommand, WireMountRow, WireSettingsRow, WireSettingsShow, WireStrategyStatus,
    WireTradingState,
};

use crate::audit;
use crate::publish::{PublisherHandle, Recv};

/// The default observe-server bind address — localhost only, reached over an SSH tunnel
/// (`ssh -L 7879:localhost:7879 the CI box`). The listener MUST stay bound to loopback: auth is defense in
/// depth, the tunnel is the reachability barrier (spec §II.5).
pub const DEFAULT_ADDR: &str = "127.0.0.1:7879";

/// A generous per-connection read timeout for the HANDSHAKE phase, so a half-open peer cannot pin a
/// connection thread in `read` forever (mirrors vike-datahub's `IDLE_READ_TIMEOUT`). Once a client
/// subscribes, the thread stops reading and becomes a writer, so this only bounds a stalled handshake.
///
/// ⚠ **It bounds the UNAUTHENTICATED phase, and now it says so in code as well as in prose.** This
/// value used to be set on the accepted socket and never touched again, so it silently became the
/// IDLE policy of every AUTHENTICATED connection too — and a control peer never subscribes, it
/// stays in this module's request/response read loop, so the node closed it the first time its
/// operator (or its agent) went five minutes without a command. `log_read_end` filed that as an
/// "idle read timeout" and the client, which learns of a close only when it next writes, reported
/// the NEXT command's outcome as UNKNOWN — "may have executed" — for a command the node had
/// stopped reading before it was written. [`handle_connection`] now REPLACES this the moment the
/// handshake grants a scope, with [`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`], which
/// argues the replacement.
const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a subscriber's writer thread blocks for the next frame before looping to re-check its
/// mailbox's closed state — bounds teardown latency on an idle push stream.
const WRITER_RECV_TIMEOUT: Duration = Duration::from_millis(200);

/// The three per-connection LINK-LIVENESS numbers this server runs a connection under, in one
/// struct so a test can scale them and production cannot.
///
/// [`Self::PRODUCTION`] is the only value any shipped path uses — [`serve`] passes it and nothing
/// else may choose one, because two of the three are halves of a CONTRACT with the client
/// (`vike_tradehub_client::liveness`, which owns the numbers; this crate consumes them and defines
/// none of its own). It exists as a parameter for one reason: the properties worth testing are
/// "an authed connection outlives the old five-minute bound" and "an idle subscriber is still being
/// written to", and a test that proved either at production scale would take five minutes and
/// fifteen seconds respectively. The unit tests below scale all three by ~1000x and assert the same
/// properties in under two seconds.
///
/// Deliberately NOT a `pub` seam: an operator knob here would let one side of a two-sided contract
/// be changed alone, which is the failure the shared home exists to prevent.
#[derive(Debug, Clone, Copy)]
struct LinkPolicy {
    /// What an UNAUTHENTICATED peer gets: how long the handshake may stall before the connection
    /// thread is released. See [`HANDSHAKE_READ_TIMEOUT`].
    handshake_read_timeout: Duration,
    /// What an AUTHENTICATED connection gets INSTEAD, from `AuthOk` onward. See
    /// [`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`] for why `None` is the answer.
    authed_idle_timeout: Option<Duration>,
    /// How often a SUBSCRIBED connection's writer re-asserts liveness when it has no frame to send.
    /// See [`vike_tradehub_client::liveness::OBSERVE_HEARTBEAT`].
    observe_heartbeat: Duration,
}

impl LinkPolicy {
    /// The shipped policy: this crate's own unauthenticated handshake bound, plus the two numbers
    /// the CLIENT crate owns because both ends must agree on them.
    const PRODUCTION: LinkPolicy = LinkPolicy {
        handshake_read_timeout: HANDSHAKE_READ_TIMEOUT,
        authed_idle_timeout: liveness::AUTHED_IDLE_TIMEOUT,
        observe_heartbeat: liveness::OBSERVE_HEARTBEAT,
    };
}

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends.
///
/// The shared `MAX_FRAME_LEN` is 64 MiB because a legitimate datahub *answer* (a chart's worth of
/// bars) can be large. A handshake frame cannot: `Hello` is a version number and `Auth` is a scope
/// plus a 32-byte mac, together a few hundred bytes even JSON-encoded. Accepting 64 MiB of them
/// means anyone who can reach the socket makes this server allocate 64 MiB per connection by
/// sending FOUR BYTES — no key, no round trip, and (before the cap in [`serve`]) no limit on how
/// many times. 64 KiB is ~200x the largest real handshake frame and 1024x cheaper to be wrong about.
///
/// Post-auth frames keep the full [`vike_tradehub_client::proto::MAX_FRAME_LEN`]: by then the peer
/// has proved possession of a scoped key, and a `Command` carries an operator rationale of
/// unbounded-ish length that `audit::sanitize_reason` caps downstream.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How many connections this server handles at once. Beyond it, a new connection is accepted by the
/// kernel and immediately DROPPED (no frame written, no thread spawned).
///
/// Each connection owns a THREAD for its lifetime — a subscriber's thread parks in `write_all`, and
/// the handshake read timeout is 300 s — so without a cap, anyone who can reach the socket spawns
/// threads until the process runs out of them, by opening sockets and saying nothing. The real
/// population is a GUI or two plus a control client, so 64 is roughly two orders of magnitude of
/// headroom over legitimate use while still being a bound.
///
/// ⚠ It bounds THREADS, not authorization: an unauthenticated peer still occupies a slot until its
/// handshake times out. That is why it is defense in depth *behind* the loopback default
/// ([`bind_exposure`]), not a reason to widen the bind.
pub const MAX_CONNECTIONS: usize = 64;

// Compile-time bounds on the two limits above, the `confirm.rs` idiom: a RANGE, so a deliberate
// tweak stays free while "the bound was effectively removed" (`usize::MAX`) and "the cap was set to
// something that refuses every connection" (`0`) do not compile. Same for the pre-auth ceiling,
// which must stay far below the shared `MAX_FRAME_LEN` it exists to undercut.
const _: () = assert!(
    MAX_CONNECTIONS > 0 && MAX_CONNECTIONS <= 4096,
    "MAX_CONNECTIONS must stay a POSITIVE, bounded number of connection threads"
);
const _: () = assert!(
    HANDSHAKE_MAX_FRAME_LEN > 0 && HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);

/// How exposed a resolved bind target is — the input to the daemon's non-loopback refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindExposure {
    /// Every resolved address is a loopback address: reachable only from this host.
    Loopback,
    /// At least one resolved address is NOT loopback (a specific interface, or the `0.0.0.0` /
    /// `::` wildcard). Carries the first such address, for the operator-facing message.
    Public(SocketAddr),
    /// The address resolved to nothing at all — `bind` would fail anyway; classified separately so
    /// a caller never has to read "not loopback" as "publicly exposed".
    Unresolvable,
}

/// Classify an already-RESOLVED bind target. Pure, so the policy is unit-testable without a socket
/// and without DNS; the caller does the `to_socket_addrs()` that produced `resolved`, which is the
/// same resolution `TcpListener::bind` performs.
///
/// A target is [`BindExposure::Loopback`] only when EVERY resolved address is loopback — a hostname
/// resolving to both `127.0.0.1` and a LAN address is exposed on the LAN, so the strictest answer
/// is the true one. `0.0.0.0` and `::` are the wildcard binds, which `IpAddr::is_loopback` reports
/// false for, so they classify as [`BindExposure::Public`] — correctly: they listen on every
/// interface, which is the single most common way this surface gets accidentally exposed.
pub fn bind_exposure(resolved: &[SocketAddr]) -> BindExposure {
    match resolved.iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => BindExposure::Public(*a),
        None if resolved.is_empty() => BindExposure::Unresolvable,
        None => BindExposure::Loopback,
    }
}

/// What the daemon should DO about a bind target — the policy, kept here in the library rather than
/// inline in the binary so it is unit-testable and so a change to it shows up as a change to a
/// named function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// Loopback (or unresolvable — `bind` will report that itself): bind, say nothing special.
    Proceed,
    /// Non-loopback WITH the operator's explicit opt-in: bind, but announce what is now reachable.
    ProceedExposed(SocketAddr),
    /// Non-loopback with NO opt-in: do not bind. The daemon keeps trading headless.
    Refuse(SocketAddr),
}

/// Decide whether to open the listener. `allow_public` is the operator's explicit opt-in
/// (`flags.tradehub_allow_public_bind` / `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND`).
///
/// ⚠ The refusal is the point, and it is a DEFAULT rather than a prohibition. This server's
/// handshake is plaintext and per-connection (see the module doc), so its confidentiality and
/// integrity come from being unreachable — an SSH tunnel, the way `vike-datahub` is reached. But
/// reaching a node across a trusted LAN or a VPN interface is a real deployment, and a guard with
/// no escape hatch is one people route around by other means, so the escape hatch is NAMED and
/// separate: an address cannot be its own consent, because typing the address is the mistake.
///
/// An UNRESOLVABLE target proceeds deliberately: `TcpListener::bind` is about to fail on it with a
/// better message than this function could invent, and reporting "not loopback" for an address
/// that is not anything would send an operator looking for an exposure that does not exist.
pub fn bind_decision(resolved: &[SocketAddr], allow_public: bool) -> BindDecision {
    match bind_exposure(resolved) {
        BindExposure::Loopback | BindExposure::Unresolvable => BindDecision::Proceed,
        BindExposure::Public(a) if allow_public => BindDecision::ProceedExposed(a),
        BindExposure::Public(a) => BindDecision::Refuse(a),
    }
}

/// Decrements the live-connection count when a connection thread ends — including on a panic, which
/// a plain `fetch_sub` at the end of [`handle_connection`] would leak past.
struct ConnSlot(Arc<AtomicUsize>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// WHERE the node's [`Request::SettingsShow`] answer comes from (split-plane REQ-7, read half):
/// the boot-resolved settings DIRECTORY plus the daemon's own startup environment sweep, threaded
/// in by the BINARY the way the identity block was for B3 — the server holds a handle, the binary
/// owns the I/O and the env read (the settings-registry rule).
///
/// ## Freshness — the CLI's semantics, deliberately
///
/// `vike-cli config show` re-reads the settings files on every invocation, so this source does
/// too: each request runs `vike_config::describe` (which re-loads the TOMLs) over the SAME
/// directory the daemon resolved at boot and the SAME env sweep it booted with. Two consequences,
/// both intended:
///
/// - A file edited AFTER boot answers with the NEWER value — i.e. the rows show what a restart
///   would load, which is exactly the view the WRITE half's restart-to-apply flow needs (and the
///   same window `main.rs`'s startup disclosure already accepts for `vike_config::boot_lines`).
/// - Settings that no longer LOAD (a typo'd key written after boot) answer an honest
///   [`Response::Error`] naming the file, never a fabricated table.
///
/// The env half does NOT re-sweep: a daemon's environment is fixed at spawn, so the boot sweep IS
/// the current one, and re-sweeping would make a library read global state its caller cannot see.
///
/// ## Scope — why [`Scope::Observe`] suffices (the redaction argument)
///
/// The payload is the FILES half of `config show` ONLY — `vike_config::show::file_rows`, the
/// shared builder whose rows are **redacted on construction** (`vike_config::show`'s
/// `resolve_file_row` applies `vike_config::redact::is_secret_key` to every key; no settings
/// field is credential-shaped today, and the insurance is pinned by that module's
/// `a_secret_settings_value_never_enters_the_file_row` plus this crate's
/// `the_daemons_real_effective_rows_cross_the_wire` daemon test, whose planted secret must never
/// reach the serialized payload). The CLI's ENV-REGISTRY half — several
/// hundred rows whose credential grid discloses `<set>`/`<unset>` per venue key, an enumeration
/// of which venues hold live credentials — is deliberately NOT served. What remains is paths,
/// addresses and flags: the same disclosure class as the identity block and snapshot an Observe
/// peer already reads. Read-only, so no audit record either — the audit trail records COMMANDS,
/// and this verb executes nothing (same treatment as `Snapshot`/`StrategyStatus`).
#[derive(Clone, Debug)]
pub struct SettingsShowSource {
    /// `<project>/settings` as the daemon's ONE boot walk resolved it (`vike_boot::Booted`'s
    /// `settings_dir`); `None` = no project above the working directory (every row reports its
    /// compiled-in default — an honest answer, loudly rendered).
    pub settings_dir: Option<std::path::PathBuf>,
    /// The daemon's startup `std::env::vars()` sweep, owned by the binary (the same map its own
    /// settings load consumed), so the env layer here can never disagree with the one the daemon
    /// booted on.
    pub env: std::collections::HashMap<String, String>,
    /// The HOT-APPLY seam (REQ-7 v2): the queue whose other end the daemon's summary tick drains
    /// ([`crate::hot_reload`]). `None` — a daemon that wired no tick-side applier, and every
    /// test fixture that predates the seam — keeps v1 behaviour: every accepted write answers
    /// restart-to-apply.
    pub hot: Option<crate::hot_reload::HotApplyHandle>,
}

impl SettingsShowSource {
    /// The durable CHANGE JOURNAL for this daemon's project —
    /// `<settings_dir>/state/changes` — or `None` when the boot walk found no project.
    ///
    /// Derived from [`SettingsShowSource::settings_dir`], the SAME already-resolved directory the
    /// settings write itself lands in, rather than from a fresh walk. That is
    /// `vike_model::state_path::user_data_dir_beside`'s rule and it is load-bearing here: the bare
    /// walk is `$VIKE_SETTINGS_DIR`-blind, all three shipped units set that variable precisely so
    /// the answer stops depending on `WorkingDirectory=`, and a journal resolved a second way would
    /// describe one project's ceiling while sitting in another project's folder.
    ///
    /// `None` — no project above the working directory — is the same honest degradation every row
    /// of the `SettingsShow` reply already makes: nothing is journalled, and the `tracing` line
    /// remains, exactly as before the journal existed.
    pub fn change_journal(&self) -> Option<vike_model::change_journal::ChangeJournal> {
        use vike_model::change_journal::{ChangeJournal, Proc};
        use vike_model::state_path::STATE_SUBDIR;

        // `Proc::current` reads `current_exe`, so it is resolved ONCE per process rather than per
        // settings write. Neither the value nor the read can change during a run.
        static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
        let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));
        let state = self.settings_dir.as_deref()?.join(STATE_SUBDIR);
        Some(ChangeJournal::in_state_dir(&state, process.clone()))
    }

    /// Build the [`Request::SettingsShow`] reply: describe → the shared row builder → wire rows.
    /// A load failure answers [`Response::Error`] naming the cause.
    fn response(&self) -> Response {
        match vike_config::describe(self.settings_dir.as_deref(), &self.env) {
            Ok(d) => {
                let rows = vike_config::file_rows(&d, None, false)
                    .into_iter()
                    .map(|r| {
                        let read_by = r.read_cell().to_string();
                        WireSettingsRow {
                            section: r.file.to_string(),
                            key: r.key,
                            value: r.value,
                            origin: r.origin,
                            read_by,
                        }
                    })
                    .collect();
                Response::SettingsShow(Box::new(WireSettingsShow {
                    settings_dir: d.settings_dir.map(|p| p.display().to_string()),
                    rows,
                }))
            }
            Err(e) => Response::Error(format!("settings could not be loaded: {e}")),
        }
    }

    /// Lower ONE accepted `WireCommand::SetSetting` onto disk (split-plane REQ-7, write half):
    /// resolve the file, enforce the TYPED-CONFIRM contract for `policy.toml`, and hand the file
    /// mechanics to [`vike_config::set_setting`] — which validates the WOULD-BE file with the
    /// loader before a byte lands (so the write can never produce a file the next boot refuses)
    /// and edits comment-preservingly + atomically. `Err(reason)` is the wire refusal
    /// ([`Response::Error`], via `AcceptError::Refused`); `Ok` carries the audit raw material.
    ///
    /// ## The typed-confirm contract (the author's REQ-7 ratification)
    ///
    /// A write naming `policy.toml` — the risk ceilings — is REFUSED unless `confirm` equals the
    /// EXACT dotted `key` being changed. The client's job is to make the operator TYPE it (never
    /// pre-fill); this arm's job is to refuse anything else, so no client can quietly skip the
    /// ceremony. Missing and mismatched confirms get distinct messages, each naming the expected
    /// spelling. Non-policy files ignore `confirm` entirely.
    ///
    /// ## Restart vs hot-apply (v2)
    ///
    /// The returned `restart_required` is decided by [`crate::hot_reload::classify`] — the
    /// per-key HOT-vs-RESTART table (policy is NEVER hot: the file is refused into `Restart`
    /// before the table is consulted, the sealed-policy doctrine). A HOT key with the seam wired
    /// (`self.hot`) enqueues the apply for the daemon's summary tick and waits, bounded by
    /// [`crate::hot_reload::HOT_APPLY_DEADLINE`], for its verdict: **`false` is returned ONLY
    /// when the tick confirmed the apply executed.** Everything else — a restart-class key, a
    /// hot key on a daemon with no seam wired, an apply failure, a deadline — answers `true`,
    /// which is always the safe direction (the write is on disk; the next boot loads it). This
    /// is the one spot that decides it, beside the write, exactly as the v1 doc promised.
    fn apply_set_setting(
        &self,
        file: &str,
        key: &str,
        value: &str,
        confirm: Option<&str>,
    ) -> Result<(vike_config::SettingsWrite, bool), String> {
        let Some(settings_file) = vike_config::SettingsFile::parse(file) else {
            return Err(vike_config::unknown_file_message(file));
        };
        if settings_file == vike_config::SettingsFile::Policy {
            match confirm {
                None => {
                    return Err(format!(
                        "policy.toml holds this node's risk ceilings — a policy write must carry \
                         the typed confirm: re-send with confirm set to the exact key `{key}`"
                    ));
                }
                Some(c) if c != key => {
                    return Err(format!(
                        "policy confirm mismatch: the confirm must equal the exact key `{key}` \
                         (got `{c}`) — nothing was written"
                    ));
                }
                Some(_) => {}
            }
        }
        let Some(dir) = self.settings_dir.as_deref() else {
            return Err(
                "settings write unavailable: this node resolved no settings directory at boot \
                 (no project above its working directory)"
                    .to_string(),
            );
        };
        let write =
            vike_config::set_setting(dir, settings_file, key, value).map_err(|e| e.to_string())?;
        let restart_required = match crate::hot_reload::classify(settings_file, key) {
            crate::hot_reload::HotClass::Hot { .. } => match &self.hot {
                Some(seam) => !seam.request_apply(key, crate::hot_reload::HOT_APPLY_DEADLINE),
                None => true,
            },
            crate::hot_reload::HotClass::Restart => true,
        };
        Ok((write, restart_required))
    }
}

/// Accept connections forever, handling each on its own thread. `publisher` is cloned per connection
/// (a cheap Arc handle) to register subscribers and answer point-in-time snapshots; `keys` is the
/// server's scoped [`NodeKeys`]; `commands` is the core's [`CommandSink`] (PR-12) — `Some` ⇒ the
/// control path is enabled on this node, `None` ⇒ every `Request::Command` is refused. Both are
/// cloned per connection (like `keys`/`publisher`). `limits` is the server-edge
/// [`ControlLimitsConfig`] — resolved ONCE by the caller (the daemon binary reads the env; audit
/// F13, the settings-registry rule) and FIXED for this server's lifetime, with each accepted
/// connection building its own [`ControlLimits`] token bucket from it. (Deliberate behavior change
/// from the retired per-connection `from_env`: limits no longer hot-reload per accepted connection
/// — that re-read was never a documented feature; a changed limit now lands on daemon restart.)
/// `datahub_advertise` is the REQ-2 datahub advertisement (`config.toml`'s
/// `datahub_advertise_addr`, resolved once by the daemon binary like `limits`): `Some(addr)` ⇒
/// every connection's `Welcome.features` carries `datahub=<addr>` (see `served_features`),
/// `None` ⇒ nothing is advertised — the pre-REQ-2 Welcome, and what every test caller passes.
///
/// Returns only if `listener.incoming()` yields `None` (it does not for a TCP listener), so in
/// practice this runs for the process lifetime. An accept error is logged and the loop continues.
pub fn serve(
    listener: TcpListener,
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<SettingsShowSource>,
    datahub_advertise: Option<String>,
) -> io::Result<()> {
    serve_with_link_policy(
        listener,
        publisher,
        keys,
        commands,
        limits,
        settings,
        datahub_advertise,
        LinkPolicy::PRODUCTION,
    )
}

/// [`serve`] with the per-connection [`LinkPolicy`] as a parameter — the seam the unit tests drive,
/// so "an authed connection outlives the handshake bound" and "an idle subscriber is heartbeaten"
/// are proven at ~1000x scale instead of in five minutes. Crate-private with exactly one production
/// caller ([`serve`], which passes [`LinkPolicy::PRODUCTION`]): two of those three numbers are the
/// server's half of a contract the client crate owns, and a caller that could pick its own would be
/// able to break that contract from one side.
#[allow(clippy::too_many_arguments)] // the seam adds ONE argument to a function whose parameter
// list is already the daemon's whole composition surface; hiding them in a struct would move the
// argument for each one away from the doc that makes it.
fn serve_with_link_policy(
    listener: TcpListener,
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<SettingsShowSource>,
    datahub_advertise: Option<String>,
    policy: LinkPolicy,
) -> io::Result<()> {
    let live = Arc::new(AtomicUsize::new(0));
    // Arc'd ONCE here rather than cloned per connection: the source carries the whole startup env
    // sweep, and every connection reads it immutably.
    let settings = settings.map(Arc::new);
    // Same shape for the REQ-2 advertisement (`config.toml`'s `datahub_advertise_addr`, resolved
    // by the caller — this module reads no settings): fixed for the server's lifetime, shared
    // into every connection's Welcome. See `served_features` for what it advertises.
    let datahub_advertise: Option<Arc<str>> = datahub_advertise.map(Arc::from);
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                // Reserve the slot BEFORE spawning, and release it from the thread's own `ConnSlot`
                // guard — so the count can never be raced past the cap by a burst of accepts, and
                // never leaks if a connection thread unwinds.
                let taken = live.fetch_add(1, Ordering::AcqRel) + 1;
                let slot = ConnSlot(Arc::clone(&live));
                if taken > MAX_CONNECTIONS {
                    // Dropped WITHOUT a reply: writing an error frame would spend a thread and an
                    // allocation on the peer that is already exhausting them, and would answer an
                    // unauthenticated stranger. `slot`/`stream` drop here, freeing the reservation.
                    tracing::warn!(
                        peer = ?stream.peer_addr().ok(),
                        live = MAX_CONNECTIONS,
                        "vike-tradehub node: connection REFUSED — already at MAX_CONNECTIONS; \
                         dropping without a reply"
                    );
                    continue;
                }
                let publisher = publisher.clone();
                let keys = keys.clone();
                let commands = commands.clone();
                let settings = settings.clone();
                let datahub_advertise = datahub_advertise.clone();
                thread::spawn(move || {
                    let _slot = slot;
                    handle_connection(
                        stream,
                        publisher,
                        keys,
                        commands,
                        limits,
                        settings,
                        datahub_advertise,
                        policy,
                    )
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "vike-tradehub node: accept failed, continuing");
            }
        }
    }
    Ok(())
}

/// Mint a fresh 32-byte challenge nonce for one connection (CSPRNG per connection — the anti-replay
/// property: a mac captured off one connection's nonce fails against another's).
fn fresh_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// The default command rate cap (commands/sec) when the rate knob is unset or unparseable.
const DEFAULT_CONTROL_RATE: f64 = 20.0;

/// The server-edge control-limit KNOBS — the pure CONFIG half of [`ControlLimits`], owned by the
/// CALLER (audit F13): the daemon binary resolves both ONCE at startup (see `main.rs`'s
/// `resolve_control_limits`) and hands the result to [`serve`]. Fixed for the server's lifetime;
/// every accepted connection builds its own [`ControlLimits`] token bucket from it.
///
/// The two knobs come from DIFFERENT places, and deliberately so:
///
/// - `max_notional` is a **policy ceiling** — `max_notional_per_order` in
///   `<vike home>/policy.toml`, via `vike_config::Policy`. It has no environment layer at all.
///   ⚠ It was `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` until Phase 5 of the settings-unification design;
///   a ceiling that a shell export or a stale systemd unit can raise is not a ceiling, so the
///   variable was removed and a daemon that still finds it set refuses to start.
/// - `rate_per_sec` stays `VIKE_TRADEHUB_CONTROL_RATE`: it is a throughput knob, not a risk
///   ceiling — raising it cannot place a larger order — so it keeps the normal env layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlLimitsConfig {
    /// Per-order notional ceiling on Submit/Modify. `None` = no size cap (the core `RiskGate` is
    /// still the floor).
    pub max_notional: Option<f64>,
    /// Command rate cap, commands/sec (token bucket; the bucket caps at ~1s of rate).
    pub rate_per_sec: f64,
}

impl Default for ControlLimitsConfig {
    /// No policy ceiling + [`DEFAULT_CONTROL_RATE`] commands/sec — exactly what
    /// [`Self::from_policy`] resolves with no `policy.toml` and the rate variable unset.
    fn default() -> Self {
        ControlLimitsConfig { max_notional: None, rate_per_sec: DEFAULT_CONTROL_RATE }
    }
}

impl ControlLimitsConfig {
    /// Combine the resolved POLICY ceiling with the RAW rate value (`None` = no ceiling / variable
    /// unset) — both already obtained by the caller, so the binary owns the I/O and this stays a
    /// pure, unit-testable resolver.
    ///
    /// `max_notional_per_order` arrives already typed and already validated (`vike_config` rejects
    /// a non-positive or non-finite ceiling when loading `policy.toml`, naming the file and key);
    /// the guard here is the belt to that braces, for a caller that built a `Policy` some other
    /// way — `0.0` would refuse every order, a silent halt. The rate keeps the old string
    /// semantics exactly: trimmed `f64` parse, garbage/non-positive treated as unset, defaulting
    /// to [`DEFAULT_CONTROL_RATE`].
    ///
    /// ⚠ Was `from_values(Option<&str>, Option<&str>)`, whose first argument was
    /// `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` — see [`ControlLimitsConfig`]'s doc for why that
    /// variable no longer exists.
    pub fn from_policy(max_notional_per_order: Option<f64>, rate_per_sec: Option<&str>) -> Self {
        let max_notional = max_notional_per_order.filter(|n| n.is_finite() && *n > 0.0);
        let rate_per_sec = rate_per_sec
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|r| *r > 0.0)
            .unwrap_or(DEFAULT_CONTROL_RATE);
        ControlLimitsConfig { max_notional, rate_per_sec }
    }
}

/// Server-side control-command limits — DEFENSE-IN-DEPTH beyond the scope key + the core `RiskGate`:
/// a per-order NOTIONAL ceiling and a command RATE limiter, enforced at the ONE edge every remote
/// command funnels through ([`accept_command`], which the TCP [`Request::Command`] arm AND the
/// `telegram` channel both call). So even a leaked `VIKE_TRADEHUB_CONTROL_KEY` — or any
/// non-GUI client — cannot place an arbitrarily large order or flood the core. Per-SURFACE (control
/// connections are few; each connection, and the Telegram poller, gets its own bucket), built from
/// the ONE [`ControlLimitsConfig`] the binary resolved at startup and [`serve`] carries.
pub struct ControlLimits {
    /// Reject a Submit/Modify whose `|qty| * price` exceeds this. `None` = no size cap.
    max_notional: Option<f64>,
    rate_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl ControlLimits {
    /// A fresh per-surface limiter over `cfg` — the token bucket starts full (`rate_per_sec`).
    pub fn new(cfg: ControlLimitsConfig) -> Self {
        ControlLimits {
            max_notional: cfg.max_notional,
            rate_per_sec: cfg.rate_per_sec,
            tokens: cfg.rate_per_sec,
            last_refill: Instant::now(),
        }
    }

    /// Vet one command before it reaches the core. `Some(reason)` ⇒ REFUSE (surfaced as a
    /// `Response::Error`); `None` ⇒ allow. The rate token is consumed on EVERY command; the notional
    /// cap applies ONLY to order-INCREASING verbs that carry a price (Submit / Modify) —
    /// Cancel/Flatten/MassCancel/MarketExit/SetTradingState are risk-reducing and never size-capped.
    pub fn vet(&mut self, cmd: &WireCommand) -> Option<String> {
        if !self.take_token() {
            return Some(format!(
                "rate limited: control commands capped at {}/s on this node",
                self.rate_per_sec
            ));
        }
        self.notional_reason(cmd)
    }

    /// The NOTIONAL-cap check in isolation — no rate token consumed, `&self`. Shared by the executing
    /// [`Self::vet`] (which consumes a token first) and the read-only [`Self::preview_vet`].
    /// `Some(reason)` ⇒ would refuse; `None` ⇒ passes (no cap set, nothing to size, or within cap).
    fn notional_reason(&self, cmd: &WireCommand) -> Option<String> {
        let max = self.max_notional?;
        // NOTIONAL IS A MAGNITUDE — `vike_model::order_notional` is the workspace definition and it
        // takes `.abs()` of every factor. This site used to abs the QTY only, so a NEGATIVE price
        // yielded a negative notional, `n > max` could never trip, and the ceiling was bypassed by a
        // sign alone. (The core `RiskGate` never had this bug: it routes through `order_notional`.)
        let notional = match cmd {
            WireCommand::Submit(r) => r.price.map(|p| r.qty.abs() * p.abs()),
            WireCommand::Modify { new_qty: Some(q), new_price: Some(p), .. } => {
                Some(q.abs() * p.abs())
            }
            // ⚠ A qty-RAISING modify that names NO price is UNCHECKABLE at this edge, so it is
            // REFUSED rather than waved through. This shape used to fall into `_ => None` — "no
            // price to size, therefore fine" — which let `/modify <coid> qty=<huge>` walk past the
            // node's one order-size ceiling completely: place a small in-cap order, then modify it
            // up. This edge holds no order registry and no book, so it cannot resolve the resting
            // price needed to size the projected order; "I cannot evaluate this ceiling" must never
            // render as "the ceiling passed". The operator's fix is to name the price, which makes
            // the command checkable. The core `RiskGate` DOES resolve the resting terms and is the
            // enforcing gate regardless — this is the defense-in-depth edge, now failing CLOSED.
            WireCommand::Modify { new_qty: Some(q), new_price: None, .. } => {
                return Some(format!(
                    "modify to qty {q} names no price, so its notional cannot be checked against \
                     the node's policy ceiling max_notional_per_order {max:.2} — re-send the \
                     modify with an explicit price"
                ));
            }
            // VETTING DECISION (split-plane B4): the notional cap does NOT apply to `UpdateParams`.
            // A params update is not an order — it carries no qty×price to size, and the policy key
            // this ceiling enforces (`max_notional_per_order`) is defined over ONE ORDER's
            // notional. This is not the priceless-modify hole above wearing a new verb: that shape
            // names a CONCRETE projected order this edge merely cannot price, whereas a re-tune
            // names none — every order a re-tuned strategy later emits still passes the core
            // `RiskGate` and the mount's mandatory live risk budget (`ProfileRisk`), which are the
            // enforcing gates for strategy-originated size. The rate token in `vet` DOES apply
            // (consumed for every command — a flood of re-tunes is still a flood).
            WireCommand::UpdateParams { .. } => None,
            // VETTING DECISION (split-plane B5): the mount verbs are not orders either — a mount
            // request carries no qty×price to size, and every order the mounted strategy later
            // emits passes the core `RiskGate` + the mount's live risk budget, the enforcing gates
            // for strategy-originated size (the `UpdateParams` argument verbatim). The unmount is
            // risk-REDUCING (it cancels the mount's resting orders). The rate token in `vet` still
            // applies to both.
            WireCommand::MountStrategy { .. } | WireCommand::UnmountStrategy { .. } => None,
            // VETTING DECISION (split-plane REQ-7): the notional cap does NOT apply to
            // `SetSetting` — a settings write is not an order and carries no qty×price to size,
            // and the one policy key this ceiling enforces (`max_notional_per_order`) is defined
            // over ONE ORDER's notional (the `UpdateParams` argument verbatim). Note the
            // direction that DOES matter is already closed elsewhere: a settings write that
            // RAISES the ceiling itself faces the typed-confirm contract at the acceptance arm,
            // and lands restart-to-apply — this running node's `ControlLimits` keeps its
            // boot-time cap regardless. The rate token in `vet` still applies (a flood of writes
            // is still a flood).
            WireCommand::SetSetting { .. } => None,
            _ => None,
        };
        if let Some(n) = notional
            && n > max
        {
            // Names the SETTING, not a variable: the ceiling is `max_notional_per_order` in
            // the node's `policy.toml` (settings unification, Phase 5) and telling the caller
            // to export something would send them looking for a knob that no longer exists.
            return Some(format!(
                "order notional {n:.2} exceeds the node's policy ceiling \
                     max_notional_per_order {max:.2}"
            ));
        }
        None
    }

    /// The DRY-RUN verdict for the Preview verb: the same edge policy [`Self::vet`] enforces, but
    /// WITHOUT consuming a rate token — a preview is read-only and must not drain the command budget,
    /// nor be rate-limited into uselessness. Only the deterministic notional cap is evaluated; the
    /// transient rate limiter is deliberately not assessed. `Some(reason)` ⇒ would refuse; `None` ⇒
    /// would pass.
    pub fn preview_vet(&self, cmd: &WireCommand) -> Option<String> {
        self.notional_reason(cmd)
    }

    /// Token-bucket refill+take. The bucket caps at ~1s of rate (a short burst, never an unbounded
    /// backlog after an idle period).
    fn take_token(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.rate_per_sec.max(1.0));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Drive one connection: handshake, then serve. Never panics the caller — a fault logs and returns,
/// dropping the socket. The phases:
///
/// 1. HANDSHAKE (read-timeout-bounded): `Hello` -> `Welcome{ nonce }`, then `Auth` -> verify. A
///    bad/absent-key mac (either scope), or a `Control` scope on a control-less node, -> `AuthDenied`
///    and close. Any session verb BEFORE `AuthOk` (an unauthenticated `Subscribe`/`Snapshot`/
///    `Command`) -> `AuthDenied` and close. The granted [`Scope`] is carried into (2).
/// 2. AUTHED SESSION: request/response until the client `Subscribe`s (Observe only). `Snapshot` ->
///    one frame; `Ping` -> `Pong`; `Command` -> lowered to the core when the peer is `Control` AND a
///    `CommandSink` is present (else refused). `Subscribe` transitions to (3).
/// 3. PUSH (this thread becomes the writer): drain the subscriber mailbox, `write_all` each frame,
///    until a write error, the peer closing, or the publisher shutting down.
///
/// # What an AUTHENTICATED connection experiences when it is idle
///
/// Nothing. The handshake read timeout is REPLACED — not merely cleared — the instant a scope is
/// granted, with `policy.authed_idle_timeout`
/// ([`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`], `None`): an authed peer may stay quiet
/// for as long as it likes and this server will not close its connection. A control client holds
/// one connection for the life of its process and speaks only when its operator does, so any finite
/// bound here is a timer that eventually kills a working link — which is exactly what the 300 s
/// handshake bound did when it was left in place after `AuthOk`. The full argument, including what
/// the removal costs and why a bigger number is not an answer, is on that constant.
///
/// The UNAUTHENTICATED bound is untouched: a peer that opens a socket and says nothing still gets
/// [`HANDSHAKE_READ_TIMEOUT`] and no more, which is the property [`MAX_CONNECTIONS`] leans on.
#[allow(clippy::too_many_arguments)] // see `serve_with_link_policy`'s note — the policy is one more
// per-connection input on a function that already takes the daemon's whole composition surface.
fn handle_connection(
    mut stream: TcpStream,
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<Arc<SettingsShowSource>>,
    datahub_advertise: Option<Arc<str>>,
    policy: LinkPolicy,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-tradehub node: connection opened");

    if let Err(e) = stream.set_read_timeout(Some(policy.handshake_read_timeout)) {
        tracing::warn!(?peer, error = %e, "vike-tradehub node: could not set read timeout, closing");
        return;
    }

    // --- Phase 1: handshake (carries the granted scope forward) ---
    let scope = match run_handshake(&mut stream, &keys, peer, datahub_advertise.as_deref()) {
        HandshakeOutcome::Authed(scope) => scope,
        HandshakeOutcome::Closed => {
            tracing::info!(?peer, "vike-tradehub node: connection closed (handshake refused)");
            return;
        }
    };

    // THE REPLACEMENT (see this function's doc). The bound above exists to stop an
    // UNAUTHENTICATED peer pinning this thread; the peer has now proved possession of a scoped
    // key, so it gets the authed policy instead. A failure here is fatal to the connection rather
    // than ignored: carrying on would leave the handshake's bound in force, which is precisely the
    // defect — an authed connection silently living under a stranger's timer.
    if let Err(e) = stream.set_read_timeout(policy.authed_idle_timeout) {
        tracing::warn!(
            ?peer, ?scope, error = %e,
            "vike-tradehub node: could not apply the authed idle policy, closing"
        );
        return;
    }

    // WHO this connection authenticated AS — the stable, non-secret fingerprint of the key whose
    // mac verified ([`NodeKeys::key_id`]). Resolved ONCE here, immediately after the handshake,
    // because this is the only place the granted scope and the server's keys are both in hand; it
    // is what the change journal records as the actor, since the daemon authenticates a KEY and
    // there are no human accounts in this system. `None` is unreachable on this path (the scope was
    // granted, so its key is non-empty) and is carried honestly rather than unwrapped — an absent
    // key must record NO id, never an invented one.
    let key_id = keys.key_id(scope);

    // Control-command limits (a no-op for an Observe peer, which never reaches the Command arm):
    // this connection's own token bucket over the server-lifetime config.
    let mut limits = ControlLimits::new(limits);

    // --- Phase 2: authed Observe session, until Subscribe ---
    loop {
        let body = match read_frame_raw(&mut stream) {
            Ok(b) => b,
            Err(e) => {
                log_read_end(&e, peer);
                return;
            }
        };
        let request = match serde_json::from_slice::<Request>(&body) {
            Ok(r) => r,
            Err(e) => {
                // A well-framed but undecodable body is a bad request, not a bad connection.
                let _ = write_frame(
                    &mut stream,
                    &Response::Error(format!("unrecognized/undecodable request: {e}")),
                );
                continue;
            }
        };

        match request {
            // Transition to push mode — this thread becomes the writer for the rest of the session.
            Request::Subscribe { topics } => {
                tracing::info!(?peer, ?topics, "vike-tradehub observe: subscribed (push mode)");
                run_push_writer(&mut stream, &publisher, peer, policy.observe_heartbeat);
                return;
            }
            // A point-in-time snapshot off the publisher's current cell.
            Request::Snapshot => {
                let frame = publisher.snapshot_frame_now();
                if let Err(e) = write_all_flush(&mut stream, &frame) {
                    tracing::warn!(?peer, error = %e, "vike-tradehub observe: snapshot write fault, closing");
                    return;
                }
            }
            Request::Ping => {
                if write_frame(&mut stream, &Response::Pong).is_err() {
                    return;
                }
            }
            // Control path (PR-12): only a Control-authenticated peer, on a node with a CommandSink,
            // may lower a command into the core. Every other case is refused, never folded.
            Request::Command { cmd: wire_cmd, reason } => {
                // (a) Scope gate: an Observe peer is read-only and can never command.
                if scope != Scope::Control {
                    let _ = write_frame(
                        &mut stream,
                        &Response::AuthDenied {
                            reason: "read-only: authenticated as Observe".into(),
                        },
                    );
                    continue;
                }
                // (b) Sink gate: control may be authenticated but not ENABLED on this node.
                let Some(sink) = &commands else {
                    let _ = write_frame(
                        &mut stream,
                        &Response::Error("control not enabled on this node".into()),
                    );
                    continue;
                };
                // (c) The SHARED acceptance path — the same `accept_command` the Telegram channel
                // calls: edge limits (rate token + notional cap), rationale sanitization, lowering,
                // the single-writer lane, and the audit record, in that ONE order for every
                // surface. This connection's own `limits` bucket and TCP `peer` are what make it
                // this surface's call; everything else is common by construction. `settings` is
                // the REQ-7 write lowering's source (the same boot-threaded handle the
                // `SettingsShow` arm reads) — a `SetSetting` on a source-less server refuses
                // inside, every other command ignores it.
                match accept_command(
                    wire_cmd,
                    reason.as_deref(),
                    &mut limits,
                    sink,
                    settings.as_deref(),
                    peer,
                    key_id.as_deref(),
                ) {
                    Ok(Accepted::Coid(coid)) => {
                        if write_frame(&mut stream, &Response::Ack { coid }).is_err() {
                            return;
                        }
                    }
                    // The REQ-7 settings write: its acceptance is not an Ack (nothing entered
                    // the core, no coid) — the reply carries the restart-to-apply signal.
                    Ok(Accepted::SettingsWritten { restart_required }) => {
                        if write_frame(&mut stream, &Response::SettingsWritten { restart_required })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = write_frame(&mut stream, &Response::Error(e.message()));
                        // Only a `Gone` (the core's lane closed) ends the connection — a refusal or
                        // a transient `Busy` leaves this peer free to try again, exactly as before.
                        if e.is_fatal() {
                            return;
                        }
                    }
                }
            }
            // Preview path (v3): a Control peer asks what a command WOULD do WITHOUT executing it.
            // Server-authoritative dry-run — READ-ONLY: nothing is lowered, nothing reaches the core,
            // no order is placed, no rate token is consumed, and it is NOT audit-logged as an executed
            // command. Only the deterministic edge policy (the notional cap) is evaluated and returned.
            Request::Preview(wire_cmd) => {
                // Scope gate: an Observe peer is read-only and can never command — nor preview a
                // command (same gate as `Request::Command`).
                if scope != Scope::Control {
                    let _ = write_frame(
                        &mut stream,
                        &Response::AuthDenied {
                            reason: "read-only: authenticated as Observe".into(),
                        },
                    );
                    continue;
                }
                let reason = limits.preview_vet(&wire_cmd);
                tracing::debug!(
                    ?peer,
                    kind = command_kind(&wire_cmd),
                    accepted = reason.is_none(),
                    "vike-tradehub control: preview (dry-run, nothing executed)"
                );
                if write_frame(
                    &mut stream,
                    &Response::Preview { accepted: reason.is_none(), reason },
                )
                .is_err()
                {
                    return;
                }
            }
            // STRATEGY-level read (split-plane B4): what is this node running? Post-auth under
            // EITHER scope — it is read-only (identity + rendered params, data every pushed
            // snapshot already carries), so Observe suffices, exactly like `Snapshot`. Answered
            // from the publisher's process-static identity block plus its per-mount rows
            // (split-plane I10): a `[[mounts]]` daemon publishes one `WireMountRow` per mount
            // (`publish::spawn_with_mounts`); a publisher spawned without rows (the mount-less
            // `publish::spawn`, every pre-I10 caller) falls back to deriving the single row from
            // the identity block — byte-identical to the pre-I10 answer.
            //
            // ⚠ THE FALLBACK'S `live: id.live` IS THE WEAKER ANSWER, and it is kept only because
            // an identity-only publisher has no better one. `WireNodeIdentity::live` is the
            // PROCESS-wide `flags.tradehub_live` gate; `WireMountRow::live` is documented as "this
            // MOUNT trades LIVE", a per-VENUE question the gate cannot answer — a mount whose venue
            // has no credentials, or one declared `data_only = true`, is gated LIVE and executes on
            // the paper book. `vike-tradehub`'s own daemon no longer takes this path at all: `main`
            // publishes a row per mount (single-mount daemons included) whose `live` comes from
            // `build_node`'s arming record. Do not "simplify" by dropping those rows again.
            Request::StrategyStatus => {
                let resp = match publisher.identity() {
                    Some(id) => {
                        let mut mounts = publisher.mounts();
                        if mounts.is_empty() {
                            mounts = vec![WireMountRow {
                                strategy: id.strategy.clone(),
                                params: id.params.clone(),
                                live: id.live,
                            }];
                        }
                        Response::StrategyStatus(Box::new(WireStrategyStatus {
                            effective_params: id.params.clone(),
                            identity: id,
                            mounts,
                        }))
                    }
                    // An identity-less publisher (possible through `publish::spawn(.., None)`;
                    // the shipped daemon always passes an identity) has no mounted truth to
                    // report — an honest error, never a fabricated empty status.
                    None => Response::Error(
                        "strategy status unavailable: this node publishes no identity block".into(),
                    ),
                };
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // SETTINGS read (split-plane REQ-7, read half): the node's effective settings-file
            // rows, from the boot-threaded [`SettingsShowSource`]. Post-auth under EITHER scope —
            // Observe suffices, and the argument is [`SettingsShowSource`]'s doc: the payload is
            // the FILES half only, redacted ON CONSTRUCTION in the shared `vike_config::show`
            // builder (the env-registry half, whose credential grid enumerates which venues hold
            // keys, is never served), leaving paths/addresses/flags — the disclosure class an
            // Observe peer already reads off the identity block. Read-only ⇒ no audit record,
            // exactly like `Snapshot`/`StrategyStatus`.
            Request::SettingsShow => {
                let resp = match &settings {
                    Some(src) => src.response(),
                    // A server constructed without a source (possible through `serve(.., None)`;
                    // the shipped daemon always passes one) has no settings truth to report — an
                    // honest error, never a fabricated empty table (the `StrategyStatus`
                    // identity-less shape).
                    None => Response::Error(
                        "settings unavailable: this node was started without a settings source"
                            .into(),
                    ),
                };
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // Handshake verbs after AuthOk are a protocol error, but not fatal.
            Request::Hello { .. } | Request::Auth { .. } => {
                let _ = write_frame(
                    &mut stream,
                    &Response::Error("already authenticated; handshake is complete".into()),
                );
            }
        }
    }
}

/// The result of the handshake phase.
enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed to the session with it.
    Authed(Scope),
    /// The handshake was refused (or a transport error) — the connection must close.
    Closed,
}

/// Run the `Hello` -> `Welcome{ nonce }` -> `Auth` -> verify handshake. Verifies the presented mac
/// against the REQUESTED scope's key (scope-generic): [`Scope::Observe`] is granted whenever its key
/// verifies; [`Scope::Control`] additionally requires the node to HOLD a control key (absent ⇒
/// control disabled ⇒ refused before any key is consulted). Any session verb arriving before auth is
/// refused too (an unauthenticated `Subscribe` never registers).
fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    peer: Option<std::net::SocketAddr>,
    datahub_advertise: Option<&str>,
) -> HandshakeOutcome {
    // Read the opener; it MUST be Hello. PRE-AUTH, so it is read under the small
    // `HANDSHAKE_MAX_FRAME_LEN` ceiling rather than the 64 MiB `MAX_FRAME_LEN` — an unauthenticated
    // peer must not be able to name an allocation size.
    let body = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(e) => {
            log_read_end(&e, peer);
            return HandshakeOutcome::Closed;
        }
    };
    let opener = match serde_json::from_slice::<Request>(&body) {
        Ok(r) => r,
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Hello (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };
    let client_version = match opener {
        Request::Hello { proto_version } => proto_version,
        // Any other verb before the handshake completes is unauthenticated — refuse it. This is where
        // an unauthenticated Subscribe/Snapshot/Command is denied and the connection dropped.
        _ => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // Send the challenge. We always answer Hello with our version + a fresh nonce; a client on a
    // different protocol version cannot forge a valid mac anyway (the version is signed), so a skew
    // fails cleanly at verify rather than needing a special branch here.
    let nonce = fresh_nonce();
    if write_frame(
        stream,
        &Response::Welcome {
            proto_version: NODE_PROTO_VERSION,
            nonce,
            features: served_features(datahub_advertise),
        },
    )
    .is_err()
    {
        return HandshakeOutcome::Closed;
    }
    if client_version != NODE_PROTO_VERSION {
        tracing::debug!(
            ?peer,
            client_version,
            server_version = NODE_PROTO_VERSION,
            "vike-tradehub observe: client protocol version differs; auth will fail on the signed version"
        );
    }

    // Read the Auth answer — still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(e) => {
            log_read_end(&e, peer);
            return HandshakeOutcome::Closed;
        }
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

    // Control must be ENABLED on this node: a `Control` request on a node with no control key is
    // refused WITHOUT consulting it — the closed-gate shape the observe path uses for an absent key.
    // (The `CommandSink` presence is enforced per-command in `handle_connection`; the key is the auth
    // gate here.) When `VIKE_TRADEHUB_CONTROL` is off the daemon zeroes the control key, so this is
    // also the belt to that suspenders.
    if scope == Scope::Control && !keys.has(Scope::Control) {
        // WARN, not info, and the level is the whole point: `deploy/vike-tradehub.service` sets
        // `Environment=VIKE_LOG_FILE_LEVEL=warn`, so an `info` refusal never reaches the log file
        // on a live box. `crate::audit::FILE_PIN` pins the ACCEPTED half of the control trail onto
        // disk; without this the file would hold a complete record of every command that was taken
        // and no evidence whatever that anyone was ever turned away — a shape that is worse than
        // the old one, because the file now LOOKS authoritative. A refused handshake is also a
        // fault rather than an ordinary event, which is the argument the audit line itself cannot
        // make (see `crate::audit::FILE_PIN`'s doc for why THAT one stayed at `info` and took a
        // pin instead). Rate: one line per refused handshake, bounded by the connection rate.
        tracing::warn!(
            ?peer,
            "vike-tradehub node: control auth refused — control disabled on this node"
        );
        let _ = write_frame(
            stream,
            &Response::AuthDenied { reason: "control disabled on this node".into() },
        );
        return HandshakeOutcome::Closed;
    }

    // Verify the mac against the REQUESTED scope's key (constant-time). An absent key is a closed gate
    // — `verify` against an empty key never accepts a real mac — so `keys.has(scope)` guards it.
    let key = keys.key_for(scope);
    let mac_ok = keys.has(scope) && auth::verify(key, &nonce, NODE_PROTO_VERSION, scope, &mac);
    if mac_ok {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        tracing::info!(?peer, ?scope, "vike-tradehub node: authenticated");
        HandshakeOutcome::Authed(scope)
    } else {
        // WARN for the same reason as the refusal above: at the shipped `VIKE_LOG_FILE_LEVEL=warn`
        // an `info` line never reaches the file, and "somebody presented a bad mac for the Control
        // scope, repeatedly" is exactly the sentence a post-incident review needs to find there.
        tracing::warn!(?peer, ?scope, "vike-tradehub node: auth denied (bad mac / no key)");
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

/// The push loop: register with the publisher, then drain the subscriber mailbox and `write_all` each
/// framed snapshot to the socket. Exits on a write error (peer gone / broken pipe), the publisher
/// shutting down ([`Recv::Closed`]), and deregisters automatically when the [`Subscription`] drops.
///
/// This thread STOPS reading the socket once here — a stalled client blocks only this one thread in
/// `write_all`, never the publisher (which merely keeps enqueueing into this connection's bounded,
/// drop-oldest mailbox). Disconnect is detected on the next push write failing.
///
/// # The HEARTBEAT: why a quiet stream must still say something
///
/// After `Subscribe` this connection is ONE-WAY — the client sends nothing more — so when the node
/// has nothing to publish, NOTHING crosses the socket in either direction. A client therefore had
/// no way at all to tell an idle node from a link that died in silence (a sleeping laptop, a VPN
/// re-key, an `ssh -L` whose local socket stays open), and `RemoteCoreHandle::is_connected` stayed
/// `true` for hours while `vike-cli mcp` served the pre-drop frame as live. Every `heartbeat` of
/// quiet, this writer now sends one [`Response::Pong`], and the client deadlines its read at three
/// of them (`vike_tradehub_client::liveness`).
///
/// **`Pong`, not a re-sent snapshot frame, and the choice is load-bearing.** Re-publishing the last
/// frame would work as a keepalive and needs no new variant either — but it puts a payload on the
/// wire that a consumer can mistake for state. A repeated `seq` is a repeated FACT, and every
/// reader of this stream would have to be trusted, forever, to treat it as liveness rather than as
/// change (`vike-app-core`'s observe bridge gates on `seq != last_seq`, `mcp`'s snapshot tool waits
/// for `seq > 0` — both fine today, neither structurally guaranteed). `Pong` cannot be mistaken for
/// anything: `RemoteCoreHandle`'s receive loop has always dropped every non-`SnapshotFrame` reply
/// on the floor, so this is invisible to an old client by CONSTRUCTION rather than by convention.
/// It is also the smallest frame the protocol has, and it is not a wire change at all — the variant
/// has existed since PR-10 — which is what lets the capability be advertised
/// ([`FEATURE_OBSERVE_HEARTBEAT`]) instead of forcing a `NODE_PROTO_VERSION` bump the signed auth
/// mac would break every running node over.
///
/// A BUSY node emits none of these: the elapsed timer is reset by every real frame, and a node
/// publishing at the core's ≥16 ms cadence never goes `heartbeat` without one. The cost is on an
/// IDLE node only, and it is four tiny frames a minute per subscriber.
///
/// ⚠ Second effect, worth having on purpose: this is also how a stalled or vanished SUBSCRIBER is
/// eventually noticed. Before it, a writer with nothing to send parked in `recv_timeout` forever
/// and the connection thread outlived the client that owned it; now a dead peer's socket fails a
/// heartbeat write (once the send buffer fills) and the thread exits.
fn run_push_writer(
    stream: &mut TcpStream,
    publisher: &PublisherHandle,
    peer: Option<std::net::SocketAddr>,
    heartbeat: Duration,
) {
    let subscription = publisher.subscribe();
    // Since the last thing this connection put on the wire — a real frame or a heartbeat. Starts
    // now, so a subscriber that is handed its first frame immediately does not also get a beat.
    let mut last_write = Instant::now();
    loop {
        match subscription.recv_timeout(WRITER_RECV_TIMEOUT) {
            Recv::Frame(bytes) => {
                if let Err(e) = write_all_flush(stream, &bytes) {
                    tracing::info!(?peer, error = %e, "vike-tradehub observe: push write ended, closing");
                    return; // dropping `subscription` deregisters from the publisher
                }
                last_write = Instant::now();
            }
            // No new frame. Re-check the mailbox's closed state, and — if the socket has been
            // silent for a whole heartbeat — prove the link is alive rather than leaving the client
            // unable to distinguish this from a corpse.
            Recv::Timeout => {
                if last_write.elapsed() >= heartbeat {
                    if let Err(e) = write_frame(stream, &Response::Pong) {
                        tracing::info!(?peer, error = %e, "vike-tradehub observe: heartbeat write ended, closing");
                        return;
                    }
                    last_write = Instant::now();
                }
            }
            Recv::Closed => {
                tracing::info!(?peer, "vike-tradehub observe: publisher stopped, closing push");
                return;
            }
        }
    }
}

/// Write pre-framed bytes and flush — the raw-bytes counterpart to `write_frame`. The publisher
/// already serialized these snapshot frames ONCE, so they are written verbatim (not re-encoded).
fn write_all_flush(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes)?;
    stream.flush()
}

/// The verbs this node server answers, advertised in `Welcome.features`. (Control is gated by the
/// server's `NodeKeys`/`CommandSink`, not by an advertised feature string, so it stays off this list.)
///
/// `datahub_advertise` is the REQ-2 advertisement — `config.toml`'s `datahub_advertise_addr`,
/// resolved by the daemon binary (this module reads no settings; audit F13): `Some(addr)` appends
/// the ONE value-carrying entry, `datahub=<addr>` (spelled by
/// `vike_tradehub_client::proto::datahub_feature`, the round-trip authority), naming where a
/// client should DIAL the datahub this backend fronts. `None` — the default, and every test
/// caller — advertises nothing: the list is byte-identical to the pre-REQ-2 one.
fn served_features(datahub_advertise: Option<&str>) -> Vec<String> {
    let mut features = vec![
        "observe".to_string(),
        "subscribe".to_string(),
        "snapshot".to_string(),
        // The server-authoritative dry-run verb (v3): a Control peer can ask what a command WOULD do
        // without executing it. Advertised (unlike control itself) because it is read-only.
        "preview".to_string(),
        // The STRATEGY verbs (split-plane B4): the `StrategyStatus` read + the `UpdateParams`
        // command. Advertised — rather than version-bumped — because `NODE_PROTO_VERSION` is folded
        // into the signed auth MAC (a bump breaks the handshake against every running node); a
        // client that does not see this string refuses the verbs CLIENT-side, sending nothing.
        FEATURE_STRATEGY_VERBS.to_string(),
        // The SETTINGS read verb (split-plane REQ-7, read half): `SettingsShow`. Same
        // feature-not-version design as the strategy verbs. Advertised unconditionally — a node
        // started without a [`SettingsShowSource`] still decodes the request and answers an
        // honest `Response::Error`, which is a better answer than a capability that flickers
        // with construction details (and mirrors `StrategyStatus` on an identity-less publisher).
        FEATURE_SETTINGS_SHOW.to_string(),
        // The SETTINGS WRITE verb (split-plane REQ-7, write half): `WireCommand::SetSetting`.
        // Its OWN capability (a read-half node advertises `settings-show` yet cannot decode the
        // variant — the mount-verbs argument). Advertised unconditionally, like its read
        // sibling: a server started without a settings source still decodes the command and
        // refuses it honestly inside `accept_command`.
        FEATURE_SETTINGS_WRITE.to_string(),
        // The runtime MOUNT verbs (split-plane B5): `MountStrategy`/`UnmountStrategy`. A SEPARATE
        // capability from `strategy-verbs` — a B4-era node advertises that string yet cannot
        // decode these variants, so the new verbs need their own (see `FEATURE_MOUNT_VERBS`).
        FEATURE_MOUNT_VERBS.to_string(),
        // The observe HEARTBEAT (`run_push_writer`): this node writes an idle `Response::Pong` on
        // a subscribed stream that has had nothing to say for `liveness::OBSERVE_HEARTBEAT`.
        // Unlike every string above it this advertises no VERB — nothing new may be sent — it
        // licenses a client-side DEADLINE, so a client may treat silence as death. Advertised
        // unconditionally because every connection this server serves is heartbeaten: the
        // capability is a property of the BUILD, not of a construction detail (the
        // `settings-show` argument, and the same reason the `LinkPolicy` seam is test-only —
        // a node that could be configured not to heartbeat while still advertising it would be
        // handing its clients a false promise).
        FEATURE_OBSERVE_HEARTBEAT.to_string(),
    ];
    // The datahub advertisement (split-plane REQ-2) — advertised ONLY when configured, so an
    // unconfigured daemon's Welcome is byte-identical to the pre-REQ-2 one. Advertisement, never
    // proxying: this server serves no data verb; the client dials the advertised address itself.
    if let Some(addr) = datahub_advertise {
        features.push(datahub_feature(addr));
    }
    features
}

/// Why [`accept_command`] refused, and whether the SURFACE must close.
///
/// The three variants are exactly the three non-`Ack` outcomes the TCP arm has always produced;
/// splitting them out (instead of one `String`) is what preserves the "a `Gone` closes the
/// connection, a `Busy`/refusal does not" rule across BOTH surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptError {
    /// Refused at the server edge — the rate limiter, the notional cap, or a command
    /// [`lower_command`] declines to lower (e.g. a submit with no pre-minted client-order-id). The
    /// surface stays open; the caller may send another command.
    Refused(String),
    /// The core's ingest lane was FULL. Transient — retry.
    Busy,
    /// The core's ingest lane is CLOSED (the core is shutting down). Terminal for the surface.
    Gone,
}

impl AcceptError {
    /// The operator-visible text. The `Busy`/`Gone` strings are the verbatim `Response::Error`
    /// bodies the TCP arm sent before this was factored out — do not reword them casually, a
    /// client may key off them.
    pub fn message(&self) -> String {
        match self {
            AcceptError::Refused(msg) => msg.clone(),
            AcceptError::Busy => "core busy, retry".to_string(),
            AcceptError::Gone => "core is shutting down".to_string(),
        }
    }

    /// True when the surface must stop after reporting this (only [`AcceptError::Gone`]).
    pub fn is_fatal(&self) -> bool {
        matches!(self, AcceptError::Gone)
    }
}

impl std::fmt::Display for AcceptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// What [`accept_command`] accepted — the reply-shaping half of its verdict. Almost every command
/// lowers into the core and echoes a coid ([`Response::Ack`]); the ONE exception is the REQ-7
/// settings write, which lands on DISK at the daemon edge (nothing enters the core, there is no
/// coid) and whose caller instead needs `restart_required`
/// ([`Response::SettingsWritten`]). An enum rather than a stringly convention so a surface cannot
/// accidentally ack a settings write as an order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// The command entered the core's single-writer lane; the coid to echo (empty for the
    /// account-wide verbs, exactly as before).
    Coid(String),
    /// A `SetSetting` landed on disk. `restart_required` is the restart-to-apply signal
    /// ([`SettingsShowSource::apply_set_setting`] decides it): `false` ONLY when the key is
    /// hot-safe ([`crate::hot_reload::classify`]) AND the daemon's summary tick confirmed the
    /// apply executed; `true` otherwise — the running node keeps its boot-time value.
    SettingsWritten {
        /// `true` ⇒ the running node keeps its boot-time value for this key until restarted.
        restart_required: bool,
    },
}

impl Accepted {
    /// The coid for surfaces whose reply vocabulary is coid-shaped (the Telegram channel): the
    /// echoed coid, or the account-wide empty string for a settings write.
    pub fn into_coid(self) -> String {
        match self {
            Accepted::Coid(coid) => coid,
            Accepted::SettingsWritten { .. } => String::new(),
        }
    }
}

/// **The ONE acceptance path.** Every remote control SURFACE — the TCP [`Request::Command`] arm and
/// the opt-in `telegram` channel (behind the crate feature of the same name) — funnels through
/// here, so neither can be gated differently from the other by accident.
///
/// In order:
/// 1. [`ControlLimits::vet`] — consumes a rate token, then applies the per-order notional cap.
///    Refusal short-circuits: NOTHING is lowered and nothing is audited.
/// 2. [`audit::sanitize_reason`] — the rationale is remote free text landing in a structured JSON
///    log line, so it is stripped/capped HERE, once, for every surface.
/// 3. [`command_kind`] — read off the wire command BEFORE [`lower_command`] consumes it.
/// 4. [`lower_command`] — into the core's real `Command`/`OrderIntent`.
/// 5. `CommandSink::try_command` — the non-blocking single-writer lane.
/// 6. [`audit::record`] — ONLY on acceptance, exactly as before.
///
/// **The ONE branch: `WireCommand::SetSetting` (REQ-7) replaces steps 4–6** — after the SAME rate
/// token and the SAME sanitizer, it lowers onto DISK instead of into the core
/// ([`SettingsShowSource::apply_set_setting`]: file resolution, the policy typed-confirm, the
/// loader-validated comment-preserving write) and its audit record is
/// [`audit::record_settings_write`], which carries the old→new values. `settings` is that
/// boot-threaded source — `None` (a surface with no settings lowering: the Telegram channel, a
/// server started without one) refuses the verb; every other command ignores the parameter
/// entirely.
///
/// The `reason` never reaches [`lower_command`], so it can never touch `OrderRequest`, the core
/// fold, the journal, or a venue. `peer` is the TCP peer for a socket surface and `None` for a
/// non-socket one (the Telegram channel names its origin inside `reason` instead).
///
/// `key_id` is the caller's authenticated identity — `vike_tradehub_client::auth::NodeKeys`'
/// `key_id`, the stable non-secret fingerprint of the key whose mac verified, resolved once per
/// connection in `handle_connection`. It is the ACTOR the change journal records, because this
/// daemon authenticates a key and never a person. `None` for a surface that authenticates no key
/// (the Telegram channel, whose own identity is its chat id) — recorded as an ABSENT field rather
/// than as an invented one. ⚠ It is an `Option<&str>` next to an `Option<SocketAddr>`, so the two
/// cannot be transposed at a call site the way four adjacent `&str`s could (the hazard
/// [`audit::SettingsWriteAudit`] exists for).
///
/// `Ok` is the [`Accepted`] verdict the surface shapes its reply from ([`Accepted::Coid`] ⇒
/// `Ack`, [`Accepted::SettingsWritten`] ⇒ `SettingsWritten`).
pub fn accept_command(
    cmd: WireCommand,
    reason: Option<&str>,
    limits: &mut ControlLimits,
    sink: &CommandSink,
    settings: Option<&SettingsShowSource>,
    peer: Option<std::net::SocketAddr>,
    key_id: Option<&str>,
) -> Result<Accepted, AcceptError> {
    if let Some(refusal) = limits.vet(&cmd) {
        return Err(AcceptError::Refused(refusal));
    }
    let audit_reason = audit::sanitize_reason(reason);
    let kind = command_kind(&cmd);
    if let WireCommand::SetSetting { file, key, value, confirm } = cmd {
        // The REQ-7 settings write: same gate order as every command (the rate token above, the
        // sanitizer above), then the DISK lowering instead of the core one. NOTE the notional cap
        // is n/a by `notional_reason`'s own vetting decision at its arm.
        let src = settings.ok_or_else(|| {
            AcceptError::Refused(
                "settings write unavailable: this surface serves no settings source".to_string(),
            )
        })?;
        let (write, restart_required) = src
            .apply_set_setting(&file, &key, &value, confirm.as_deref())
            .map_err(AcceptError::Refused)?;
        // The record goes BOTH to the tracing line (console/journald) and to the durable change
        // journal — see `audit`'s module doc for why the log line alone was measured not to survive
        // on a real deployment. The journal is derived from the same boot-resolved settings
        // directory this write just landed in, so the ledger and the file it describes can never
        // resolve to two different projects.
        let journal = src.change_journal();
        audit::record_settings_write(audit::SettingsWriteAudit {
            peer,
            key_id,
            journal: journal.as_ref(),
            now_ms: vike_model::now_ms(),
            write: &write,
            reason: audit_reason.as_deref(),
            // The restart-to-apply bit the peer is about to be told, recorded so the journal
            // answers "was that ceiling actually ARMED" rather than only "was it written".
            outcome: if restart_required {
                vike_model::change_journal::Outcome::AppliedPendingRestart
            } else {
                vike_model::change_journal::Outcome::Applied
            },
        });
        return Ok(Accepted::SettingsWritten { restart_required });
    }
    let (lowered, coid) = lower_command(cmd).map_err(AcceptError::Refused)?;
    match sink.try_command(lowered) {
        Ok(()) => {
            audit::record(peer, kind, &coid, audit_reason.as_deref());
            Ok(Accepted::Coid(coid))
        }
        Err(CommandRejected::Busy) => Err(AcceptError::Busy),
        Err(CommandRejected::Gone) => Err(AcceptError::Gone),
    }
}

/// Lower ONE [`WireCommand`] into the core's real `(Command, coid)` — the TOTAL mapping (every wire
/// variant maps). The returned `coid` is what the [`Response::Ack`] echoes: the client-order-id for
/// order-scoped verbs, empty for the account-wide ones (mass-cancel / flatten / market-exit /
/// trading-state). `Err(msg)` is a request the server refuses to lower — surfaced as
/// [`Response::Error`], never silently dropped.
fn lower_command(wc: WireCommand) -> Result<(Command, String), String> {
    match wc {
        WireCommand::Submit(req) => {
            // SECURITY / idempotency policy: the NETWORK path must pre-mint its own coid. An empty
            // one would let the runtime mint (fine in-process, but a remote peer then has no stable
            // handle to cancel/dedup by), so it is refused rather than lowered.
            if req.client_order_id.trim().is_empty() {
                return Err("remote submit requires a pre-minted client_order_id".to_string());
            }
            let coid = req.client_order_id.clone();
            let order = OrderRequest {
                client_order_id: req.client_order_id,
                venue: req.venue,
                symbol: req.symbol,
                side: req.side,
                qty: req.qty,
                order_type: req.order_type,
                price: req.price,
                trigger_price: req.trigger_price,
                reduce_only: req.reduce_only,
                ..Default::default()
            };
            Ok((Command::Order(OrderIntent::Submit(Box::new(order))), coid))
        }
        WireCommand::Cancel(coid) => Ok((Command::Order(OrderIntent::Cancel(coid.clone())), coid)),
        WireCommand::Modify { client_order_id, new_qty, new_price } => {
            let coid = client_order_id.clone();
            Ok((Command::Order(OrderIntent::Modify { client_order_id, new_qty, new_price }), coid))
        }
        WireCommand::MassCancel { venue, symbol } => {
            Ok((Command::Order(OrderIntent::MassCancel { venue, symbol }), String::new()))
        }
        WireCommand::Flatten { venue, symbol } => {
            Ok((Command::Order(OrderIntent::Flatten { venue, symbol }), String::new()))
        }
        WireCommand::MarketExit { venue } => {
            Ok((Command::Order(OrderIntent::MarketExit { venue }), String::new()))
        }
        WireCommand::SetTradingState(ws) => {
            Ok((Command::SetTradingState(project_wire_trading_state(ws)), String::new()))
        }
        // STRATEGY-level write (split-plane B4): the wire carries the core's own externally-tagged
        // `StrategyParams` JSON (delegated, not mirrored — see the variant's doc in the client
        // crate), so lowering IS a deserialize into the journaled schema. An undecodable payload is
        // REFUSED here — surfaced as `Response::Error`, never silently dropped — which is also what
        // keeps "the daemon folds only params shapes the core itself defines" true by construction.
        // The empty coid is the account-wide-verb convention (nothing order-scoped to echo).
        WireCommand::UpdateParams { venue, symbol, interval, params } => {
            let params: StrategyParams = serde_json::from_value(params).map_err(|e| {
                format!(
                    "update_params: payload is not a vike StrategyParams \
                     (expected the externally-tagged form, e.g. {{\"SpreadMaker\": {{…}}}}): {e}"
                )
            })?;
            Ok((
                Command::UpdateParams(Box::new(ParamsUpdate { venue, symbol, interval, params })),
                String::new(),
            ))
        }
        // RUNTIME strategy MOUNT (split-plane B5): validate the SPEC at this edge with the daemon's
        // own profile machinery — `crate::mount_factory::validate_spec` runs the SAME
        // `DaemonProfile` refusals a `[strategy]` table faces at load (unknown name,
        // simulator-only, unread/mistyped params keys, name-XOR-rhai), so a remote peer gets a
        // `Response::Error` carrying the profile vocabulary's own message instead of a silent
        // recent-events note. RESOLUTION (compiling/instantiating the strategy) still happens in
        // the core via the injected `CoreConfig::strategy_factory` (which validates AGAIN — this
        // edge is UX, the factory is the authority); a refusal the core raises later (duplicate
        // LIVE id, unknown venue) surfaces in recent-events, the `UpdateParams` unknown-target
        // contract. The empty coid is the account-wide-verb convention.
        WireCommand::MountStrategy {
            venue,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            // ⚠ `account: None` — the WIRE cannot name one. A remote peer mounting onto a second
            // account would be naming a book the operator never armed for that channel, so the
            // runtime mount command stays on the venue's default account and a second account is
            // reached through the daemon PROFILE (`DaemonProfile::account`), which is a file the
            // operator edits rather than a frame a peer sends.
            let spec = MountSpec {
                venue,
                symbol,
                interval,
                account: None,
                controller_id,
                name,
                rhai,
                params,
            };
            crate::mount_factory::validate_spec(&spec)
                .map_err(|e| format!("mount_strategy: {e}"))?;
            Ok((Command::MountStrategy(Box::new(spec)), String::new()))
        }
        // RUNTIME strategy UNMOUNT (split-plane B5): total except for an empty id (which can name
        // nothing). The core cancels the mount's attributed resting orders before removal — the
        // documented safe default (`vike_core`'s `unmount_strategy_runtime` arm is the authority).
        WireCommand::UnmountStrategy { controller_id } => {
            if controller_id.trim().is_empty() {
                return Err("unmount_strategy: empty mount id".to_string());
            }
            Ok((Command::UnmountStrategy { controller_id }, String::new()))
        }
        // The REQ-7 settings write is NOT a core command: `accept_command` intercepts it and
        // lowers it onto disk ([`SettingsShowSource::apply_set_setting`]) before this function is
        // reached. The arm exists so the mapping stays total; a future surface that calls
        // `lower_command` directly gets an honest refusal, never a silent drop.
        WireCommand::SetSetting { .. } => Err(
            "set_setting: not a core command — it is lowered at the daemon edge (accept_command)"
                .to_string(),
        ),
    }
}

/// Map the wire trading-state mirror back onto `vike_exec::TradingState` — the reverse of
/// `crate::publish::project_trading_state`.
fn project_wire_trading_state(ws: WireTradingState) -> TradingState {
    match ws {
        WireTradingState::Active => TradingState::Active,
        WireTradingState::Reducing => TradingState::Reducing,
        WireTradingState::Halted => TradingState::Halted,
    }
}

/// The audit VERB for a wire command — a stable, low-cardinality string for the audit record (read
/// off the wire command BEFORE it is consumed by [`lower_command`]).
fn command_kind(wc: &WireCommand) -> &'static str {
    match wc {
        WireCommand::Submit(_) => "submit",
        WireCommand::Cancel(_) => "cancel",
        WireCommand::Modify { .. } => "modify",
        WireCommand::MassCancel { .. } => "mass_cancel",
        WireCommand::Flatten { .. } => "flatten",
        WireCommand::MarketExit { .. } => "market_exit",
        WireCommand::SetTradingState(_) => "set_trading_state",
        WireCommand::UpdateParams { .. } => "update_params",
        WireCommand::MountStrategy { .. } => "mount_strategy",
        WireCommand::UnmountStrategy { .. } => "unmount_strategy",
        WireCommand::SetSetting { .. } => "set_setting",
    }
}

/// Log the end of a read the way vike-datahub does: a clean EOF is silent, an idle timeout is info, a
/// real fault is a warning. Every path leads to closing the connection (a timeout is never resumed —
/// that would desync the stream).
///
/// ⚠ The `WouldBlock`/`TimedOut` arm is now reachable ONLY from the UNAUTHENTICATED handshake
/// phase, and reading it as anything else would misdiagnose an incident. It used to fire on authed
/// connections too — that line, "idle read timeout, closing connection", was the node's own record
/// of it killing a live control link every five quiet minutes — and it was indistinguishable in the
/// log from a stalled stranger. [`handle_connection`] replaces the bound at `AuthOk`, so a line
/// here now means a peer that connected and never finished authenticating.
fn log_read_end(e: &io::Error, peer: Option<std::net::SocketAddr>) {
    match e.kind() {
        io::ErrorKind::UnexpectedEof => {}
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            tracing::info!(?peer, "vike-tradehub observe: idle read timeout, closing connection");
        }
        _ => {
            tracing::warn!(?peer, error = %e, "vike-tradehub observe: read fault, closing connection");
        }
    }
}

/// The REQ-2 datahub advertisement in `Welcome.features` — served ONLY when configured, and
/// readable by the client-side parser it is spelled for (the round-trip's server half; the
/// client half is pinned in `vike_tradehub_client::proto`'s own tests).
#[cfg(test)]
mod served_features_tests {
    use super::*;
    use vike_tradehub_client::proto::advertised_datahub;

    /// Unconfigured — the default, and every pre-REQ-2 caller — advertises NO datahub entry, and
    /// the list is exactly the named capabilities it always was.
    #[test]
    fn an_unconfigured_daemon_advertises_no_datahub() {
        let features = served_features(None);
        assert!(advertised_datahub(&features).is_none());
        assert!(features.iter().all(|f| !f.starts_with("datahub=")));
    }

    /// Configured — the entry rides BESIDE the named capabilities (nothing is displaced; the
    /// exact-match feature guards still find their strings) and round-trips through the client
    /// parser verbatim.
    #[test]
    fn a_configured_daemon_advertises_its_datahub_beside_the_named_capabilities() {
        let features = served_features(Some("127.0.0.1:7878"));
        assert_eq!(advertised_datahub(&features), Some("127.0.0.1:7878".to_string()));
        for named in [
            "observe",
            "preview",
            FEATURE_STRATEGY_VERBS,
            FEATURE_SETTINGS_SHOW,
            FEATURE_OBSERVE_HEARTBEAT,
        ] {
            assert!(features.iter().any(|f| f == named), "{named} displaced");
        }
        assert_eq!(features, {
            let mut unconfigured = served_features(None);
            unconfigured.push(datahub_feature("127.0.0.1:7878"));
            unconfigured
        });
    }
}

/// The REACHABILITY half of this module's contract — the three properties that hold up "auth is
/// defense in depth, the tunnel is the barrier" (see the module doc). Each is stated as a test
/// because each is one edit away from silently evaporating.
#[cfg(test)]
mod exposure_tests {
    use super::*;
    use std::io::Cursor;
    use std::net::ToSocketAddrs;

    fn resolve(addr: &str) -> Vec<SocketAddr> {
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
    }

    /// The DEFAULT is loopback and must stay so: it is the reachability barrier the plaintext
    /// handshake depends on, not a convenience.
    #[test]
    fn the_default_address_is_loopback() {
        assert_eq!(bind_exposure(&resolve(DEFAULT_ADDR)), BindExposure::Loopback);
    }

    /// ⚠ The wildcards are the whole point. `0.0.0.0` / `::` are what an operator types when they
    /// want "reachable from my laptop", and they are NOT loopback — `IpAddr::is_loopback` is false
    /// for both — so they must classify as exposed, or the guard would pass the single most common
    /// way this surface gets published to a network.
    #[test]
    fn the_wildcard_binds_are_public_not_loopback() {
        for addr in ["0.0.0.0:7879", "[::]:7879"] {
            assert!(
                matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)),
                "{addr} must classify as exposed — it listens on EVERY interface"
            );
        }
    }

    #[test]
    fn loopback_spellings_are_all_loopback_and_a_routable_ip_is_not() {
        // 127.0.0.0/8 in full, both families, and the name.
        for addr in ["127.0.0.1:7879", "127.9.9.9:7879", "[::1]:7879", "localhost:7879"] {
            assert_eq!(bind_exposure(&resolve(addr)), BindExposure::Loopback, "{addr}");
        }
        for addr in ["<host>:7879", "<host>:7879", "[2001:db8::1]:7879"] {
            assert!(matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)), "{addr}");
        }
    }

    /// ⚠ **THE GUARD ITSELF.** Without an opt-in a non-loopback bind is REFUSED, not warned about:
    /// a `warn!` on a surface that (with control on) places real orders is a line in a log file
    /// nobody reads until afterwards. With the opt-in it proceeds, and says so.
    #[test]
    fn a_public_bind_is_refused_without_the_opt_in_and_announced_with_it() {
        let public = resolve("0.0.0.0:7879");
        let exposed = public[0];
        assert_eq!(
            bind_decision(&public, false),
            BindDecision::Refuse(exposed),
            "default (no flags.tradehub_allow_public_bind) must REFUSE a wildcard bind"
        );
        assert_eq!(
            bind_decision(&public, true),
            BindDecision::ProceedExposed(exposed),
            "the named opt-in permits it — and the decision still carries what is exposed, so the \
             daemon can name it"
        );
    }

    /// …and the opt-in changes NOTHING for a loopback bind: it is not a general "skip the checks"
    /// switch, so leaving it set on a host later moved back to loopback is inert.
    #[test]
    fn a_loopback_bind_proceeds_identically_with_or_without_the_opt_in() {
        let local = resolve(DEFAULT_ADDR);
        assert_eq!(bind_decision(&local, false), BindDecision::Proceed);
        assert_eq!(bind_decision(&local, true), BindDecision::Proceed);
        // An address that resolves to nothing is left to `bind` to reject with its own message,
        // rather than being reported as an exposure it is not.
        assert_eq!(bind_decision(&[], false), BindDecision::Proceed);
    }

    /// A name resolving to BOTH loopback and a routable address is exposed on that address, so the
    /// strictest reading is the true one. (Constructed directly: no DNS in a unit test.)
    #[test]
    fn one_routable_address_among_loopbacks_is_still_public() {
        let mixed = vec![
            "127.0.0.1:7879".parse::<SocketAddr>().unwrap(),
            "<host>:7879".parse::<SocketAddr>().unwrap(),
        ];
        assert!(matches!(bind_exposure(&mixed), BindExposure::Public(_)));
        // …and "resolved to nothing" is its own answer, never silently read as exposed.
        assert_eq!(bind_exposure(&[]), BindExposure::Unresolvable);
    }

    /// PRE-AUTH ALLOCATION. A four-byte length prefix from a peer that has sent no key must not be
    /// able to name a 64 MiB buffer. The guard fires on the LENGTH — note the body is never
    /// supplied, so a passing read would have had to allocate first.
    #[test]
    fn an_unauthenticated_peer_cannot_name_a_huge_allocation() {
        let over = HANDSHAKE_MAX_FRAME_LEN + 1;
        let mut framed = over.to_be_bytes().to_vec();
        framed.push(0); // one byte of "body" — the refusal must not be a short-read artifact
        let err = read_frame_raw_capped(&mut Cursor::new(framed), HANDSHAKE_MAX_FRAME_LEN)
            .expect_err("a length past the handshake cap must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        // The SAME frame is fine under the post-auth ceiling — proving the cap, not the framing, is
        // what refused it, and that the pre-auth phase is genuinely stricter than the session.
        let mut ok_framed = over.to_be_bytes().to_vec();
        ok_framed.extend(std::iter::repeat_n(b'x', over as usize));
        assert_eq!(
            read_frame_raw(&mut Cursor::new(ok_framed)).expect("under MAX_FRAME_LEN").len(),
            over as usize
        );
    }

    /// A real handshake fits the cap with orders of magnitude to spare — the cap must bound an
    /// attacker, not a client. Both pre-auth frames are measured as they go on the wire.
    #[test]
    fn the_handshake_cap_is_far_larger_than_a_real_handshake() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &Request::Hello { proto_version: NODE_PROTO_VERSION }).unwrap();
        write_frame(&mut buf, &Request::Auth { scope: Scope::Control, mac: vec![0u8; 32] })
            .unwrap();
        assert!(
            buf.len() * 8 < HANDSHAKE_MAX_FRAME_LEN as usize,
            "both handshake frames are {} bytes; the cap ({HANDSHAKE_MAX_FRAME_LEN}) must keep a \
             wide margin over them",
            buf.len()
        );
    }

    /// The slot guard must RELEASE — a counter that only goes up would wedge the server at
    /// [`MAX_CONNECTIONS`] LIFETIME connections, turning a DoS guard into the DoS. (That the cap
    /// itself is a sane positive bound is asserted at COMPILE time, beside the constant.)
    #[test]
    fn a_connection_slot_is_released_when_its_thread_ends() {
        let live = Arc::new(AtomicUsize::new(0));
        {
            let _a = ConnSlot(Arc::clone(&live));
            live.fetch_add(1, Ordering::AcqRel);
            let _b = ConnSlot(Arc::clone(&live));
            live.fetch_add(1, Ordering::AcqRel);
            assert_eq!(live.load(Ordering::Acquire), 2);
        }
        assert_eq!(live.load(Ordering::Acquire), 0, "both slots freed on drop");
    }
}

#[cfg(test)]
mod control_limits_tests {
    use super::*;
    use vike_tradehub_client::wire::WireOrderRequest;

    fn submit(qty: f64, price: Option<f64>) -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "c1".into(),
            venue: "hyperliquid".into(),
            symbol: "BTC".into(),
            side: 1,
            qty,
            order_type: "limit".into(),
            price,
            trigger_price: None,
            reduce_only: false,
        })
    }

    fn limits(max_notional: Option<f64>, rate: f64) -> ControlLimits {
        // The exact per-connection construction `handle_connection` performs (audit F13) — a fresh
        // full bucket over a caller-owned config, no env involved.
        ControlLimits::new(ControlLimitsConfig { max_notional, rate_per_sec: rate })
    }

    #[test]
    fn notional_cap_rejects_oversized_submit_allows_small() {
        let mut l = limits(Some(1_000.0), 1e9); // huge rate so ONLY the size cap is under test
        assert!(l.vet(&submit(100.0, Some(50.0))).is_some(), "100*50=5000 > 1000 → refused");
        assert!(l.vet(&submit(10.0, Some(50.0))).is_none(), "10*50=500 ≤ 1000 → allowed");
        assert!(l.vet(&submit(-100.0, Some(50.0))).is_some(), "|qty| used: -100*50=5000 → refused");
    }

    #[test]
    fn market_order_without_a_price_skips_the_size_cap() {
        // No price → the server can't compute notional → not size-capped (the RiskGate still applies).
        let mut l = limits(Some(1.0), 1e9);
        assert!(l.vet(&submit(1_000_000.0, None)).is_none());
    }

    /// THE MODIFY HOLE: the cap matched `Modify { new_qty: Some, new_price: Some }` — BOTH
    /// required — so omitting the price fell to `_ => None` and the ceiling did not apply at all.
    /// `/orders` prints coids, so the reachable sequence from either remote surface was
    /// `/orders` → `/modify <coid> qty=<huge>` with no price named. Unsizeable must fail CLOSED.
    #[test]
    fn a_priceless_qty_raising_modify_is_refused_not_waved_through() {
        let mut l = limits(Some(1_000.0), 1e9);
        let reason = l
            .vet(&WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(1e9),
                new_price: None,
            })
            .expect("a modify the edge cannot size must be REFUSED, not allowed");
        assert!(
            reason.contains("no price"),
            "the refusal must tell the operator how to make it checkable, got {reason:?}"
        );
        // ...and the same shape is refused on the read-only preview, so a preview can never report
        // a command as fine that `vet` would refuse (they share `notional_reason`).
        assert!(
            l.preview_vet(&WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(1e9),
                new_price: None,
            })
            .is_some()
        );
    }

    /// A priced modify is still sized normally — the refusal above must not become a blanket ban on
    /// the verb, and a price-only modify (no qty change) carries no new size to check.
    #[test]
    fn a_priced_modify_is_capped_and_a_price_only_modify_is_not_refused() {
        let mut l = limits(Some(1_000.0), 1e9);
        let m = |q: Option<f64>, p: Option<f64>| WireCommand::Modify {
            client_order_id: "c1".into(),
            new_qty: q,
            new_price: p,
        };
        assert!(l.vet(&m(Some(100.0), Some(50.0))).is_some(), "100*50=5000 > 1000 → refused");
        assert!(l.vet(&m(Some(10.0), Some(50.0))).is_none(), "10*50=500 ≤ 1000 → allowed");
        // No qty change ⇒ nothing to size at this edge (the core RiskGate judges the projected
        // order either way, and it DOES hold the resting terms).
        assert!(l.vet(&m(None, Some(50.0))).is_none(), "a price-only modify is not size-refused");
        assert!(l.vet(&m(None, None)).is_none(), "an empty modify is not size-refused");
    }

    /// NOTIONAL IS A MAGNITUDE. The cap abs'd the QTY but not the PRICE, so a negative price made
    /// `n` negative, `n > max` could never trip, and the ceiling was bypassed by a sign alone —
    /// on BOTH the submit and the modify arm. (`vike_model::order_notional`, which the core
    /// `RiskGate` uses, abs's every factor; this edge did not.)
    #[test]
    fn a_negative_price_cannot_slip_past_the_cap() {
        let mut l = limits(Some(1_000.0), 1e9);
        assert!(
            l.vet(&submit(100.0, Some(-50.0))).is_some(),
            "|100 * -50| = 5000 > 1000 → refused; without the price abs this returned -5000 and passed"
        );
        assert!(
            l.vet(&WireCommand::Modify {
                client_order_id: "c1".into(),
                new_qty: Some(100.0),
                new_price: Some(-50.0),
            })
            .is_some(),
            "the modify arm had the identical sign hole"
        );
    }

    #[test]
    fn risk_reducing_verbs_are_never_size_capped() {
        let mut l = limits(Some(1.0), 1e9);
        assert!(l.vet(&WireCommand::Cancel("c1".into())).is_none());
        assert!(
            l.vet(&WireCommand::Flatten { venue: "hyperliquid".into(), symbol: "BTC".into() })
                .is_none()
        );
        assert!(l.vet(&WireCommand::MarketExit { venue: None }).is_none());
        assert!(l.vet(&WireCommand::MassCancel { venue: None, symbol: None }).is_none());
    }

    /// The B4 vetting decision, pinned: `UpdateParams` is NOT notional-capped (it is not an order —
    /// see the `notional_reason` arm for the argument) but it DOES consume a rate token like every
    /// command, so a re-tune flood is still rate-refused.
    #[test]
    fn update_params_skips_the_size_cap_but_still_pays_the_rate_token() {
        let up = || WireCommand::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            params: serde_json::json!({"SpreadMaker": {"qty": 1e12}}),
        };
        // A tiny notional ceiling that would refuse ANY sized order: the re-tune passes anyway.
        let mut sized = limits(Some(0.000_001), 1e9);
        assert!(sized.vet(&up()).is_none(), "a params update carries no order notional to cap");
        // …but the rate bucket applies: 2/s ⇒ the third immediate re-tune is refused.
        let mut rated = limits(None, 2.0);
        assert!(rated.vet(&up()).is_none(), "token 1");
        assert!(rated.vet(&up()).is_none(), "token 2");
        assert!(rated.vet(&up()).is_some(), "3rd immediate re-tune → rate limited");
    }

    #[test]
    fn rate_limit_refuses_a_burst_past_the_cap() {
        let mut l = limits(None, 2.0); // 2/s, bucket starts full at 2 tokens
        assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 1");
        assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 2");
        assert!(l.vet(&submit(1.0, Some(1.0))).is_some(), "3rd immediate command → rate limited");
    }

    #[test]
    fn no_size_cap_without_a_policy_ceiling() {
        // No `policy.toml` (or no `max_notional_per_order` key) ⇒ `from_policy(None, ..)` ⇒
        // `max_notional: None` ⇒ any notional passes (the RiskGate is the floor); a huge order is
        // NOT refused by the size gate. This is today's default, unchanged by Phase 5.
        let mut l = limits(None, 1e9);
        assert!(l.vet(&submit(1e9, Some(1e9))).is_none());
    }

    // --- ControlLimitsConfig::from_policy — the pure resolver main.rs feeds the loaded POLICY
    // ceiling and the raw rate value to (audit F13: the semantics the retired per-connection
    // `from_env` had, now unit-testable and with the ceiling no longer env-settable). ---

    #[test]
    fn from_policy_with_nothing_set_gives_the_default_config() {
        assert_eq!(ControlLimitsConfig::from_policy(None, None), ControlLimitsConfig::default());
        // …and the default is: no size cap, DEFAULT_CONTROL_RATE commands/sec.
        assert_eq!(
            ControlLimitsConfig::default(),
            ControlLimitsConfig { max_notional: None, rate_per_sec: DEFAULT_CONTROL_RATE }
        );
    }

    /// The Phase-5 "value flows" property at this end: a `policy.toml` ceiling becomes the
    /// server-edge notional cap, while the rate keeps its (still env-settable) string parse.
    #[test]
    fn from_policy_carries_the_ceiling_through_and_still_trims_the_rate() {
        assert_eq!(
            ControlLimitsConfig::from_policy(Some(250.5), Some(" 40 ")),
            ControlLimitsConfig { max_notional: Some(250.5), rate_per_sec: 40.0 }
        );
    }

    #[test]
    fn from_policy_treats_a_nonsense_ceiling_or_rate_as_unset() {
        // A non-positive/non-finite ceiling degrades to OFF rather than arming a limit that would
        // refuse EVERY order (a silent halt) — `vike_config` already rejects both when loading
        // `policy.toml`, so this only bites a `Policy` built some other way. Garbage/non-positive
        // rate falls back to the default, exactly as before.
        for bad in [Some(0.0), Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(ControlLimitsConfig::from_policy(bad, None), ControlLimitsConfig::default());
        }
        assert_eq!(
            ControlLimitsConfig::from_policy(None, Some("nope")),
            ControlLimitsConfig::default()
        );
        assert_eq!(
            ControlLimitsConfig::from_policy(None, Some("-3")),
            ControlLimitsConfig::default()
        );
    }

    #[test]
    fn preview_vet_applies_the_notional_cap() {
        // The dry-run verdict mirrors the executing `vet`'s notional decision (oversized → reason,
        // within-cap → None), so a Preview reflects the real gate.
        let l = limits(Some(1_000.0), 1e9);
        assert!(l.preview_vet(&submit(100.0, Some(50.0))).is_some(), "5000 > 1000 → would refuse");
        assert!(l.preview_vet(&submit(10.0, Some(50.0))).is_none(), "500 ≤ 1000 → would pass");
    }

    #[test]
    fn preview_vet_consumes_no_rate_token() {
        // A preview must not drain the command budget nor be rate-limited: even a tiny bucket answers
        // every preview, and the real `vet` afterward still has its full allotment of tokens.
        let mut l = limits(None, 2.0);
        for _ in 0..100 {
            assert!(
                l.preview_vet(&submit(1.0, Some(1.0))).is_none(),
                "previews are never rate-limited"
            );
        }
        // The bucket is untouched: two real commands still pass, the third is rate-limited (as if no
        // preview had ever happened).
        assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 1 intact");
        assert!(l.vet(&submit(1.0, Some(1.0))).is_none(), "token 2 intact");
        assert!(
            l.vet(&submit(1.0, Some(1.0))).is_some(),
            "3rd → rate limited, so previews consumed none"
        );
    }
}

#[cfg(test)]
mod lower_command_tests {
    //! The B4 lowering edge in isolation: the wire `UpdateParams` payload IS the core's own
    //! `StrategyParams` serde schema, and an undecodable payload is a REFUSAL, never a silent
    //! drop. (The end-to-end proof — the lowered command reaching a mounted strategy through the
    //! real server — is `tests/control_roundtrip.rs`'s
    //! `update_params_over_the_wire_retunes_the_mounted_strategy`.)

    use super::*;

    /// A fully-populated `SpreadMakerParams` payload built from the REAL core type — what proves
    /// "the wire schema IS the core schema" rather than a lookalike.
    fn spread_maker_params(qty: f64, half_spread: f64) -> StrategyParams {
        StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
            qty,
            half_spread,
            target_inventory: 0.0,
            max_inventory: 1.0,
            skew: 0.0,
            fill_window_ms: 0,
            net_fill_threshold: 0.0,
            suppress_cooldown_ms: 0,
            style: vike_model::QuoteStyle::Mid,
            depth_levels: 1,
            tick_size: 0.0,
            filter_own: false,
            avellaneda_stoikov: None,
            refresh_tolerance: None,
            ladder: None,
            reward: None,
            toxicity: None,
        })
    }

    #[test]
    fn update_params_lowers_into_the_real_core_command() {
        let wire = WireCommand::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            // The core's own externally-tagged form — proven against the REAL type below.
            params: serde_json::to_value(spread_maker_params(2.0, 1.0)).unwrap(),
        };
        let (cmd, coid) = lower_command(wire).expect("a decodable payload lowers");
        assert_eq!(coid, "", "account-wide-verb convention: nothing order-scoped to echo");
        let Command::UpdateParams(u) = cmd else {
            panic!("must lower to Command::UpdateParams, got a different command");
        };
        assert_eq!(
            (u.venue.as_str(), u.symbol.as_str(), u.interval.as_str()),
            ("binance", "BTCUSDT", "1m")
        );
        let StrategyParams::SpreadMaker(p) = u.params else {
            panic!("the typed variant survives the wire round-trip");
        };
        assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "the payload's knobs land verbatim");
        assert_eq!(p.half_spread.to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn an_undecodable_params_payload_is_refused_not_dropped() {
        let wire = WireCommand::UpdateParams {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            params: serde_json::json!({"NoSuchStrategyParams": {"qty": 1.0}}),
        };
        let err = lower_command(wire).expect_err("an unknown variant must refuse");
        assert!(err.contains("update_params"), "names the verb: {err}");
    }

    #[test]
    fn update_params_has_its_own_audit_kind() {
        let wire = WireCommand::UpdateParams {
            venue: "b".into(),
            symbol: "s".into(),
            interval: "1m".into(),
            params: serde_json::json!({}),
        };
        assert_eq!(command_kind(&wire), "update_params");
    }
}

// The LINK-LIVENESS suite: a real paper node on a real socket, driven through the crate-private
// `LinkPolicy` seam so the five-minute and fifteen-second properties are proven at ~1000x scale
// rather than in five minutes. A child module rather than an integration test because that seam is
// deliberately not public — see the file's own doc, and `crates/vike-cli/src/cmd/
// mcp_node_drop_tests.rs` for the same trade made for the same reason.
#[cfg(test)]
#[path = "server_link_liveness_tests.rs"]
mod link_liveness_tests;
