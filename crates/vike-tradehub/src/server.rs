//! `server` — the authenticated node server (headless two-layer plan, Layer 2, PR-11 observe + PR-12
//! control).
//!
//! Binds `VIKE_TRADEHUB_ADDR` (default [`DEFAULT_ADDR`] = `127.0.0.1:7879`), thread-per-connection,
//! and runs the PR-10 node handshake before serving anything: `Hello` -> `Welcome{ nonce }` ->
//! `Auth{ scope, mac }` -> (verify via [`vike_tradehub_client::auth::verify`] against the scope's
//! [`NodeKeys`]) -> `AuthOk`. On `Subscribe` an [`Scope::Read`] connection registers with the
//! [`crate::publish`] publisher and this thread becomes the connection's WRITER, draining its
//! bounded mailbox and writing pushed frames to the socket.
//!
//! ## The control path (PR-12) — a SEPARATE connection that never subscribes
//!
//! A subscribed connection is a one-way PUSH pipe (after `Subscribe` this thread only writes), so a
//! control client authenticates under [`Scope::Write`] and NEVER subscribes — it stays in the
//! request/response loop, and each [`Request::Command`] is lowered ([`lower_command`]) into the core's
//! real `Command`/`OrderIntent` and handed to the [`vike_core::CommandSink`] the daemon threaded in.
//! Control is DOUBLE-GATED: the node must hold a control key ([`NodeKeys::has`]) AND the daemon must
//! pass `Some(sink)` (gated on `VIKE_TRADEHUB_CONTROL=1`). Absent either, a `Control` auth or a
//! `Command` is refused. An [`Scope::Read`] peer's `Command` is always refused (read-only). Every
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
//! - **`WireCommand::UpdateParams`** (write, `Scope::Write`): lowered by [`lower_command`] into
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
//! [`venue_refusal`] (the ROUTING gate) -> [`account_refusal`] (the same gate one field along) ->
//! [`audit::sanitize_reason`] -> [`lower_command`] ->
//! `CommandSink::try_command` ->
//! [`audit::record`], in that order — and BOTH surfaces call it. Identical gating by CONSTRUCTION,
//! not by review. The TCP arm keeps its own scope/sink gates (they are connection properties) and
//! renders [`AcceptError`] back onto the exact `Response::Error` strings it always sent.
//!
//! ## WHICH BOOK a command lands on — [`venue_refusal`]
//!
//! Every order-carrying `WireCommand` names a venue and always has; nothing CHECKED it. The core's
//! `CoreThread::route_of` resolves that string to an engine index and takes `.unwrap_or(0)` when it
//! resolves to none, and engine 0 is the PRIMARY — so a peer naming a venue this process runs no
//! engine for had its order signed and sent by the primary venue's client, answered `Ack`, and
//! shown in the snapshot, with no error anywhere. [`venue_refusal`] is the check, and it REFUSES
//! (naming the venues this node does run) rather than choosing one: refusing costs the operator a
//! retry, guessing costs them a position on a book they did not name. The node advertises
//! [`FEATURE_VENUE_ROUTING`] so a client can tell it from one that still misroutes silently; the
//! client half of that is `vike_app_core::tradehub_control::venue_routing_verdict`.
//!
//! ⚠ **WHICH BOOK OF IT — [`account_refusal`].** A node may run several ACCOUNTS of one exchange,
//! and `venues[].venue` is the same string on every one of them, so the gate above is evidence that
//! cannot tell two books apart. A submit naming an account this node does not run was therefore
//! Acked and refused out of band by the core, and the client printed `accepted` over an order that
//! never existed. [`account_refusal`] is that verdict moved in front of the Ack, compared against
//! the published ROUTE KEYS rather than the venue ids. It is a SECOND gate beside the first, never
//! a widening of it — its doc carries the argument, and so does
//! `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`, whose *What would
//! reopen this* named this exact configuration in advance.
//!
//! ## The LIVE-JOURNAL REPORT verb (ruling 16) — the one verb that READS THIS NODE'S DISK
//!
//! [`Request::Tearsheet`] asks this node to fold the command journal IT is writing into a
//! `vike_report::LiveTearsheet` and reply with the document's JSON ([`tearsheet_reply`]). Post-auth
//! under EITHER scope, like `Snapshot`/`StrategyStatus` — it executes nothing, so no audit record —
//! and advertised as [`FEATURE_TEARSHEET`].
//!
//! ⚠ It is the first verb here whose answer comes from the FILESYSTEM rather than from the
//! publisher's snapshot cell, and that is worth knowing for two reasons. The read happens on the
//! CONNECTION thread, so a large journal costs that one peer's thread and nothing else — the
//! hot-path guarantee below is unchanged, since nothing on this path touches the core. And the
//! journal is resolved from the daemon's own startup environment sweep, which is one rung short of
//! what the daemon itself resolves; [`tearsheet_reply`] names the missing rung and what the reply
//! says instead of pretending.
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
//! Three numbers govern it. Two are the server's half of a contract with the client and are NOT
//! defined here — they live one layer down in `vike_tradehub_client::liveness` and this module
//! consumes them; the third has no client half and is this module's own ([`PUSH_WRITE_TIMEOUT`]).
//! [`LinkPolicy::PRODUCTION`] is where all of them land.
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
//! - **A SUBSCRIBED stream's writes are BOUNDED.** [`handle_connection`] sets [`PUSH_WRITE_TIMEOUT`]
//!   on the socket the moment a `Subscribe` turns the connection thread into a writer, so a peer
//!   that keeps its socket open and stops READING is closed after that long instead of parking the
//!   thread in `write_all` for as long as it likes. The heartbeat cannot do this: it catches a DEAD
//!   peer, whose socket eventually errors a write, never a live one that does not drain.
//!
//! ## The hot-path guarantee
//!
//! Nothing in this module touches the vike-core fold. The publisher (which this server feeds) reads
//! only the arc-swap snapshot cell; per-connection writer threads do socket I/O in isolation. A
//! stalled client blocks only ITS OWN writer thread (parked in `write_all` on a full send buffer),
//! never the publisher and never another client — the mailbox drop-oldest contract guarantees it.
//! This is why the p99 core-hop latency gate is unaffected (the PR-11 merge condition).
//!
//! ⚠ "Only its own writer thread" was, until [`PUSH_WRITE_TIMEOUT`], the WHOLE of the bound: that
//! thread stayed parked for as long as the peer kept its socket open — indefinitely, inside the
//! daemon that signs orders, holding a [`MAX_CONNECTIONS`] slot — and this paragraph presented the
//! isolation as if it were also a limit. It is not; the write timeout is. A subscriber that reads
//! nothing for that long is closed, and the thread and the slot come back.
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
    FEATURE_ACCOUNT_ROUTING, FEATURE_ACCOUNT_VERBS, FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_CLASS,
    FEATURE_MOUNT_VERBS, FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS, FEATURE_TEARSHEET, FEATURE_VENUE_ROUTING,
    NODE_PROTO_VERSION, Request, Response, Scope, datahub_feature, read_frame_raw,
    read_frame_raw_capped, write_frame,
};
use vike_tradehub_client::wire::{
    AccountRequest, AccountVerb, WireAccountList, WireAccountRow, WireAccountWritten, WireCommand,
    WireMountRow, WireSettingsRow, WireSettingsShow, WireStrategyStatus, WireTradingState,
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

/// How long a SUBSCRIBED connection's writer may sit in ONE socket write making NO progress before
/// the node closes the connection — the socket's `SO_SNDTIMEO`, applied by [`handle_connection`]
/// the moment a `Subscribe` turns the connection thread into [`run_push_writer`].
///
/// # The hole it closes
///
/// Before it, a push write had no bound at all. The heartbeat
/// ([`vike_tradehub_client::liveness::OBSERVE_HEARTBEAT`]) catches a DEAD peer: a socket nobody
/// answers eventually fails a write (TCP gives up retransmitting) and the thread exits. It does
/// nothing for a LIVE peer that stops READING — and on this server that is the ordinary shape of a
/// stall, because the loopback peer is normally `sshd`: the moment its channel to a sleeping laptop
/// stops draining, sshd stays alive, keeps the socket open, ACKs every byte until its buffer is
/// full and then simply reads no more. The send buffer fills, `write_all` parks, and the thread
/// stayed parked for as long as that socket stayed open — indefinitely, inside the daemon that
/// signs orders, holding one of [`MAX_CONNECTIONS`]. The publisher was never at risk (the mailbox
/// is drop-oldest and it never touches a socket), which is why the module doc could truthfully say
/// "blocks only its own writer thread" and leave it there; "only its own thread" is still a thread
/// and a slot, leaked per stalled peer.
///
/// # What a timeout means — and what it cannot mean
///
/// `SO_SNDTIMEO` bounds a wait for SPACE, not a whole frame: `write_all` loops on `write`, each
/// `write` copies whatever fits into the send buffer and returns that count, and only a `write`
/// that could copy NOTHING for the whole window errors (`WouldBlock` on Linux, `TimedOut` on
/// Windows — [`log_push_write_end`] names both). So a timeout here never means "a big frame took a
/// while" — it means the peer's kernel freed not one byte of receive window for that long, which a
/// peer that is reading at ALL cannot produce: a frame is kilobytes, and draining a whole send
/// buffer is a millisecond of reading on any link that can carry the stream, a tunnel included.
///
/// # Why 30 s
///
/// Anchored on the two cadences this stream runs at. A BUSY node writes at the core's ≥16 ms
/// coalesced publish cadence; an IDLE one writes a heartbeat every 15 s. 30 s is two heartbeats:
/// three orders of magnitude above anything a scheduling hiccup on either side can reach, and
/// short enough that a stalled subscriber's thread is back before a human would have noticed the
/// stall. It is also the number the client side already chose for the same reason twice —
/// `vike_tradehub_client::liveness::CONTROL_REPLY_TIMEOUT` for a control reply, and
/// `crates/vike-datahub-client/src/client.rs`'s `REQUEST_WRITE_TIMEOUT` for a request write —
/// where a wait that long is likewise "the peer stopped, not a large body in flight". The
/// compile-time bounds below keep it inside [one heartbeat, the handshake bound]: tighter would
/// clip a heartbeat write on a loaded box; looser would let an authenticated subscriber that
/// stopped reading out-live a stranger who never spoke.
///
/// ⚠ Not a `liveness` constant, deliberately: the client crate's numbers are halves of a contract
/// BOTH ends must agree on, and this one has no client half — a subscriber that is draining never
/// meets it — so it lives here beside [`HANDSHAKE_READ_TIMEOUT`], the other bound that is this
/// server's own business. Not a settings key either, for [`MAX_CONNECTIONS`]'s reason: it is a
/// resource bound inside the order-signing daemon, not an operator preference.
const PUSH_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// **The budget this daemon gives the settings-directory lock: 1 s** — what
/// [`SettingsShowSource::apply_set_setting`] passes to `vike_config::set_setting_within`.
///
/// ⚠ A settings write here runs on a CONNECTION THREAD, and the shared writer's ~3 s default is
/// argued for a CLI that is the whole process. It is not one here, and the two ends of the range
/// are both real:
///
/// * **Not zero.** Unlike the GUI, this waiter has nothing to paint and nobody watching a frame,
///   and a refusal is not free to the peer: it costs a round trip and one of the
///   [`ControlLimits`] rate tokens that peer is metered on. Turning a millisecond overlap with
///   this box's own `vike-cli config set` into a wire refusal would spend a token on a race
///   nobody was in.
/// * **Not long.** The thing being held is a SOCKET. The client's own reply deadline is
///   [`liveness::CONTROL_REPLY_TIMEOUT`], and blocking past it is strictly worse than refusing:
///   the peer gives up on a write that then LANDS, with nobody left to hear the answer — the one
///   failure shape a settings write must not have. An accepted HOT write can already add
///   [`crate::hot_reload::HOT_APPLY_DEADLINE`] on this same thread AFTER the lock, so the worst
///   case a peer sees is the two together, which the assertion below holds inside a fifth of its
///   deadline.
///
/// And the peer has the one thing neither other caller has: a retry loop above it. A `Busy`
/// refusal here is an answer it can act on, not a dead end.
///
/// ⚠ **DECLARED RESIDUAL: under the SHIPPED sandbox, not one millisecond of this budget is ever
/// spent.** `deploy/vike-tradehub.service` — the one shipped unit, which a box installs with its
/// own root substituted in — carries `ProtectSystem=strict` with a `ReadWritePaths=` grant naming `<project>/settings/state`
/// and nothing above it, so this daemon's settings directory is READ-ONLY in its own mount
/// namespace: the sentinel open fails with `EROFS` and the write refuses at
/// `vike_config::SettingsWriteError::Lock` before the spin loop is reached at all. MEASURED
/// 2026-09-13 on the CI box inside the live daemon's namespace (`nsenter -t <MainPID> -m`):
/// `touch settings/settings.lock` → "Read-only file system", `touch settings/state/.probe` → OK.
/// So the number above governs a CONTAINER or DEV configuration whose settings directory is
/// writable — it is not dead, and it is not what the CI box exercises. ⚠ Whether this daemon should be
/// able to write its own settings at all is an OWNER RULING in flight, not something to settle by
/// widening a grant: the grant argues the other side at itself (*"a daemon that can rewrite its own
/// ceiling has none"*). See [`SettingsShowSource::apply_set_setting`], where the same fact is stated
/// at the arm it makes unreachable.
pub const SETTINGS_LOCK_BUDGET: vike_config::LockBudget =
    vike_config::LockBudget::from_millis(1_000);

// The `PUSH_WRITE_TIMEOUT` idiom above: a RANGE at compile time, so a deliberate tweak stays free
// while "the bound was effectively removed" — in either direction — does not compile.
const _: () = assert!(
    SETTINGS_LOCK_BUDGET.max_wait_ms() > 0
        && SETTINGS_LOCK_BUDGET.max_wait_ms()
            + crate::hot_reload::HOT_APPLY_DEADLINE.as_millis() as u64
            <= liveness::CONTROL_REPLY_TIMEOUT.as_millis() as u64 / 5,
    "a SetSetting must never hold a peer near its own reply deadline, and must never refuse \
     without waiting at all — SETTINGS_LOCK_BUDGET's doc argues each edge"
);

// The `confirm.rs` idiom the bounds on `MAX_CONNECTIONS` and `HANDSHAKE_MAX_FRAME_LEN` below use: a
// RANGE at compile time, so a deliberate tweak stays free while "the bound was effectively removed"
// does not compile. `Duration` has no const comparison, so the check is over milliseconds.
const _: () = assert!(
    PUSH_WRITE_TIMEOUT.as_millis() >= liveness::OBSERVE_HEARTBEAT.as_millis()
        && PUSH_WRITE_TIMEOUT.as_millis() <= HANDSHAKE_READ_TIMEOUT.as_millis(),
    "PUSH_WRITE_TIMEOUT must stay between one observe heartbeat and the unauthenticated handshake \
     bound — its doc argues each edge"
);

/// The per-connection LINK-LIVENESS numbers this server runs a connection under, in one struct so
/// a test can scale them and production cannot.
///
/// [`Self::PRODUCTION`] is the only value any shipped path uses — [`serve`] passes it and nothing
/// else may choose one, because two of these numbers are halves of a CONTRACT with the client
/// (`vike_tradehub_client::liveness`, which owns them; this crate consumes them) and the other two
/// are this server's own resource bounds. It exists as a parameter for one reason: the properties
/// worth testing are "an authed connection outlives the old five-minute bound", "an idle subscriber
/// is still being written to" and "a subscriber that stops reading is closed", and a test that
/// proved any of them at production scale would take five minutes, fifteen seconds and thirty
/// seconds respectively. The unit tests below scale every finite number down by ~150–1000x and
/// assert the same properties in seconds.
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
    /// How long a SUBSCRIBED connection's writer may make no progress on one socket write before
    /// the connection is closed. `None` = unbounded — what shipped before the bound existed, and
    /// what the link-liveness tests' kill proof restores to show the test can fail. See
    /// [`PUSH_WRITE_TIMEOUT`].
    push_write_timeout: Option<Duration>,
}

impl LinkPolicy {
    /// The shipped policy: this crate's own unauthenticated handshake bound and push write bound,
    /// plus the two numbers the CLIENT crate owns because both ends must agree on them.
    const PRODUCTION: LinkPolicy = LinkPolicy {
        handshake_read_timeout: HANDSHAKE_READ_TIMEOUT,
        authed_idle_timeout: liveness::AUTHED_IDLE_TIMEOUT,
        observe_heartbeat: liveness::OBSERVE_HEARTBEAT,
        push_write_timeout: Some(PUSH_WRITE_TIMEOUT),
    };
}

// Production carries the bound. A `None` here is the pre-bound behaviour — a subscriber that stops
// reading parks its thread for good — and it must not be reachable by editing one line in silence.
const _: () = assert!(
    LinkPolicy::PRODUCTION.push_write_timeout.is_some(),
    "LinkPolicy::PRODUCTION must carry a push write bound — `None` is the unbounded stall"
);

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
/// Each connection owns a THREAD for its lifetime — a subscriber's thread parks in `write_all` (for
/// up to [`PUSH_WRITE_TIMEOUT`] per write), and the handshake read timeout is 300 s — so without a
/// cap, anyone who can reach the socket spawns threads until the process runs out of them, by
/// opening sockets and saying nothing. The real population is a GUI or two plus a control client,
/// so 64 is roughly two orders of magnitude of headroom over legitimate use while still being a
/// bound.
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
/// ## Scope — why [`Scope::Read`] suffices (the redaction argument)
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
    /// **WHICH SOURCE this daemon BOOTED on** — `vike_config::Settings::authority` as the running
    /// process resolved it, not as a fresh read would answer.
    ///
    /// ⚠ It exists because this reply RE-RESOLVES per request while the CORE keeps folding what it
    /// booted with. `vike-cli config adopt` is an operator act performed with the daemon still
    /// running, so between the adoption and the next restart a fresh read says the rows answer and
    /// the daemon is still running on the files. Without this field the reply would report the
    /// NEWER resolution as though the daemon had applied it — the one shape of answer this surface
    /// exists to make impossible, since it is what the GUI renders as *what the box is running on*.
    pub booted_authority: vike_config::Authority,
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
        // ⚠ **THROUGH THE SETTINGS DATABASE, for the reason this reply exists at all.** Decision
        // 0057's Phase 1 makes the store a LAYER of the loader `vike_boot::boot` applies, so a
        // reply built from the four files alone would describe a set of layers THIS DAEMON did not
        // resolve — and this is the surface the GUI's Connections panel renders as "what the box is
        // running on". A panel that can disagree with the process it is attached to is the defect
        // `vike_config::Origin` exists to make impossible, wearing a different carrier.
        //
        // A store that will not OPEN degrades rather than failing the reply: this is a DISCLOSURE
        // surface on a daemon that is already running, so a refusal here removes the panel an
        // operator would use to find out why. `Description::store_refusal` carries the fact instead
        // and the reply renders it.
        //
        // ⚠ **This reply RE-RESOLVES, and during an adoption window it can describe a resolution
        // this process never applied.** Between `vike-cli config adopt` and the next restart the
        // rows answer here while the running core is still folding what it BOOTED with, so the
        // reply names the discrepancy out loud rather than quietly reporting the newer one. A
        // disagreement that is visible beats one that is not.
        let store = self.settings_dir.as_deref().map(vike_secrets::read_settings_in);
        if let Some(Err(e)) = &store {
            tracing::warn!(error = %e, "the settings database could not be read; describing without it");
        }
        let mut store_refusal_scratch = String::new();
        let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal_scratch);
        let adopted_since_boot =
            source.adoption().is_some() && self.booted_authority != vike_config::Authority::Store;
        match vike_config::describe_with_source(self.settings_dir.as_deref(), source, &self.env) {
            Ok(d) => {
                // ⚠ The ADOPTION WINDOW, said out loud. This is a fresh resolution; the CORE is
                // still folding the one it booted with, and between `config adopt` and the next
                // restart those differ. A panel that showed the newer answer as "what the box is
                // running on" would be exactly the disagreement this surface exists to make
                // impossible, so the fact rides the `settings_dir` line — the one field every
                // client already renders — rather than being left for a reader to infer.
                if adopted_since_boot {
                    tracing::warn!(
                        "this box has been ADOPTED since this daemon booted: the rows below are \
                         what a RESTART would resolve, not what this process is running on"
                    );
                }
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
                let settings_dir = d.settings_dir.map(|p| {
                    let p = p.display().to_string();
                    if adopted_since_boot {
                        format!(
                            "{p} (⚠ ADOPTED since this daemon booted — these rows are what a \
                             RESTART would resolve, not what this process is running on)"
                        )
                    } else {
                        p
                    }
                });
                Response::SettingsShow(Box::new(WireSettingsShow { settings_dir, rows }))
            }

            Err(e) => Response::Error(format!("settings could not be loaded: {e}")),
        }
    }

    /// Lower ONE accepted `WireCommand::SetSetting` onto disk (split-plane REQ-7, write half):
    /// resolve the file, enforce the TYPED-CONFIRM contract for `policy.toml`, and hand the file
    /// mechanics to `vike_config::set_setting_within` — which validates the WOULD-BE file with the
    /// loader before a byte lands (so the write can never produce a file the next boot refuses)
    /// and edits comment-preservingly + atomically. The budget it passes that writer is
    /// [`SETTINGS_LOCK_BUDGET`], which carries its own argument. `Err(reason)` is the wire refusal
    /// ([`Response::Error`], via `AcceptError::Refused`); `Ok` carries the audit raw material.
    ///
    /// ## ⚠ This arm is UNREACHABLE on the shipped deployment, and that is a ruling, not a bug
    ///
    /// Every settings write reaching here on a the build runner box refuses at
    /// `vike_config::SettingsWriteError::Lock` with `EROFS`, before the typed-confirm contract has
    /// bought anything and before any of [`SETTINGS_LOCK_BUDGET`] is spent: the shipped units carry
    /// `ProtectSystem=strict` with a `ReadWritePaths=` grant naming `<project>/settings/state` and
    /// nothing above it, so the daemon's own settings directory is read-only in its own mount
    /// namespace. MEASURED 2026-09-13 inside the live daemon's namespace — `touch
    /// settings/settings.lock` → "Read-only file system", `touch settings/state/.probe` → OK.
    ///
    /// **Do not widen that grant to make this arm work.** Whether a daemon may rewrite the settings
    /// that cap it is an OWNER RULING in flight, and the grant already argues one side of it where
    /// it is declared: *"a daemon that can rewrite its own ceiling has none"*. What this code owes
    /// meanwhile is to stop reading as though the write lands — hence this section, and the
    /// operator-facing half in `vike_config::SettingsWriteError::Lock`'s `Display`, which leads
    /// with the namespace because `ls -ld` from an ordinary shell answers it wrongly. The arm stays
    /// compiled and tested: a container or dev deployment whose settings directory IS writable
    /// reaches all of it, and a ruling that widens the grant would arm it on the CI box with no code
    /// change at all.
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
        // ⚠ THE TYPED CONFIRM IS KEYED ON THE KEY'S SECTION WORD, not on `file`. It used to be
        // `settings_file == SettingsFile::Policy` — which is correct today and is a check that
        // stops being askable the moment the wire stops carrying file names, the half-disarmed
        // confirm `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md` names.
        // `vike_config::is_policy_plane_key` is the ONE predicate the CLI prompt and the GUI's Save
        // gate consult as well, so no surface can answer differently from this one.
        //
        // The two answers cannot disagree for a write that LANDS: `set_setting_within`'s `key_path`
        // refuses a key whose first segment is not the target file's section. A mismatched pair
        // (file `config`, key `policy.max_leverage`) now DEMANDS the confirm and is then refused by
        // that check — strictly the safe direction, and nothing is written either way.
        if vike_config::is_policy_plane_key(key) {
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
        // ⚠ `set_setting_within`, never the no-budget `set_setting`: this runs on a CONNECTION
        // THREAD with a peer on the far end. [`SETTINGS_LOCK_BUDGET`] carries the argument.
        // ⚠ **`RowSync::ReportOnly` — the ONE production caller that may not write a row.**
        // `docs/decisions/0068` rules that no layer sits above 0054's `settings/db` grant, and its
        // *What would reopen this* says what that ruling rests on: the daemon writes no settings
        // row, which "is a property of the current code, not a rule, and nothing enforces it."
        // This arm is the rule and `crates/vike-ops/tests/settings_row_writer_gate.rs` is the
        // enforcement. A control-plane write that reached a row would reopen that record from
        // inside a diff about something else.

        //
        // The fence is DOUBLE and neither half is this comment. (1) ORDERING: `set_setting_within`
        // writes the FILE first and reaches the row sync only on its success, and the `EROFS` this
        // arm hits (see this method's own doc) lands at the LOCK, before any database handle is
        // opened. (2) A PIN: `crates/vike-ops/tests/settings_row_writer_gate.rs` holds the arm each
        // call site passes, so pointing this one at a row means editing that gate on a PR.
        let write = vike_config::set_setting_within(
            dir,
            settings_file,
            key,
            value,
            SETTINGS_LOCK_BUDGET,
            vike_config::RowSync::ReportOnly(
                "the daemon's settings directory is read-only in its own mount namespace, and \
                 `docs/decisions/0068` rules that no layer sits above 0054's `settings/db` grant \
                 — so this is the decided answer rather than an undecided one",
            ),
        )
        .map_err(|e| e.to_string())?;
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

/// Render the LIVE-JOURNAL tearsheet this node's own journal answers (ruling 16) — the body of the
/// [`Request::Tearsheet`] arm, split out so the whole reply is decidable without a socket.
///
/// `seed` / `periods_per_year` are the wire's optionals; `None` means *the caller has no opinion*
/// and resolves to the renderer's own default rather than to a number invented here
/// ([`TEARSHEET_DEFAULT_SEED`] and `vike_report::report::DAILY_PERIODS_PER_YEAR`), so a remote
/// `report` and `vike-report`'s own `tearsheet --journal DIR` over one journal print the same
/// document. The payload is `serde_json::to_string_pretty`, which is the spelling
/// `tearsheet --json` uses: `vike_tradehub_client::remote_handle::tearsheet` hands this text to its
/// caller VERBATIM, so a compact spelling here would make the remote door's bytes differ from the
/// local door's for one set of fills — the exact drift [`Response::Tearsheet`]'s own doc carries
/// the JSON (rather than a typed payload) to avoid.
///
/// # WHICH journal — and the rung this build cannot see
///
/// The directory is resolved by `vike_core::journal_config_from` over
/// [`SettingsShowSource::env`], the daemon's OWN startup `std::env::vars()` sweep. That is the same
/// pure resolver, over the same three variables, that the live core's `CoreConfig::journal` was
/// built from, so this reply cannot name a journal the core is not writing.
///
/// ⚠ **It sees `VIKE_RUN_PROFILE` and `VIKE_JOURNAL_DIR`; it does NOT see `config.journal_dir`.**
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `journal_vars` folds that settings key into the map
/// it hands the core (env winning where both are present), and the folded map is not threaded into
/// this module. So on a daemon journalling purely through `config.toml` this arm answers an ERROR,
/// and the error SAYS SO BY NAME rather than reporting "journaling is off" — a node that is
/// journalling must never be described as one that is not, and the operator who reads this needs to
/// know which of the two facts they are holding. Closing it is one edit and no new parameter: make
/// that function `pub(crate)` and call it here instead of reading `env` directly. It is written as a
/// residual rather than done because `journal_vars`' precedence has exactly one home and a second
/// copy of *"the environment wins"* in this file would be the drift, not the fix.
fn tearsheet_reply(
    settings: Option<&SettingsShowSource>,
    seed: Option<f64>,
    periods_per_year: Option<f64>,
) -> Response {
    // A server constructed without a settings source (possible through `serve(.., None)`; the
    // shipped daemon always passes one) knows nothing about its own environment — the same honest
    // refusal the identity-less `StrategyStatus` and source-less `SettingsShow` arms make, never a
    // fabricated empty tearsheet.
    let Some(src) = settings else {
        return Response::Error(
            "tearsheet unavailable: this node was started without a settings source, so it cannot \
             resolve its own journal directory"
                .into(),
        );
    };
    let Some(journal) = vike_core::journal_config_from(&src.env) else {
        return Response::Error(
            "tearsheet unavailable: this node's environment enables no command journal, so there \
             are no fills to fold (set VIKE_JOURNAL_DIR, or a VIKE_RUN_PROFILE with a \
             [sinks.journal] section, and restart). ⚠ If journaling is configured through \
             config.toml's `journal_dir` instead, THIS BUILD CANNOT SEE IT — the node may well be \
             journalling; run `vike-report`'s `tearsheet --journal <dir>` on the daemon's own box \
             to read it."
                .into(),
        );
    };
    let seed = seed.unwrap_or(TEARSHEET_DEFAULT_SEED);
    let ppy = periods_per_year.unwrap_or(vike_report::report::DAILY_PERIODS_PER_YEAR);
    // Read-only and off the fold: `from_journal` opens the journal's own segment files and folds
    // them through `vike_report`'s reconstruction on THIS connection thread. Nothing touches the
    // core, the publisher or the snapshot cell — the module's hot-path guarantee is intact — and a
    // large journal costs this one peer's thread, never another client's.
    match vike_report::LiveTearsheet::from_journal(&journal.dir, seed, ppy) {
        Ok(sheet) => match serde_json::to_string_pretty(&sheet) {
            Ok(json) => Response::Tearsheet(json),
            // Effectively unreachable for this type — every field is a scalar and serde_json
            // writes a non-finite float as `null` rather than failing — but it is an arm rather
            // than an `unwrap` because a panic here kills the connection thread that is holding a
            // MAX_CONNECTIONS slot, and "the report could not be serialized" is a sentence.
            Err(e) => Response::Error(format!("tearsheet could not be serialized: {e}")),
        },
        Err(e) => Response::Error(format!(
            "tearsheet unavailable: the journal at {} could not be read ({e})",
            journal.dir.display()
        )),
    }
}

/// The starting capital a [`Request::Tearsheet`] carrying no `seed` is folded with.
///
/// It is the SAME number `vike-report`'s `tearsheet --seed` defaults to, and it is spelled here
/// because that default is a `parse_num` argument inside a CLI body rather than a named constant
/// anything can import. ⚠ **If the two ever disagree, the remote and local doors publish different
/// `total_return`/`cagr`/`sharpe` for one journal** — every one of those scales with the equity
/// base — and nothing would say so, because both answers are internally consistent. The fix when
/// that day comes is to name the constant in `vike_report::tearsheet_cli` and delete this one, not
/// to edit this number.
const TEARSHEET_DEFAULT_SEED: f64 = 10_000.0;
/// **WHERE the node's account verbs write, and the fact that this handle EXISTS at all is the
/// barrier's first part.**
///
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §3c Part 1: the
/// capability is an `Option` the BINARY constructs and hands to [`serve`], exactly as
/// `commands: Option<CommandSink>` and `settings: Option<SettingsShowSource>` already are.
///
/// > *"When it is `None` the process contains no path from a frame to the store, the feature string
/// > is absent from `served_features`, and there is nothing to forget to check."*
///
/// That is why this is a handle rather than a boolean beside a check: a refusal somebody has to
/// remember is a refusal somebody can forget, and the shipped default — every box that has not
/// declared the barrier — must be byte-identical to a build without the verb.
///
/// # What the BINARY decides before it builds one
///
/// [`crate::tradehub_cli::account_admin_source`] is the one site, and it answers three questions in
/// order: has the operator DECLARED a barrier (`config.tradehub_account_admin`, three-valued —
/// unset/`off`, `loopback`, `contained`); if `loopback`, does [`bind_exposure`] agree that every
/// resolved address is loopback (a wide bind REFUSES the capability at boot, naming the flag and
/// the address); and is there an `VIKE_TRADEHUB_ADMIN_KEY` to authenticate against. All three must
/// answer yes.
///
/// ⚠ **This server cannot ask the first two itself and must not try.** §3b measures why: `serve`
/// receives an ALREADY-BOUND `TcpListener` and never calls `local_addr`, [`bind_exposure`] runs
/// once in the binary before the listener exists, and the per-frame PEER answers wrongly in both
/// directions (through an SSH tunnel it is loopback and correct; inside a container correctly
/// published to `127.0.0.1` it is the bridge gateway, so a CONTAINED deployment would be refused;
/// and the Telegram surface reaches [`accept_command`] with no peer at all). The exposure is
/// knowable at BOOT and unknowable at the point of decision — unless it is handed in, which is what
/// this type is.
#[derive(Clone, Debug)]
pub struct AccountAdminSource {
    /// `<project>/settings` as the daemon's ONE boot walk resolved it (`vike_boot::Booted`'s
    /// `settings_dir`).
    ///
    /// ⚠ The SETTINGS directory, never a store path: `vike_secrets`'s routers ask
    /// `backend_in` about this directory, which is the same question the daemon's own credential
    /// READ asks — so the verb that lists the rows and the verb that writes one cannot disagree
    /// about which store they are talking about. A store path threaded in separately is exactly how
    /// they could.
    pub settings_dir: std::path::PathBuf,
    /// The barrier the operator DECLARED, carried so the server can say which one is in force
    /// without re-reading a flag. It decides nothing here — the binary already refused to build
    /// this handle if the declaration and the bind disagreed — and it is on the type so that a log
    /// line and a reply can be honest about what is holding the frames up.
    pub barrier: AccountBarrier,
}

/// **The operator's DECLARATION about this listener's confidentiality**, which is a three-valued
/// key rather than a boolean, and 0065 §3c Part 3 is the argument.
///
/// The node wire is PLAINTEXT and authenticates the CONNECTION rather than each frame (this
/// module's own doc), so confidentiality comes entirely from REACHABILITY. The process can CHECK
/// exactly one shape of that and cannot check the other:
///
/// | value | what the operator asserts | what the process does |
/// |---|---|---|
/// | unset / `off` | nothing | no capability, no key read, no feature advertised — byte-identical to a build without the verb |
/// | `loopback` | *this listener is on loopback and reached through a tunnel* | **CHECKS it** — armed only when [`bind_exposure`] classifies every resolved address as loopback; a wide bind REFUSES the capability at boot |
/// | `contained` | *the barrier is outside this process* — a `127.0.0.1`-published container port, a private interface | armed regardless of the bind; the process does NOT verify it and logs exactly what has been asserted |
///
/// ⚠ **A boolean would collapse the two assertions into one word and make the CHECKABLE case
/// uncheckable**, which is the whole of what this split buys. `loopback` is the only barrier the
/// process can check, so it is the only one that is checked — and the refusal lands precisely where
/// there is evidence for it.
///
/// ⚠ **`contained` is not a warning wearing a value's clothes.**
/// `docs/decisions/0026-containerisation-additive-backend-image.md` refused to let the daemon INFER
/// a container's containment (*"a `/.dockerenv` probe that auto-allowed the bind would be the daemon
/// inferring consent from its own environment"*); it did not refuse to let an operator DECLARE it.
/// The declaration sits in the same class as `flags.tradehub_allow_public_bind` — made once on the
/// box, in a file the daemon cannot rewrite from the wire on any shipped deployment
/// (`ProtectSystem=strict` with `ReadWritePaths` naming `settings/state` alone, so a settings write
/// refuses with `EROFS` inside its own namespace).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountBarrier {
    /// The listener is on loopback and reached through a tunnel. CHECKED against [`bind_exposure`].
    Loopback,
    /// The barrier is outside this process. Asserted, never verified.
    Contained,
}

impl AccountBarrier {
    /// The settings-file spelling — one derivation, so the parser and every message agree.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AccountBarrier::Loopback => "loopback",
            AccountBarrier::Contained => "contained",
        }
    }

    /// Parse the three-valued declaration. `None` for unset, blank, `off`, and for **anything
    /// unrecognised**.
    ///
    /// ⚠ **An unrecognised value is OFF, not an error and not a guess.** A typo'd `loopbak` must
    /// never arm a credential surface, and the safe direction is the one where the capability does
    /// not exist. The binary logs the unrecognised value so an operator who typed one is told
    /// rather than left believing the barrier is up — a refusal that starts nothing is worse here
    /// than a daemon that keeps trading headless, which is the same trade
    /// [`BindDecision::Refuse`] makes.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Option<AccountBarrier> {
        match raw.map(str::trim)? {
            "loopback" => Some(AccountBarrier::Loopback),
            "contained" => Some(AccountBarrier::Contained),
            _ => None,
        }
    }
}

impl AccountAdminSource {
    /// Answer one [`Request::Account`] — the ONE place a frame becomes a store write.
    ///
    /// # The CEREMONY, and whose shape it is
    ///
    /// `apply_set_setting`'s policy contract verbatim, because that is the precedent this surface
    /// was told to follow rather than invent one: the two destructive verbs
    /// ([`AccountVerb::Remove`], [`AccountVerb::SetCredential`]) are REFUSED unless `confirm`
    /// equals the exact thing being changed — the row id, or the credential key name. **Missing and
    /// mismatched get DISTINCT messages, each naming the expected spelling**, which is that arm's
    /// split. The client's job is to make the operator TYPE it and never pre-fill it; this
    /// method's job is to refuse anything else, so no client can quietly skip it.
    ///
    /// `vike_tradehub_client::wire::AccountVerb::required_confirm` is the ONE derivation of what
    /// the confirm must be, consulted by the client that prompts and by this method that refuses —
    /// so the two cannot answer differently about what the ceremony is.
    ///
    /// # What never leaves
    ///
    /// No reply carries a credential value on any path, and the refusals quote nothing the caller
    /// sent. `vike_secrets`' own refusals are safe to print by construction
    /// (`DbErrorKind::AccountHasCredentials` names key NAMES from a statement with no `value`
    /// column; `DbErrorKind::AccountLabelMalformed` echoes no token at all), and the one refusal
    /// composed here — an invalid credential KEY — names the key, which is not a secret, and never
    /// the value.
    ///
    /// # Errors
    /// `Err(reason)` is answered on the wire as [`Response::Error`]. Every one of them wrote
    /// nothing.
    fn apply(&self, req: &AccountRequest, actor: &AccountActor<'_>) -> Result<Response, String> {
        // ⚠ THE CEREMONY FIRST, before the store is opened and before anything is validated —
        // `apply_set_setting`'s ordering, so a refused ceremony costs no lock and cannot be
        // distinguished from a refused one by timing what the store did.
        if let Some(expected) = req.verb.required_confirm() {
            match req.confirm.as_deref() {
                None => {
                    return Err(format!(
                        "`{}` changes something this node cannot put back — it must carry the \
                         typed confirm: re-send with confirm set to the exact value `{expected}`",
                        req.verb.word()
                    ));
                }
                Some(c) if c.trim() != expected => {
                    return Err(format!(
                        "account confirm mismatch: the confirm must equal the exact value \
                         `{expected}` — nothing was written. (What was sent is deliberately not \
                         quoted back: on this verb the field beside it carries a credential.)"
                    ));
                }
                Some(_) => {}
            }
        }

        match &req.verb {
            AccountVerb::List => self.list(),
            AccountVerb::Add { venue, tier, label } => {
                // The venue roster and the tier vocabulary are checked HERE rather than in the
                // store: `vike-secrets` declares no `vike-*` dependency and cannot see
                // `vike_model::VENUES`. The refusals print the roster, which is what makes them
                // actionable.
                if !vike_model::venues::VENUES.contains(&venue.as_str()) {
                    return Err(format!(
                        "unknown venue '{venue}' — nothing was written. The roster is: {}",
                        vike_model::venues::VENUES.join(", ")
                    ));
                }
                if !vike_secrets::ACCOUNT_TIERS.contains(&tier.as_str()) {
                    return Err(format!(
                        "unknown tier '{tier}' — nothing was written. It must be one of: {}. \
                         `paper` IS one of them: it is the account a {{VENUE}}_SIM_* credential \
                         mints, not the policy.toml CEILING of the same name.",
                        vike_secrets::ACCOUNT_TIERS.join(" | ")
                    ));
                }
                self.write(
                    vike_secrets::AccountEdit::Create { venue, tier, label: label.as_deref() },
                    Some(format!(
                        "⚠ this arms NOTHING. A row is not a ceiling: policy.venues.{venue} is \
                         read ABOVE the credential store by the mount, so this venue stays PAPER \
                         until that line says otherwise."
                    )),
                    actor,
                )
            }
            // ⚠ **A WARNING, WHERE `docs/decisions/0065` §5 ASKS FOR A REFUSAL — a declared
            // deviation, not an oversight.** That record says of `rename`: *"plus a refusal when
            // the OLD label is named in `policy.toml`'s `[accounts.<venue>]` … with a REMOTE verb
            // the operator who renames and the operator who restarts need not be the same person,
            // so the refusal belongs at the edit."*
            //
            // What is here instead is this note, rendered on every rename that changed something,
            // naming the exact policy key. What the refusal would take, and why it was not built
            // with the rest: the label space `[accounts.<venue>]` addresses is the CREDENTIAL KEY
            // NAME grammar (`vike_model::account_keys::accounts_in_store` enumerates it), not the
            // `account` ROW's `label` column — the two agree on a labelled row and disagree on
            // every unlabelled one, and that table is the `accounts` field of
            // `crates/vike-config/src/policy.rs`'s `PolicyPatch`, which that file still documents
            // as parsed, validated, stored and *folded by nothing*. A refusal keyed on it would
            // therefore be refusing against a ceiling table whose consumer is mid-rollout, and
            // getting the label space wrong would refuse a LEGITIMATE rename — strictly worse than
            // the warning, because a refused rename has no override on this surface at all.
            //
            // What catches the ONE-STEP hazard: `vike_run::refuse_unarmed_mount_accounts` refuses
            // the whole node to start, `vike_mount::arming`'s `Block::AccountNotInStore` reports
            // it, and the mount itself answers `DukascopyRefusal::NoSuchAccount`. Three independent
            // catches, all at the next restart.
            //
            // ⚠ **THIS ARM USED TO CLAIM THOSE THREE MADE A MISROUTE IMPOSSIBLE — "an outage, never
            // a misroute". THAT IS FALSE, and it was the sentence an operator would act on.**
            // Measured 2026-09-17. All three fire on a label that STOPS resolving; none of them
            // looks at a label that now resolves to a DIFFERENT row. Two accepted renames reach
            // that state with the policy file untouched:
            //
            //   0. row A = DUKASCOPY_DEMO1_* keys; row B = DUKASCOPY_DEMO2_* keys labelled `ALT`;
            //      `policy.accounts.dukascopy.ALT` armed → orders reach Dukascopy Europe IBS AS.
            //   1. rename B to `OLD` — `AccountKeysPinTheLabel` looks for keys ending in `__ALT`
            //      and dukascopy has none (it discriminates by a token INSIDE the base name,
            //      `DukascopyAccount::key_prefix`), so the one guard that could fire is
            //      STRUCTURALLY unable to on the only venue where a label selects a BROKER.
            //   2. rename A to `ALT` — `AccountLabelTaken` sees `ALT` free; `AccountLabelHeldAsBook`
            //      sees no active row booked `ALT`.
            //   3. restart → `resolve_account` matches row A on `r.label` → Dukascopy Bank SA.
            //
            // All three catches find an armed, unambiguous, resolvable account, so none fires.
            // `DukascopyAccount::broker`'s own doc states the stake: *a fallback would route an
            // order to a broker nobody chose.* The guard 0065 §5 specifies is what closes this; it
            // is NOT built here, and this comment is the honest statement of that gap rather than
            // the reassurance that used to sit in its place. The warning below is therefore the
            // only thing in front of the two-step case, and a warning is not a guard.
            AccountVerb::Rename { id, label } => self.write(
                vike_secrets::AccountEdit::Rename { id: *id, label: label.as_deref() },
                Some(
                    "⚠ if policy.toml names this account by its OLD label, that line stops \
                     resolving and a mount addressing it is REFUSED at the next restart. Update \
                     the policy line too. ⚠ And if you then give the OLD label to a DIFFERENT \
                     account of the same venue, nothing refuses anything: the policy line resolves \
                     again, to the other account. On dukascopy the two demo accounts are different \
                     LEGAL ENTITIES, so that is an order routed to a broker nobody chose. Check \
                     which row the label names before you restart."
                        .to_string(),
                ),
                actor,
            ),
            AccountVerb::SetActive { id, active } => self.write(
                vike_secrets::AccountEdit::SetActive { id: *id, active: *active },
                (!*active).then(|| {
                    "⚠ a RUNNING daemon does not notice: the arming snapshot is read ONCE at boot, \
                     so its engines keep their credentials and keep trading until it restarts. The \
                     row and its credential keys are still there, which is what makes this \
                     reversible."
                        .to_string()
                }),
                actor,
            ),
            AccountVerb::Remove { id } => {
                self.write(vike_secrets::AccountEdit::Remove { id: *id }, None, actor)
            }
            AccountVerb::SetCredential { key, value } => self.set_credential(key, value, actor),
            AccountVerb::SetBook { id, venue_account_id, replace } => {
                self.set_book(*id, venue_account_id.as_deref(), *replace, actor)
            }
        }
    }

    /// **WRITE one row's `venue_account_id`** — the wire twin of `vike-cli secrets set-book`.
    ///
    /// ⚠ **It does NOT go through [`Self::write`] / `AccountEdit`, and that is deliberate rather
    /// than an inconsistency.** `crates/vike-ops/tests/credential_writer_gate.rs`'s
    /// `GROWTH_GUIDANCE` already classified this column once, when `set_venue_account_id` was
    /// admitted: the rule it attached was *"a SECOND FUNCTION for a second column is the shape to
    /// refuse here: this one grew a parameter instead."* Routing the book through `AccountEdit`
    /// would be exactly that second function, in the other direction — a second way to write a
    /// column that already has one, free to disagree with it about the refusal below.
    ///
    /// So this calls the SAME `vike_secrets::set_venue_account_id_in` the CLI calls, with the same
    /// arguments, and every refusal it raises is raised identically on both surfaces.
    ///
    /// ⚠ **`BookSource::Operator`, never `Handshake`.** A value that arrived over this wire was
    /// typed by a human into a GUI; stamping it as a handshake would make a hand-entered row
    /// indistinguishable from one a venue confirmed, which is the false confidence
    /// `vike_model::account_confirmation`'s whole module exists to remove. The CLI's own call site
    /// carries the same note for the same reason.
    fn set_book(
        &self,
        id: i64,
        venue_account_id: Option<&str>,
        replace: bool,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        let done = vike_secrets::set_venue_account_id_in(
            &self.settings_dir,
            id,
            venue_account_id,
            replace,
            vike_secrets::BookSource::Operator,
        )
        .map_err(|e| e.to_string())?;

        let row = WireAccountRow {
            id: done.before.id,
            venue: done.before.venue.clone(),
            tier: done.before.tier.clone(),
            label: done.before.label.clone(),
            // The column AFTER the write — `before` is what the transaction found, and a reply that
            // echoed it would tell the operator their write had not happened.
            venue_account_id: done.venue_account_id.clone(),
            active: done.before.active,
            last_verified_at: done.before.last_verified_at.clone(),
            keys: Vec::new(),
        };
        let note = (!done.changed).then(|| {
            "the row already named exactly this book, so nothing was written. Re-asserting a known \
             book is not a mistake and is not refused."
                .to_string()
        });
        self.journal_book_write(&done, actor);
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
            verb: if venue_account_id.is_some() { "set-book" } else { "clear-book" }.to_string(),
            row: Some(row),
            changed: done.changed,
            note,
        })))
    }

    /// The listing. **NO value is selected anywhere on this path** — `read_accounts` never touches
    /// the `credential` table at all, and `read_account_keys`' statement is
    /// `SELECT name, field, account_id`.
    fn list(&self) -> Result<Response, String> {
        let store = vike_secrets::db_path_in(&self.settings_dir);
        let accounts =
            vike_secrets::resolve_accounts_in(&self.settings_dir).map_err(|e| e.to_string())?;
        let rows = match &accounts {
            vike_secrets::Accounts::Known(rows) => rows,
            vike_secrets::Accounts::Unanswerable(why) => {
                return Err(format!(
                    "{why} — so this node has no account table to list. Move the box into the \
                     settings database first: `vike-cli secrets migrate`."
                ));
            }
        };
        let keyed = vike_secrets::resolve_account_keys_in(&self.settings_dir)
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        let rows = rows
            .iter()
            .map(|a| WireAccountRow {
                id: a.id,
                venue: a.venue.clone(),
                tier: a.tier.clone(),
                label: a.label.clone(),
                venue_account_id: a.venue_account_id.clone(),
                active: a.active,
                last_verified_at: a.last_verified_at.clone(),
                keys: keyed.get(&a.id).map(|k| k.names.clone()).unwrap_or_default(),
            })
            .collect();
        Ok(Response::AccountList(Box::new(WireAccountList {
            store: store.display().to_string(),
            rows,
        })))
    }

    /// One lifecycle write, through `vike_secrets::edit_account_in` — the Backend-aware router,
    /// never the db function directly, so this exercises the store choice as well as the write.
    ///
    /// ⚠ Every refusal it can return is safe to put on the wire verbatim: `vike-secrets`' account
    /// refusals name ids, venues, tiers and credential key NAMES, and the one that could have
    /// echoed an operator-supplied token (`AccountLabelMalformed`) deliberately does not.
    fn write(
        &self,
        edit: vike_secrets::AccountEdit<'_>,
        note: Option<String>,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        let done =
            vike_secrets::edit_account_in(&self.settings_dir, edit).map_err(|e| e.to_string())?;
        let row = done.after.as_ref().or(done.before.as_ref()).map(|a| WireAccountRow {
            id: a.id,
            venue: a.venue.clone(),
            tier: a.tier.clone(),
            label: a.label.clone(),
            venue_account_id: a.venue_account_id.clone(),
            active: a.active,
            last_verified_at: a.last_verified_at.clone(),
            keys: done.keys.clone(),
        });
        // The durable record, before the reply: a completed write with no ledger line is the shape
        // `run_set_book`'s ordering note argues against.
        self.journal_lifecycle(&done, actor);
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
            verb: done.verb.to_string(),
            // A REMOVE left no row; `after`/`before` above already falls back, and a row is still
            // reported for it so the reply names what went.
            row: done.after.is_none().then(|| row.clone()).flatten().or(row),
            changed: done.changed,
            note: note.filter(|_| done.changed),
        })))
    }

    /// **The verb that carries a credential VALUE.**
    ///
    /// It is a CALL SITE of `vike_secrets::save_credentials_to_store` — the ONE upsert — never a
    /// second writer, which is `docs/decisions/0036`'s reason 1 answered on the schema that exists.
    /// The key NAME is validated against `vike_model::credential_keys` exactly as
    /// `vike-cli secrets set` validates it (reason 3, reused verbatim), and the classifier is the
    /// same `vike_bridge_core::credentials::classify_credential_name` every other caller passes.
    ///
    /// ⚠ **The value's whole life in this process**: one `String` inside the deserialized frame, on
    /// this connection's own thread, for the length of this call — handed to the upsert, where it
    /// becomes a bind parameter. Nothing caches it, no response carries it, no log line and no
    /// error message reaches it, and `AccountRequest`'s hand-written `Debug` prints `<set>` in its
    /// place. ⚠ This workspace does not ZEROIZE, and 0065 §4 declares that residual rather than
    /// waiving it: the value also exists in the frame read buffer and in whatever `serde_json`
    /// allocated parsing it, and Rust drops without wiping.
    fn set_credential(
        &self,
        key: &str,
        value: &str,
        actor: &AccountActor<'_>,
    ) -> Result<Response, String> {
        // ⚠ The refusal names the KEY and never the value — and the suggestions are SUPPRESSED for
        // a labelled key, because the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a
        // DIFFERENT ACCOUNT's live key. `lookup_keys` is the same validator the CLI uses.
        if !is_settable_credential_key(key) {
            return Err(format!(
                "'{key}' is not a credential key this workspace reads, so nothing was written — a \
                 typo'd name would sit in the store forever with nothing looking for it. \
                 `vike-cli secrets template` prints the grid."
            ));
        }
        // ⚠ An ABSENT store is REFUSED, not created. `docs/decisions/0036`'s rule, and its sharpest
        // instance on this surface: the mere EXISTENCE of the settings database is the whole of
        // `vike_secrets::Backend`'s per-run choice, so a writer that created one would make every
        // credential in `secrets.env` unread on that box in the same act.
        if matches!(vike_secrets::backend_in(&self.settings_dir), vike_secrets::Backend::Files)
            && !vike_secrets::secrets_path_in(&self.settings_dir).is_file()
        {
            return Err(
                "this node has no credential store at all, and nothing here will create one: a \
                 store is an operator's only copy of their live venue keys. Create it on the box \
                 (`vike-cli secrets path` prints where), then retry."
                    .to_string(),
            );
        }
        let backend = vike_secrets::save_credentials_to_store(
            &self.settings_dir,
            vike_secrets::Table::Credential,
            &[(key.to_string(), value.to_string())],
            Some(&vike_bridge_core::credentials::classify_credential_name),
        )
        .map_err(|e| e.to_string())?;
        // The durable record — `Change::credential_write`, whose signature takes NO value
        // parameter, which is the enforcement rather than a convention. The actor is the WIRE's,
        // carrying the non-secret key fingerprint this connection authenticated under.
        self.journal_credential(key, actor);
        let store = match backend {
            vike_secrets::Backend::Database(db) => db.display().to_string(),
            vike_secrets::Backend::Files => {
                vike_secrets::secrets_path_in(&self.settings_dir).display().to_string()
            }
        };
        Ok(Response::AccountWritten(Box::new(WireAccountWritten {
            verb: "set-credential".to_string(),
            row: None,
            changed: true,
            note: Some(format!(
                "{key} written to {store}. ⚠ this arms NOTHING on its own — policy.venues is read \
                 ABOVE the credential store by the mount, and a RUNNING daemon keeps its \
                 boot-time credentials until it restarts."
            )),
        })))
    }
}

/// **Is `key` a credential name this workspace actually reads?** — the wire's half of
/// `docs/decisions/0036`'s reason 3 (*a typo'd key NAME silently creates a key nothing reads*),
/// reused verbatim rather than re-derived.
///
/// Two shapes are settable, and they are the same two `vike-cli secrets set` accepts:
///
/// * a name in `vike_model::credential_keys::lookup_keys` — the grid;
/// * a LABELLED account's key, `{BASE}__{LABEL}`, whose BASE resolves. The grammar is
///   `vike_model::account_keys`': split at the FIRST `ACCOUNT_SEPARATOR`, which is a DOUBLE
///   underscore — so a single-underscore near-miss (`..._API_KEY_ALT`, which nothing reads) is NOT
///   one of these and is correctly refused.
///
/// ⚠ **The refusal above deliberately offers NO suggestions**, which is the one place this differs
/// from the CLI's message: the CLI computes nearest names and SUPPRESSES them for a labelled key,
/// because the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a DIFFERENT ACCOUNT's live key.
/// On a remote surface the same hazard applies to every shape, and there is no terminal to print a
/// grid into — so the message names the command that prints one instead.
fn is_settable_credential_key(key: &str) -> bool {
    if vike_model::credential_keys::lookup_keys().iter().any(|k| k == key) {
        return true;
    }
    match key.split_once(vike_model::account_keys::ACCOUNT_SEPARATOR) {
        Some((base, label)) => {
            vike_model::credential_keys::key_owner(base).is_some()
                && vike_model::account_keys::AccountLabel::parse(label).is_ok()
        }
        None => false,
    }
}

/// **WHO performed an account write**, resolved once per connection and threaded in — the peer, the
/// granted scope and the NON-SECRET fingerprint of the key whose mac verified.
///
/// The same three cells [`accept_command`] already records for a settings write, and they exist
/// here for the reason `docs/decisions/0065` §4.3 gives: `vike_model::change_journal::Actor::Wire`
/// *"already carries a stable NON-SECRET key fingerprint, taken under a domain separator that is
/// deliberately not one of the two protocol signing domains, so a remote credential write is
/// attributable to a KEY without the key. That is an existing asset to use, not a thing to build."*
///
/// A borrowed struct rather than three parameters, so a call site cannot transpose the peer and the
/// key id — two `Option<&str>` cells that render identically when they are wrong.
pub struct AccountActor<'a> {
    /// `TcpStream::peer_addr` as [`handle_connection`] bound it, rendered. `None` on a surface with
    /// no socket.
    pub peer: Option<&'a str>,
    /// The granted scope's word — always `admin` on this path, carried rather than assumed so a
    /// widening shows up in the ledger instead of being invisible in it.
    pub scope: &'a str,
    /// `NodeKeys::key_id` for the scope that authenticated. `None` is unreachable on this path (the
    /// scope was granted, so its key is non-empty) and is carried honestly rather than unwrapped —
    /// an absent key must record NO id, never an invented one.
    pub key_id: Option<&'a str>,
}

impl AccountActor<'_> {
    fn to_actor(&self) -> vike_model::change_journal::Actor {
        vike_model::change_journal::Actor::wire(self.peer, Some(self.scope), self.key_id)
    }
}

impl AccountAdminSource {
    /// The durable CHANGE JOURNAL for this daemon's project — `<settings_dir>/state/changes`.
    ///
    /// Derived from [`AccountAdminSource::settings_dir`], the SAME already-resolved directory the
    /// write itself lands in, rather than from a fresh walk — so the ledger and the store it
    /// describes can never resolve to two different projects. [`SettingsShowSource::change_journal`]
    /// is the same derivation for the settings plane.
    fn change_journal(&self) -> vike_model::change_journal::ChangeJournal {
        use vike_model::change_journal::{ChangeJournal, Proc};
        use vike_model::state_path::STATE_SUBDIR;

        // `Proc::current` reads `current_exe`, so it is resolved ONCE per process rather than per
        // write. Neither the value nor the read can change during a run.
        static PROCESS: std::sync::OnceLock<Proc> = std::sync::OnceLock::new();
        let process = PROCESS.get_or_init(|| Proc::current(env!("CARGO_PKG_VERSION")));
        ChangeJournal::in_state_dir(&self.settings_dir.join(STATE_SUBDIR), process.clone())
    }

    /// Record one lifecycle write. Key NAMES only, and the SIGNATURE is the enforcement:
    /// `Change::account_lifecycle` takes no value parameter, exactly as `credential_write` and
    /// `account_book` do not.
    /// Record one BOOK write into the durable ledger — the wire twin of `vike-cli secrets`'
    /// `record_book_write`, through the same `Change::account_book` constructor so the two surfaces
    /// cannot produce differently-shaped rows for the same act.
    ///
    /// ⚠ **Nothing is recorded when nothing changed**, and the rule is sharper than it looks: a
    /// ledger line for a no-op reads as a RE-POINTING that did not happen, which is the one thing a
    /// reader of this channel must not be told. `changed` is a claim about the BOOK alone.
    fn journal_book_write(&self, done: &vike_secrets::BookWrite, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome};

        if !done.changed {
            return;
        }
        let change = Change::account_book(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            done.before.id,
            &done.before.venue,
            &done.before.tier,
            done.before.venue_account_id.as_deref(),
            // `None` is a CLEAR — old present, new absent — which `AccountBookTarget::cleared`
            // reads. A repair therefore shows as *cleared, assigned* rather than as one edit.
            done.venue_account_id.as_deref(),
        );
        self.append(&change, "account book");
    }

    fn journal_lifecycle(&self, done: &vike_secrets::AccountWrite, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome};

        // Nothing to record when nothing changed: a ledger line for a no-op reads as an edit that
        // did not happen, which is `vike-cli`'s `record_book_write` rule and for its reason.
        if !done.changed {
            return;
        }
        let Some(row) = done.after.as_ref().or(done.before.as_ref()) else { return };
        let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
        let change = Change::account_lifecycle(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            done.verb,
            row.id,
            &row.venue,
            &row.tier,
            done.before.as_ref().and_then(|b| b.label.as_deref()),
            done.after.as_ref().and_then(|a| a.label.as_deref()),
            done.after.as_ref().is_some_and(|a| a.active),
            &keys,
        );
        self.append(&change, "account lifecycle");
    }

    /// Record one credential write. ⚠ `Change::credential_write` takes NO value parameter — that is
    /// the enforcement, and it is why this function cannot leak one however it is called.
    fn journal_credential(&self, key: &str, actor: &AccountActor<'_>) {
        use vike_model::change_journal::{Change, Outcome, TIER_UNTIERED, VENUE_MULTI};

        // ⚠ The venue/tier cells are the DOCUMENTED spellings for a write this surface cannot
        // decompose rather than invented ones: this verb takes a KEY NAME, and deriving a venue
        // from it would be a second classifier beside
        // `vike_bridge_core::credentials::classify_credential_name` — the exact duplication
        // `vike_secrets`' injected-classifier seam exists to avoid. The key NAME is in the record
        // and is what a reader actually searches for.
        let change = Change::credential_write(
            Outcome::Applied,
            actor.to_actor(),
            vike_secrets::DB_FILE,
            VENUE_MULTI,
            TIER_UNTIERED,
            &[key],
        );
        self.append(&change, "credential write");
    }

    /// Append, and report a failure at `error` WITHOUT failing the call.
    ///
    /// The store write already happened, so reporting failure would send a caller down an error
    /// path for a write that succeeded — `vike_connections::save_credentials_journalled`'s ordering
    /// rule, and `error` is the one level `deploy/vike-tradehub-project.service`'s
    /// `VIKE_LOG_FILE_LEVEL=warn` still lets through.
    fn append(&self, change: &vike_model::change_journal::Change, what: &str) {
        let journal = self.change_journal();
        if let Err(e) = journal.append(vike_model::now_ms(), change) {
            tracing::error!(
                error = %e,
                dir = %journal.dir().display(),
                "vike-tradehub node: {what} NOT recorded to the change journal (the write DID land)"
            );
        }
    }
}

/// **The account plane's ADMISSION decision — part 2 of `docs/decisions/0065`'s barrier, split out
/// so it is decidable without a socket.**
///
/// This is the one place that says whether a frame reaching [`Request::Account`] may proceed, and it
/// is a free function for a measured reason rather than a stylistic one: `Request::Account` has
/// exactly ONE construction site in the tree — the arm that consumes it — so nothing builds the
/// frame and no test could drive the decision where it used to live, INSIDE that arm's `match`. A
/// 2026-09-17 mutation proved the cost: widening the accepting arm to `(Some(src), _)` and deleting
/// the refusal — i.e. any authenticated peer, Observe included, reaching `SetCredential` — left
/// 2911 tests green. 0065 calls this half load-bearing (it is what keeps the key a desktop carries
/// to place orders from being the key that writes key material), and it had no ratchet under it.
///
/// `has_capability` is `accounts.is_some()`, part 1 of the barrier: a box that DECLARED nothing
/// holds no writer, so the frame is refused because there is nothing to refuse WITH. The two
/// refusals are deliberately DIFFERENT strings — an absence and an authorization failure are not the
/// same fact, and an operator debugging one must not be sent looking for the other.
///
/// Part 3 (confidentiality) is decided at BOOT and is not knowable here, so it is carried on the
/// handle rather than asked at the frame — see [`AccountAdminSource`]'s own doc.
fn account_admission(has_capability: bool, scope: Scope) -> Result<(), String> {
    if !has_capability {
        // ⚠ The capability does not exist. The message names the DECLARATION rather than a flag to
        // flip, because flipping it is not sufficient: the key has to exist too, and on `loopback`
        // the bind has to agree.
        return Err("account administration is not armed on this node: it holds no account \
                    writer at all. It is armed by DECLARING the barrier this wire's \
                    confidentiality comes from — config.toml `tradehub_account_admin` = \
                    \"loopback\" (checked against the bind) or \"contained\" (asserted by the \
                    operator) — plus a VIKE_TRADEHUB_ADMIN_KEY in the node-key store. This \
                    node advertised no `account-verbs` capability, which is how a conforming \
                    client knows without asking."
            .into());
    }
    if scope != Scope::Account {
        // ⚠ The capability EXISTS and this peer is not Admin. Named as an authorization refusal
        // rather than as an absence, for the reason above.
        return Err("account administration requires the ADMIN scope — a Control key cannot \
                    reach key material on this node. Authenticate with \
                    VIKE_TRADEHUB_ADMIN_KEY."
            .into());
    }
    Ok(())
}

/// **Does this node hold a key for the scope this peer is CLAIMING?** — the handshake's
/// closed-gate check, split out so it is decidable without a socket.
///
/// ⚠ **Extracted for exactly the reason [`account_admission`] was**, and that function's doc
/// carries the measurement: a decision living inside [`run_handshake`] cannot be driven by any
/// test, because `run_handshake` takes a `&mut TcpStream` and there is no seam to hand it
/// anything else. Before this split, `crates/vike-tradehub/tests/` contained no test naming
/// `run_handshake` or `Scope::Account` at all — so the ONE check standing between a peer's CLAIM of
/// admin and the account plane was covered by nothing, on the binary that signs orders.
///
/// `docs/decisions/0070` names this the FIRST of the account boundary's two layers: the MAC
/// verified below folds the scope tag, so a peer cannot claim `Admin` without the admin key — and
/// this gate refuses the claim one step earlier, without consulting a key at all, on a node that
/// holds none. That ordering is the "closed gate" property: an absent key is not an empty key to
/// compare against, it is a scope that cannot be reached.
///
/// Returns the AuthDenied `reason` verbatim on refusal, so the wire text has one home.
fn scope_admission(scope: Scope, keys: &NodeKeys) -> Result<(), &'static str> {
    match scope {
        Scope::Account if !keys.has(Scope::Account) => {
            Err("account administration is not armed on this node")
        }
        Scope::Write if !keys.has(Scope::Write) => Err("control disabled on this node"),
        _ => Ok(()),
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
#[allow(clippy::too_many_arguments)] // this list IS the daemon's composition surface, and each
// capability is an `Option` the BINARY decides — hiding them in a struct would move the argument
// for each one away from the doc that makes it.
pub fn serve(
    listener: TcpListener,
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<SettingsShowSource>,
    accounts: Option<AccountAdminSource>,
    datahub_advertise: Option<String>,
) -> io::Result<()> {
    serve_with_link_policy(
        listener,
        publisher,
        keys,
        commands,
        limits,
        settings,
        accounts,
        datahub_advertise,
        LinkPolicy::PRODUCTION,
    )
}

/// [`serve`] with the per-connection [`LinkPolicy`] as a parameter — the seam the unit tests drive,
/// so "an authed connection outlives the handshake bound", "an idle subscriber is heartbeaten" and
/// "a subscriber that stops reading is closed" are proven at ~150–1000x scale instead of in
/// minutes. Crate-private with exactly one production caller ([`serve`], which passes
/// [`LinkPolicy::PRODUCTION`]): two of those numbers are the server's half of a contract the client
/// crate owns, and a caller that could pick its own would be able to break that contract from one
/// side.
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
    accounts: Option<AccountAdminSource>,
    datahub_advertise: Option<String>,
    policy: LinkPolicy,
) -> io::Result<()> {
    let live = Arc::new(AtomicUsize::new(0));
    // Arc'd ONCE here rather than cloned per connection: the source carries the whole startup env
    // sweep, and every connection reads it immutably.
    let settings = settings.map(Arc::new);
    // ⚠ **THE ACCOUNT CAPABILITY, and its being an `Option` here is the barrier's first part.**
    // `None` — every box that has not DECLARED a barrier, which is every shipped box today — means
    // this process holds no path from a frame to the credential store at all, and `served_features`
    // advertises nothing. The verb is then refused because there is nothing to refuse WITH, which
    // is a different and stronger thing than a check somebody has to remember
    // (`docs/decisions/0065` §3c Part 1). Arc'd once, like the settings source, and read immutably
    // by every connection.
    let accounts = accounts.map(Arc::new);
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
                let accounts = accounts.clone();
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
                        accounts,
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

    /// **The RATE token alone** — `Some(reason)` ⇒ REFUSE, `None` ⇒ allow.
    ///
    /// For a verb that is not a [`WireCommand`] at all and therefore has no notional to size: the
    /// account-admin plane (`docs/decisions/0065`). A flood is still a flood — this surface opens
    /// a SQLite transaction per frame — and the cap being n/a is the `SetSetting` vetting decision
    /// verbatim: the one policy key it enforces is defined over ONE ORDER's notional.
    ///
    /// ⚠ It shares the CONNECTION's bucket rather than owning a second one, so an Admin peer
    /// cannot spend an account-verb budget and an order budget at once.
    pub fn vet_rate(&mut self) -> Option<String> {
        (!self.take_token()).then(|| {
            format!("rate limited: control commands capped at {}/s on this node", self.rate_per_sec)
        })
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
#[allow(clippy::too_many_arguments)] // see `serve_with_link_policy`'s note — the policy is one
// more per-connection input on a function that already takes the daemon's whole composition
// surface, and the account capability is one `Option` more of exactly that kind.
fn handle_connection(
    mut stream: TcpStream,
    publisher: PublisherHandle,
    keys: NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<Arc<SettingsShowSource>>,
    accounts: Option<Arc<AccountAdminSource>>,
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
    let scope = match run_handshake(
        &mut stream,
        &keys,
        peer,
        datahub_advertise.as_deref(),
        accounts.is_some(),
    ) {
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
    // The peer as a STRING, rendered once — the change journal takes `Option<&str>`, and
    // re-rendering it per write would be a second spelling of the cell an audit record is read by.
    let peer_str = peer.map(|p| p.to_string());

    // Control-command limits (a no-op for an Observe peer, which never reaches the Command arm):
    // this connection's own token bucket over the server-lifetime config.
    let mut limits = ControlLimits::new(limits);
    // ONE-SHOT latch for the "this node cannot check where your command is going" warning — see its
    // emission in the `Request::Command` arm. Per CONNECTION, not per order, and not per process:
    // a new control link is a new operator session that has not been told.
    let mut unrouted_warned = false;

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
                // THE PUSH WRITE BOUND, applied at the exact transition it governs: from here on
                // this thread only writes, so neither read policy above bounds anything any more,
                // and without this a peer that keeps its socket open and stops reading parks the
                // thread in `write_all` for as long as it likes (`PUSH_WRITE_TIMEOUT`'s doc). Fatal
                // on failure for the same reason the authed-policy replacement is: carrying on
                // would run the writer unbounded, which is precisely the defect.
                if let Err(e) = stream.set_write_timeout(policy.push_write_timeout) {
                    tracing::warn!(
                        ?peer, error = %e,
                        "vike-tradehub observe: could not apply the push write bound, closing"
                    );
                    return;
                }
                tracing::info!(?peer, ?topics, "vike-tradehub observe: subscribed (push mode)");
                run_push_writer(&mut stream, &publisher, peer, policy);
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
                if scope != Scope::Write {
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
                // (c.1) THE ENGINE ROSTER this command's address is checked against — read off the
                // publisher's snapshot cell PER COMMAND rather than captured at connect, because a
                // node can MOUNT an engine at runtime (`WireCommand::MountStrategy`) and a roster
                // frozen at handshake would keep refusing a venue the node had since acquired.
                // Operator cadence, one `Vec` per command, never the fold.
                let engines = publisher.engine_venues();
                // (c.2) …and the ROUTE-KEY roster the ACCOUNT gate is checked against, read off
                // the same cell PER COMMAND — but for a narrower reason than the line above gives,
                // and the narrower one is the true one for BOTH: this roster has exactly ONE
                // transition, EMPTY -> PUBLISHED. Nothing adds an engine at runtime
                // (`CoreThread::mount_strategy_runtime` refuses a mount whose route key no engine
                // carries — "a mount cannot conjure one"), so what a per-command read buys is that
                // a connection opened BEFORE the core's first publish starts checking the moment it
                // publishes, instead of staying blind for as long as that peer holds the socket.
                // A SECOND read rather than a second field of one: the two gates take different
                // slices because they answer different questions (`account_refusal`'s doc argues
                // it), and both are empty together because both project one `portfolio.venues`.
                let route_keys = publisher.engine_route_keys();
                // ⚠ ONCE PER CONNECTION, not per order: the pre-publish window in which this node
                // cannot check an address is exactly the state nobody noticed for as long as the
                // fallback was silent, so it says so — and it says it once, because a per-order
                // line on a busy control link is a line an operator scrolls past.
                if engines.is_empty() && !unrouted_warned {
                    unrouted_warned = true;
                    tracing::warn!(
                        peer = ?peer,
                        "node command routing UNCHECKED on this connection: the core has published \
                         no engine set yet, so a command naming a venue this node does not run \
                         cannot be refused and falls through to the PRIMARY engine (the historical \
                         behaviour). It resolves itself on this core's first publish"
                    );
                }
                match accept_command(
                    wire_cmd,
                    reason.as_deref(),
                    &mut limits,
                    sink,
                    settings.as_deref(),
                    &engines,
                    &route_keys,
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
                if scope != Scope::Write {
                    let _ = write_frame(
                        &mut stream,
                        &Response::AuthDenied {
                            reason: "read-only: authenticated as Observe".into(),
                        },
                    );
                    continue;
                }
                // The dry-run answers the SAME routing verdict the real command would get, and
                // must: a preview whose whole job is "what would this do" but that stays silent
                // about the command landing on a different book than the one it names is worse
                // than no preview. Ordered exactly as `accept_command` orders them (the edge gate
                // first, then the venue address, then the ACCOUNT within it), so the two cannot
                // report different reasons for one frame — and the account gate is here for the
                // same reason the venue one is: a preview that stayed silent about the named BOOK
                // not existing would hand the operator a dry run whose real send is a refusal.
                // Read-only like the rest of this arm — both gates are pure, and each roster read
                // is a snapshot-cell load.
                let reason = limits
                    .preview_vet(&wire_cmd)
                    .or_else(|| venue_refusal(&wire_cmd, &publisher.engine_venues()))
                    .or_else(|| account_refusal(&wire_cmd, &publisher.engine_route_keys()));
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
                                venue: String::new(),
                                symbol: String::new(),
                                interval: String::new(),
                                typed_params: None,
                                // ⚠ `None` and it cannot be otherwise HERE: this arm synthesises a
                                // row from the process-static IDENTITY BLOCK, which carries a
                                // strategy name and a params string and no mount config at all.
                                // The comment above says this daemon no longer takes this path —
                                // `main` publishes a row per mount, and THOSE carry the class from
                                // the profile. This is the old-shape fallback, so its `None` means
                                // "this row was derived, not published", which is the same thing it
                                // already means for the three empty addressing fields above.
                                asset_class: None,
                            }];
                        }
                        // The LIVE overlay (`FEATURE_STRATEGY_PARAMS`): the rows above are the
                        // process-static boot block, so their key is empty and their `params`
                        // string is whatever the profile rendered at boot. The core's own snapshot
                        // is the only thing that knows what each mount holds NOW, so read it here
                        // and write the addressing key + typed params onto the rows, by MOUNT
                        // ORDER — the one alignment both sides share (`publish::live_mount_params`
                        // filters the residual row out precisely so this index means the same
                        // thing on both sides).
                        //
                        // ⚠ OVERLAY, never a replacement: a row the snapshot cannot match (the
                        // identity fallback above, a core whose mount count disagrees with the boot
                        // block's, a mid-remount read) KEEPS its present shape — empty key, `None`
                        // params — rather than being dropped. A status that omits a mount is worse
                        // than one that admits it cannot type it: the omission reads as "that mount
                        // is gone", which is a claim about a LIVE book.
                        for (row, (venue, symbol, interval, typed)) in
                            mounts.iter_mut().zip(publisher.live_mount_params())
                        {
                            row.venue = venue;
                            row.symbol = symbol;
                            row.interval = interval;
                            row.typed_params = typed;
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
            // **ACCOUNT ADMINISTRATION** (`docs/decisions/0065`): the settings database's `account`
            // table, plus the one verb on this wire that carries a credential VALUE.
            //
            // The three parts of the barrier meet here, and each is a different KIND of thing:
            //
            //   1. STRUCTURAL — `accounts` is `None` on every box that has not DECLARED one, so
            //      there is no writer in this process to call and the frame is refused because
            //      there is nothing to refuse WITH. `served_features` withheld the capability
            //      string too, so a conforming client sent nothing; what reaches this arm with
            //      `None` is a client that skipped the negotiation.
            //   2. AUTHORIZATION — `Scope::Account`, a THIRD key. Checked here rather than at the
            //      handshake because the handshake grants a scope and this arm is where a scope
            //      becomes a capability; a Control peer that reached this frame is refused by name.
            //   3. CONFIDENTIALITY — decided at BOOT and unknowable here (`AccountAdminSource`'s
            //      own doc measures why the listener and the peer both answer wrongly), so it is
            //      carried on the handle rather than asked at the frame.
            //
            // ⚠ The refusals are audited NOWHERE, which is this daemon's uniform rule for every
            // refused command (`accept_command`'s `SetSetting` arm declares the same thing and
            // argues it). The ACCEPTED writes journal themselves, inside `apply`.
            Request::Account(req) => {
                // ⚠ The decision is [`account_admission`]'s, not this arm's — see its doc for the
                // mutation that measured what an in-arm `match` cost. This tuple exists only to
                // bind `src` once admission has already said yes.
                let resp = match (&accounts, account_admission(accounts.is_some(), scope)) {
                    (Some(src), Ok(())) => {
                        // ⚠ `req.verb.word()`, never `{req:?}` and never the request. The `Debug`
                        // impl on `AccountRequest` redacts — but a redacting impl is a PROMISE, and
                        // `word()` cannot carry a value at all. This is the line that says an
                        // account verb happened on a box where the audit trail is the only record.
                        tracing::warn!(
                            ?peer,
                            verb = req.verb.word(),
                            barrier = src.barrier.as_str(),
                            "vike-tradehub node: ACCOUNT ADMIN verb accepted — this peer may write \
                             the credential store"
                        );
                        // The rate token, consumed for an account verb exactly as it is for every
                        // control command: a flood is still a flood, and this surface opens a
                        // SQLite transaction per frame. The notional cap is n/a — an account verb
                        // is not an order and carries no qty×price to size, the `SetSetting`
                        // vetting decision verbatim.
                        match limits.vet_rate() {
                            Some(refusal) => Response::Error(refusal),
                            None => {
                                let actor = AccountActor {
                                    peer: peer_str.as_deref(),
                                    scope: "admin",
                                    key_id: key_id.as_deref(),
                                };
                                match src.apply(&req, &actor) {
                                    Ok(r) => r,
                                    Err(reason) => Response::Error(reason),
                                }
                            }
                        }
                    }
                    // Both refusals — absent capability and wrong scope — carry the message
                    // `account_admission` composed, so there is ONE authority for each string.
                    (_, Err(refusal)) => Response::Error(refusal),
                    // Unreachable by construction: admission is handed `accounts.is_some()`, so an
                    // `Ok` with a `None` source cannot occur. A daemon REFUSES rather than panics
                    // (`docs/decisions/0013`), and the message says the impossible thing happened
                    // rather than pretending the capability is merely unarmed.
                    (None, Ok(())) => Response::Error(
                        "account administration: internal inconsistency — admission passed while \
                         this node holds no account writer. Nothing was written. Please report \
                         this, it indicates a defect rather than a configuration problem."
                            .into(),
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
            // LIVE-JOURNAL REPORT read (ruling 16 of the datahub market-data wire design). Post-auth
            // under EITHER scope — Observe suffices, exactly like `Snapshot`/`StrategyStatus`: the
            // payload is a few dozen aggregate numbers over fills this node's snapshot already
            // publishes positions and PnL for, it executes nothing, and so it takes no audit record.
            //
            // ⚠ **This arm used to be a written IOU** — a named refusal, paired with a
            // `served_features` that deliberately withheld `FEATURE_TEARSHEET` so a conforming
            // client refused the verb client-side and nothing reached here. Both halves flip
            // together or neither does: the capability now rides the advertisement (see
            // `served_features`) and [`tearsheet_reply`] is the renderer behind it. What did NOT
            // change is the honesty rule the IOU followed — every way this can fail (no settings
            // source, no journal enabled, an unreadable journal) answers a `Response::Error` naming
            // the cause, never a fabricated empty tearsheet.
            //
            // The JOURNAL does not cross the wire, the ANSWER does — `Request::Tearsheet`'s own doc
            // carries that compute-to-data argument, and [`tearsheet_reply`] carries which journal
            // is read and the one rung this build cannot see.
            Request::Tearsheet { seed, periods_per_year } => {
                let resp = tearsheet_reply(settings.as_deref(), seed, periods_per_year);
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
/// against the REQUESTED scope's key (scope-generic): [`Scope::Read`] is granted whenever its key
/// verifies; [`Scope::Write`] additionally requires the node to HOLD a control key (absent ⇒
/// control disabled ⇒ refused before any key is consulted). Any session verb arriving before auth is
/// refused too (an unauthenticated `Subscribe` never registers).
fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    peer: Option<std::net::SocketAddr>,
    datahub_advertise: Option<&str>,
    // Whether this node holds an `AccountAdminSource` — the ONE conditional entry in
    // `served_features`. See that function and `vike_tradehub_client::proto::FEATURE_ACCOUNT_VERBS`.
    account_admin: bool,
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
            features: served_features(datahub_advertise, account_admin),
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
    // ⚠ The ADMIN gate, and it is the SAME closed-gate shape one rung up: a node that holds no
    // admin key refuses that scope WITHOUT consulting a key. On this surface it is also the
    // belt to a suspenders the BINARY already fastened — `account_admin_source` reads
    // `VIKE_TRADEHUB_ADMIN_KEY` only when the declaration armed the capability, so an
    // undeclared box reaches `serve` with an admin key that is EMPTY however its store is filled.
    // ⚠ The DECISION is [`scope_admission`]'s; these two arms keep the logging and the frame,
    // which is what a socket-less test cannot observe anyway. Kept as two blocks rather than one
    // because the two refusals log DIFFERENT sentences and a post-incident review searches for
    // them by text — see each `warn!` below.
    if scope == Scope::Account
        && let Err(reason) = scope_admission(scope, keys)
    {
        // WARN for `Control`'s reason verbatim: at the shipped `VIKE_LOG_FILE_LEVEL=warn` an
        // `info` refusal never reaches the log file, and *somebody presented an Admin handshake on
        // a node that has no admin capability* is exactly the sentence a post-incident review
        // needs to find there.
        tracing::warn!(
            ?peer,
            "vike-tradehub node: ADMIN auth refused — account administration is not armed on \
             this node"
        );
        let _ = write_frame(stream, &Response::AuthDenied { reason: reason.into() });
        return HandshakeOutcome::Closed;
    }
    if scope == Scope::Write
        && let Err(reason) = scope_admission(scope, keys)
    {
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
        let _ = write_frame(stream, &Response::AuthDenied { reason: reason.into() });
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
/// drop-oldest mailbox), and blocks it for at most `policy.push_write_timeout` per write
/// ([`PUSH_WRITE_TIMEOUT`] in production — the socket's `SO_SNDTIMEO`, set by [`handle_connection`]
/// at the `Subscribe`). Disconnect is detected on the next push write failing, or timing out.
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
/// ⚠ Second effect, worth having on purpose: this is also how a VANISHED subscriber is eventually
/// noticed. Before it, a writer with nothing to send parked in `recv_timeout` forever and the
/// connection thread outlived the client that owned it; now a dead peer's socket fails a heartbeat
/// write (once the send buffer fills and TCP gives up) and the thread exits. That is the DEAD-peer
/// half only, and this paragraph used to claim "stalled or vanished" for it: a LIVE peer that stops
/// reading never fails a write — its kernel ACKs every byte until its window is shut, and then the
/// write PARKS — so the heartbeat could not end that, and nothing did. [`PUSH_WRITE_TIMEOUT`] is
/// what ends it; [`log_push_write_end`] tells the two apart in the log.
fn run_push_writer(
    stream: &mut TcpStream,
    publisher: &PublisherHandle,
    peer: Option<std::net::SocketAddr>,
    policy: LinkPolicy,
) {
    let subscription = publisher.subscribe();
    // Since the last thing this connection put on the wire — a real frame or a heartbeat. Starts
    // now, so a subscriber that is handed its first frame immediately does not also get a beat.
    let mut last_write = Instant::now();
    loop {
        match subscription.recv_timeout(WRITER_RECV_TIMEOUT) {
            Recv::Frame(bytes) => {
                if let Err(e) = write_all_flush(stream, &bytes) {
                    log_push_write_end(&e, peer, "push write", policy.push_write_timeout);
                    return; // dropping `subscription` deregisters from the publisher
                }
                last_write = Instant::now();
            }
            // No new frame. Re-check the mailbox's closed state, and — if the socket has been
            // silent for a whole heartbeat — prove the link is alive rather than leaving the client
            // unable to distinguish this from a corpse.
            Recv::Timeout => {
                if last_write.elapsed() >= policy.observe_heartbeat {
                    if let Err(e) = write_frame(stream, &Response::Pong) {
                        log_push_write_end(&e, peer, "heartbeat write", policy.push_write_timeout);
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
fn served_features(datahub_advertise: Option<&str>, account_admin: bool) -> Vec<String> {
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
        // ...and that verb's ACCOUNT field, which needs a capability of its OWN rather than riding
        // the line above it. The line above is a DECODE claim — a B4-era node cannot read the
        // variant and says so. This one guards the opposite failure: a mount-verbs node from the
        // stage in between decodes the frame perfectly, `#[serde(default)]` turns the field it
        // never heard of into `account: None`, and the mount lands on the venue's default account
        // while the operator reads a normal acknowledgement. Advertised unconditionally — the field
        // and the arm that reads it ship together, so it is a property of the BUILD.
        FEATURE_MOUNT_ACCOUNT.to_string(),
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
        // The STRUCTURED live-params read: this node's `WireMountRow`s carry the mount's addressing
        // key and its typed params. Its OWN capability rather than riding `strategy-verbs`, and one
        // rung sharper than the mount verbs' reason: a node predating it ANSWERS `StrategyStatus`
        // with rows whose new fields `#[serde(default)]` fills in as empty, which a client cannot
        // tell from an honest "this mount publishes none" — see `FEATURE_STRATEGY_PARAMS`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument verbatim: a node whose mounts all
        // answer `typed_params: None` is still answering honestly, and a capability that flickered
        // with a construction detail (an identity-less publisher, a mount-less core) would be worse
        // than one that always means "this build can answer the question".
        FEATURE_STRATEGY_PARAMS.to_string(),
        // WHAT PRODUCT each mount trades: this node's `WireMountRow`s carry `asset_class`, the
        // stored word of a `vike_model::AssetClass`. `docs/decisions/0061`'s Phase 5 made a mount
        // NAME its class — `NOT NULL` in the settings database, refused at store time if absent —
        // and until this string the value was visible to the database alone.
        //
        // Its own capability for the `strategy-params` reason, one rung sharper again: a `None`
        // here has THREE sources and `#[serde(default)]` flattens two of them. A node predating
        // this cannot carry the field; a CURRENT node whose mount is still TOML-backed honestly has
        // no class, because `config::MountCfg`'s field is an `Option` and that asymmetry IS the
        // migration 0061 describes. Without the string a client reports the second as the first and
        // sends an operator to upgrade a daemon that is already current.
        //
        // Advertised UNCONDITIONALLY, the `strategy-params` argument verbatim: a node whose mounts
        // all answer `None` is still answering honestly, and a capability that flickered with a
        // deployment's migration state would mean "some mount here happens to be migrated" rather
        // than "this build can answer the question".
        FEATURE_MOUNT_CLASS.to_string(),
        // ROUTING BY THE ADDRESSED VENUE (`venue_refusal`): this node refuses a command naming a
        // venue it runs no engine for instead of applying it to its primary engine. Advertised
        // UNCONDITIONALLY, and the argument is one rung past `settings-show`'s: this capability is
        // a property of the BUILD — every command on this server goes through `accept_command`,
        // which applies the gate — and the only state in which the gate can answer nothing is the
        // window before the core's first publish, which is transient and self-resolving rather
        // than a configuration. A capability that flickered off during that window would be worse
        // than one that always means "this build checks the address": a client would see it absent
        // at connect, fall back to its own venue restriction for the whole session, and refuse
        // venues the node routes perfectly well.
        //
        // ⚠ It licenses a client-side LOOSENING, not a verb. A client that does NOT see it must
        // send only venues it can positively see in `WireSnapshot::venues`; seeing it means the
        // node will say no rather than misroute, so the client may offer the operator every venue
        // and let the node answer. `FEATURE_VENUE_ROUTING`'s own doc carries the direction.
        FEATURE_VENUE_ROUTING.to_string(),
        // ACCOUNT ROUTING: this node routes an order-carrying command onto the account its wire
        // payload names, instead of always the venue's default engine — the field
        // `crates/vike-core/src/runtime/apply.rs`'s `apply_intent_routed` now reads to resolve it.
        // Advertised UNCONDITIONALLY, `FEATURE_VENUE_ROUTING`'s argument one rung further: this is
        // a property of the BUILD, not of any one process's runtime state.
        //
        // ⚠ It was WITHHELD for exactly one window and no longer than that: between
        // `lower_command` copying the wire's `account` onto `vike_model::OrderRequest` (Task 3) and
        // `apply_intent_routed` reading that field to route (Task 4). Advertising inside that window
        // would have told a compliant client (check the string, then set the field) yes while it
        // still misrouted in silence — the exact defect this string exists to prevent. The window is
        // closed; the string is truthful again.
        // `served_features_tests::the_node_advertises_account_routing_now_that_routing_reads_the_field`
        // pins the pairing, and `FEATURE_ACCOUNT_ROUTING`'s own doc in
        // `vike_tradehub_client::proto` carries the client-side refusal rule this string releases.
        FEATURE_ACCOUNT_ROUTING.to_string(),
        // The LIVE-JOURNAL REPORT verb (ruling 16): `Request::Tearsheet` → `Response::Tearsheet`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument verbatim — the capability is a
        // property of this BUILD (the arm and its renderer are compiled in), not of whether this
        // particular process happens to have journaling switched on. A capability that flickered
        // with that runtime fact would be worse than one that always means "this build can answer
        // the question": the client refuses CLIENT-SIDE on a missing string
        // (`vike_tradehub_client::remote_handle`'s `tearsheet`), so flickering it off would turn a
        // node with no journal — an honest, named `Response::Error` an operator can act on — into
        // "this node does not support the verb", which is a claim about the BINARY and false.
        //
        // ⚠ This string was deliberately WITHHELD until the arm existed, and the reason is worth
        // keeping: advertising a capability before its renderer works converts a clean client-side
        // refusal into an opaque server error. The pairing is the invariant — this line and
        // [`tearsheet_reply`] ship together, and neither is correct alone.
        FEATURE_TEARSHEET.to_string(),
    ];
    // The datahub advertisement (split-plane REQ-2) — advertised ONLY when configured, so an
    // unconfigured daemon's Welcome is byte-identical to the pre-REQ-2 one. Advertisement, never
    // proxying: this server serves no data verb; the client dials the advertised address itself.
    if let Some(addr) = datahub_advertise {
        features.push(datahub_feature(addr));
    }
    // ⚠ **THE ONE CAPABILITY THIS NODE WITHHOLDS CONDITIONALLY, and the condition is the barrier.**
    // Every string above is a property of the BUILD and is advertised unconditionally — a node
    // without a `SettingsShowSource` still advertises `settings-show` and answers an honest
    // `Response::Error`, because a capability that flickers with a construction detail is worse
    // than one that always means *this build can answer the question*. The account verbs are the
    // exception, and deliberately: when the capability is absent there is no honest error to answer
    // WITH. `docs/decisions/0065` Part 1 is that the verb is refused *because there is nothing in
    // the process to refuse with*, so the advertisement has to track the same absence — a node that
    // advertised it and then refused every frame would be telling its clients a barrier had been
    // declared on a box where it had not.
    //
    // ⚠ Seeing this string is NOT authorization. Every account verb additionally requires
    // `Scope::Account`, a THIRD key whose tag is inside the signed preimage — so a Control peer that
    // reads this advertisement still cannot authenticate for it.
    if account_admin {
        features.push(FEATURE_ACCOUNT_VERBS.to_string());
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
///    Then [`venue_refusal`] — the ROUTING gate: a command naming a venue this node runs no engine
///    for is REFUSED rather than falling through to the primary engine. Inside this step, AFTER the
///    rate token (so mis-addressed frames cannot be flooded past the bucket) and before everything
///    below.
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
/// `engines` is the venues this node runs an engine for
/// ([`crate::publish::PublisherHandle::engine_venues`]), against which the command's addressed
/// venue is checked by [`venue_refusal`] — step 1b, immediately after the rate token so a peer
/// cannot spam mis-addressed commands for free, and before ANYTHING is lowered, audited or sent.
/// An EMPTY slice means "this node cannot answer that question" and refuses nothing (see
/// [`venue_refusal`] for why that is a deadlock-avoidance rule rather than a soft default); a
/// surface with no engine roster at all passes `&[]` and is byte-identical to before this gate
/// existed. ⚠ The Telegram channel is exactly that surface today — see its call site.
///
/// `route_keys` is the ROUTING KEYS that same roster publishes, one per engine
/// ([`crate::publish::PublisherHandle::engine_route_keys`]), against which a submit's named ACCOUNT
/// is checked by [`account_refusal`] — step 1c, immediately after the venue gate and for the case
/// that gate passes: the venue IS run and the account is not. It is a SECOND slice rather than a
/// wider first one because the two gates answer different questions and a refusal that widened
/// would change what an existing sentence means; [`account_refusal`]'s own doc argues it. Empty has
/// the identical UNKNOWN meaning `engines` has, and cannot disagree with it — both are projections
/// of one `portfolio.venues`, so they are empty together.
///
/// `Ok` is the [`Accepted`] verdict the surface shapes its reply from ([`Accepted::Coid`] ⇒
/// `Ack`, [`Accepted::SettingsWritten`] ⇒ `SettingsWritten`).
#[allow(clippy::too_many_arguments)] // see `serve_with_link_policy`'s note — each engine roster is
// one more gate INPUT on a function whose parameter list is the whole acceptance surface, and each
// one is documented at its own name above.
pub fn accept_command(
    cmd: WireCommand,
    reason: Option<&str>,
    limits: &mut ControlLimits,
    sink: &CommandSink,
    settings: Option<&SettingsShowSource>,
    engines: &[String],
    route_keys: &[String],
    peer: Option<std::net::SocketAddr>,
    key_id: Option<&str>,
) -> Result<Accepted, AcceptError> {
    if let Some(refusal) = limits.vet(&cmd) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1b. THE ROUTING GATE. Deliberately here — after the rate token (so a peer cannot flood
    // mis-addressed frames past the bucket) and before the sanitizer, the lowering, the core and
    // the audit record: a command that names a book this node does not have is not a command this
    // node may reinterpret, so nothing downstream should ever see it. A refusal is audited nowhere,
    // which is this daemon's uniform rule for every refused command (see the `SetSetting` arm's
    // declared note below).
    if let Some(refusal) = venue_refusal(&cmd, engines) {
        return Err(AcceptError::Refused(refusal));
    }
    // 1c. THE ACCOUNT GATE, immediately after the venue one and deliberately in that order: a venue
    // this node does not run is refused in the VENUE's words above, so what reaches here is the
    // narrower fault — the exchange is mounted and the named BOOK is not. Before this the node
    // Acked such a frame and the core refused it out of band, so the client printed `accepted` over
    // an order that never existed; the whole of the change is that the node answers first. Same
    // audit rule as every refusal on this path: none.
    if let Some(refusal) = account_refusal(&cmd, route_keys) {
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
        // ⚠ **DECLARED: a REFUSED settings write appends NO change-journal row on this surface**,
        // and the `?` below is the whole mechanism — it returns before
        // [`audit::record_settings_write`] is reached, so a refusal of any kind (a missing typed
        // confirm, an unknown file, a loader validation, a `Lock`/`Busy`/`Stranded` from the
        // writer) is answered on the wire and recorded nowhere durable. That is the UNDER-recording
        // direction, on the surface whose whole reason for journalling is attribution, and it is
        // kept deliberately for now on two grounds:
        //
        //   * it is this DAEMON's uniform rule, not a SetSetting quirk. `audit::record` fires only
        //     after `CommandSink::try_command` succeeds, so no refused control command of any verb
        //     has ever been journalled. Recording refusals for one verb alone would make the
        //     daemon's own trail inconsistent with itself, which is a worse thing to read than a
        //     known-empty one.
        //   * the cells such a row would carry are the ones a REFUSAL cannot back. Closing this
        //     properly needs `apply_set_setting` to return a TYPED refusal (so the verb-level ones
        //     — which attempted nothing against a file — can be told apart from the writer's, and
        //     `vike_config::journal_outcome` can classify the latter) plus an audit entry point
        //     that does not take a `vike_config::SettingsWrite`, a type whose own doc calls itself
        //     "what an accepted `set_setting` did". Both are real changes with their own review.
        //
        // The LOCAL surfaces answer the other way and say so at their own sites:
        // `crates/vike-cli/src/cmd/settings_write.rs`'s module doc (decision 2) and
        // `crates/vike-app-core/src/tool_views/venues.rs`'s `apply_arming` both append a refusal
        // row. The asymmetry is now declared on both sides rather than on one.
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

/// **REFUSE a command that names a venue this node runs no engine for** — the routing gate, and the
/// reason it exists is that without it the command does NOT fail, it lands somewhere else.
///
/// # What went wrong without it
///
/// Every order-carrying wire variant names a venue and always has ([`WireCommand::addressed_venue`]
/// is the one reading of it). [`lower_command`] copies that string onto
/// `vike_model::OrderRequest::venue`, and `vike_core`'s `CoreThread::route_of` resolves it to an
/// engine index — `.unwrap_or(0)` when it resolves to none. Engine 0 is the PRIMARY. So a control
/// peer naming a venue this process runs no engine for (an operator's DOM on a second exchange, a
/// `vike-cli submit okx …` against a binance-primary daemon, a typo) had its order risk-gated,
/// signed and sent by the PRIMARY venue's `ExecutionClient`, answered `Ack`, and appeared in the
/// snapshot — with no error anywhere. Worse than mere misrouting: `CoreThread::caps_venue` falls
/// back to the PAYLOAD's venue when nothing routed, so the capability preflight
/// (`vike_model::preflight_order_at`) was run against the row of the venue the order never reached.
///
/// # Why REFUSE rather than pick a default
///
/// Refusing costs the operator one retry and a message naming the venues this node actually runs.
/// Guessing costs them a position on a book they did not name, discovered on a statement. There is
/// no third answer available here: nothing in the frame says which of several engines was meant,
/// and "the primary" is not an inference from the operator's input, it is the absence of one.
///
/// # The three inputs, and the one that is a trap
///
/// * `cmd`'s address. `None` (an order-scoped, account-wide, or non-book verb) is never refused —
///   see [`WireCommand::addressed_venue`] for why `None` is a claim rather than an omission. In
///   particular the UNSCOPED panic button (`MarketExit { venue: None }`) reaches every engine and
///   can never be refused by this gate.
/// * `engines` — the venues this node runs an engine for
///   ([`crate::publish::PublisherHandle::engine_venues`]).
/// * ⚠ **An EMPTY `engines` means the core has not published yet, and is treated as UNKNOWN — the
///   command is NOT refused.** This is the one place this function deliberately does not bias
///   toward refusing, and the reason is that the opposite is a deadlock, not a conservative choice:
///   a core publishes when its state goes dirty, a refused command never reaches the core, so a
///   feed-less daemon that refused on an empty set would refuse every command it ever received, for
///   ever. The residual is the window between a node accepting connections and its first publish,
///   in which routing is exactly as unchecked as it was before this function existed.
///
/// The match is EXACT, never case-folded or trimmed, because exact is what the core does: engine
/// selection is a string comparison against `ExecutionEngine::route_key`, so accepting `"BINANCE"`
/// here would hand the core a string it then fails to route and silently sends to engine 0 — this
/// gate's own defect, reintroduced by being helpful. The refusal message names the roster, which is
/// what makes a case or spelling slip obvious in one line.
pub fn venue_refusal(cmd: &WireCommand, engines: &[String]) -> Option<String> {
    let venue = cmd.addressed_venue()?;
    // ⚠ The SENTENCE is `crate::config::no_engine_refusal`'s, not this function's, and the move is
    // the point rather than tidying: 0057's Phase 0 names the mount path as "the mount-side twin of
    // the defect the order path had fixed", so the profile path refuses an engine-less venue in
    // these exact words. Two spellings of one refusal is how an operator learns to read two
    // different faults into one situation.
    crate::config::no_engine_refusal(venue, engines, "The command")
}

/// **REFUSE a submit naming an ACCOUNT of that venue this node runs no engine for** —
/// [`venue_refusal`]'s account-level sibling, and the answer to the question that gate cannot pose.
///
/// # What went wrong without it
///
/// `vike-cli trade order submit binance/NOSUCH BTCUSDT buy 1` printed `accepted`. The node Acked
/// the frame and refused the order OUT OF BAND, as a recent-events note nobody was watching, so the
/// client reported success over an order that never existed. The core's own refusal is real and
/// stays — `crates/vike-core/src/runtime/mod.rs`'s `route_for_payload_account` composes
/// `route_key_of(venue, account)` and refuses when no engine carries it, rather than falling
/// through to that venue's default book — but it happens AFTER the Ack, on the other side of the
/// single-writer lane, which is a place a wire response can no longer be reached from. This gate
/// is the same verdict moved to where the client is still listening: the node ANSWERS before it
/// Acks. The core's copy is defence in depth for the callers this edge cannot cover (a journal
/// replay, a strategy-minted order, the GUI's own lane) and is not made redundant by this one.
///
/// # Why this is a SECOND function and not a WIDER [`venue_refusal`]
///
/// The two ask different questions, and only one of them has an answer the other could stand in
/// for. [`venue_refusal`] asks *does this node run this EXCHANGE at all*, compares against
/// [`crate::publish::PublisherHandle::engine_venues`], and its whole exactness argument is written
/// about venue ids. Re-pointing it at route keys would change what an existing refusal MEANS —
/// `binance` would stop matching a node that runs only `binance#ALT`, and the sentence an operator
/// reads would still say *"this node runs no engine for venue `binance`"* while the node plainly
/// runs one. So the venue gate keeps its slice and this one takes
/// [`crate::publish::PublisherHandle::engine_route_keys`] beside it.
///
/// The ORDER is load-bearing and is the other half of that argument: [`accept_command`] runs the
/// venue gate FIRST, so a venue this node does not run is refused in the venue's own words and can
/// never reach this function. What is left for this one is exactly the case the venue gate passes
/// and the router then misroutes — the venue IS run, the ACCOUNT is not. When no route key belongs
/// to `venue` at all this returns `None` for the same reason: that is the venue question wearing
/// account clothing, and two spellings of one refusal is how an operator learns to read two faults
/// into one situation (`crate::config::no_engine_refusal`'s own doc makes that argument for the
/// sentence it owns).
///
/// # `DEFAULT` is a NAME, and it resolves through the one composer
///
/// The wire admits `"account":"DEFAULT"`, meaning *the unlabelled account, deliberately* — a claim,
/// never an omission ([`vike_tradehub_client::wire::WireOrderRequest::account`] states the three
/// wire states). ⚠ `route_key_of` renders that account as the BARE VENUE ID, so `binance#DEFAULT`
/// is a key no engine anywhere carries, and a gate that suffixed the label text unconditionally
/// would turn the one spelling that addresses the original book into a refusal. This composes the
/// key with `route_key_of` — the same function `route_for_payload_account` composes with, so the
/// edge and the core cannot disagree about a spelling — and then asks whether the roster holds it.
/// A two-account node publishes `["binance", "binance#ALT"]`, so `DEFAULT` matches; a node whose
/// binance accounts are ALL labelled publishes neither, so `DEFAULT` names a book it does not have
/// and is refused by name. `vike_core`'s
/// `naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue` and
/// `a_payload_naming_default_on_an_unmounted_venue_refuses_while_its_account_less_twin_does_not`
/// pin the same two rows one plane down.
///
/// # The three inputs, and the two that are traps
///
/// * `cmd`. ONLY [`WireCommand::Submit`] is checked, and the match below has no wildcard arm so
///   that a new variant is classified rather than defaulted. The three RISK-REDUCING verbs are
///   deliberately NOT here: the risk-direction law of
///   `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §4.5 is
///   that a reducing venue verb naming no account FANS OUT to every account of that venue while an
///   increasing one refuses, and [`venue_refusal`]'s own doc records that the UNSCOPED panic button
///   can never be refused by a gate of this family. Narrowing a way OUT of a position is the one
///   thing an account-routing change must not do.
/// * `route_keys` — one per engine ([`crate::publish::PublisherHandle::engine_route_keys`]).
/// * ⚠ **An EMPTY `route_keys` means the core has not published yet, and is treated as UNKNOWN —
///   the command is NOT refused.** Inherited from [`venue_refusal`] rather than re-decided, and the
///   argument there is a deadlock rather than a preference: a core publishes when its state goes
///   dirty and a refused command never reaches the core, so a feed-less daemon that refused on an
///   empty roster would refuse every command it ever received, for ever. The inheritance is also
///   STRUCTURAL — both rosters are projections of one `portfolio.venues`, so they are empty
///   together and this gate cannot be armed while the venue gate is blind.
/// * ⚠ **A MALFORMED account is not this gate's refusal.** `parse_wire_account` owns the charset,
///   the length bound and the case-sensitivity that makes `alt` a different string from `ALT`, and
///   [`lower_command`] already refuses on it — before the Ack, since it runs inside
///   [`accept_command`]. Answering here too would put two sentences on one fault, so a label this
///   gate cannot parse falls through to the parser that can.
///
/// The membership test is EXACT, never case-folded or trimmed, for [`venue_refusal`]'s reason
/// applied one field along: engine selection is a string comparison against
/// `vike_exec::ExecutionEngine::route_key`, so being helpful here hands the core a key it then
/// fails to route. The refusal NAMES the accounts of that venue this node does hold, rendered with
/// `AccountLabel`'s own `Display` (so the unlabelled one reads `DEFAULT`, the one spelling a client
/// can send back), sorted and deduplicated exactly as the venue roster is — which is what makes a
/// spelling slip obvious in one line.
pub fn account_refusal(cmd: &WireCommand, route_keys: &[String]) -> Option<String> {
    // NO WILDCARD ARM, for `WireCommand::addressed_venue`'s reason: a new order-carrying variant
    // must be CLASSIFIED here rather than fall into the not-checked column by default, which is
    // exactly where a new risk-INCREASING verb would land silently.
    let (venue, account) = match cmd {
        WireCommand::Submit(req) => (req.venue.as_str(), req.account.as_deref()?),
        // The RISK-REDUCING trio — see this function's doc. ⚠ This comment used to say "an account
        // NARROWS these verbs", and that is FALSE: `lower_command` DROPS the account on all three
        // and `vike_core`'s `CoreThread::exit_scope_engines` fans out over every account of the
        // venue, so a frame naming one would reduce books nobody named. The conclusion survives the
        // correction — this gate still returns `None` for them — but not for the reason written
        // here before.
        //
        // The refusal for a NAMED account on these three is client-side instead, through
        // `vike_tradehub_client::proto`'s `FEATURE_ACCOUNT_SCOPED_REDUCE`, which this node does not
        // advertise. That placement is a ruling, not an accident: refusing here would refuse a verb
        // the node's OWN advertised capability says is supported, which is a second defect wearing
        // a safer coat. An ABSENT account still fans out and must — §4.5's law is that a fan-out
        // cannot reach an account the sender did not mean, because the sender meant all of them —
        // and refusing a narrowed one here would make the panic button need an argument to be right.
        WireCommand::MassCancel { .. }
        | WireCommand::Flatten { .. }
        | WireCommand::MarketExit { .. } => return None,
        // ⚠ DECLARED RESIDUAL, not an oversight. `MountStrategy` names an account too and its own
        // doc rates the stakes a rung HIGHER than an order's (a misrouted mount is every order that
        // strategy will ever place, sized against the wrong book). It is refused today by
        // `CoreThread::mount_strategy_runtime`, as a recent-events note — the same out-of-band
        // shape this gate exists to fix, one rung up — and `crate::mount_factory::validate_spec`,
        // the daemon's own edge validation for the verb, validates the PROFILE and never the
        // account. Covering it here is a small widening of this match and a REPORTED finding rather
        // than a silent one, because the ruling that produced this function names the order path.
        WireCommand::MountStrategy { .. } => return None,
        // Verbs that name no account at all: order-scoped (the coid resolves the engine), the
        // account-wide kill switch, the mount-keyed verbs, and the one verb that never enters the
        // core.
        WireCommand::Cancel(_)
        | WireCommand::Modify { .. }
        | WireCommand::SetTradingState(_)
        | WireCommand::UpdateParams { .. }
        | WireCommand::UnmountStrategy { .. }
        | WireCommand::SetSetting { .. } => return None,
    };
    if route_keys.is_empty() {
        return None;
    }
    // A label this gate cannot parse belongs to `lower_command`'s refusal, not to a second one.
    let label = vike_model::account_keys::parse_wire_account(account).ok()?;
    // THE ONE COMPOSER — `route_key_of`, the same call the core's own resolution makes. The default
    // account renders as the bare venue id here, which is why `DEFAULT` matches a single-account
    // roster rather than minting `venue#DEFAULT` and refusing the book it names.
    let wanted = vike_model::account_keys::route_key_of(venue, &label);
    if route_keys.iter().any(|k| k == &wanted) {
        return None;
    }
    let mut held: Vec<String> = route_keys
        .iter()
        .filter_map(|k| vike_model::account_keys::label_of_route_key(venue, k))
        .map(|l| l.to_string())
        .collect();
    // No engine of this venue at all ⇒ the VENUE question, which `venue_refusal` answers first and
    // in its own words. Reachable here only by a caller that runs this gate alone.
    if held.is_empty() {
        return None;
    }
    held.sort_unstable();
    held.dedup();
    Some(format!(
        "this node runs no account `{account}` of venue `{venue}` — it runs: {}. The command was \
         REFUSED rather than applied to another book of that venue: an order names the book it is \
         for, and a node that cannot honour the name must not choose one",
        held.join(", ")
    ))
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
            // `parse_wire_account` is the ONE parser for this field, and it is the same one
            // `MountStrategy`'s arm below calls — so `DEFAULT` is admitted here exactly as it is
            // there, and the reserved spelling stays that function's business, not this arm's.
            // Absence still means the default account, so every pre-existing client's frame
            // lowers byte-identically. A malformed label is REFUSED here, not swallowed into
            // `None`: `None` routes to the venue's default book, and turning a typo'd account
            // name into a silent trade on the wrong book is exactly the misroute this field
            // exists to delete.
            let account = match req
                .account
                .as_deref()
                .map(vike_model::account_keys::parse_wire_account)
                .transpose()
            {
                Ok(a) => a,
                Err(e) => return Err(format!("submit: `account` — {e}")),
            };
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
                account,
                ..Default::default()
            };
            Ok((Command::Order(OrderIntent::Submit(Box::new(order))), coid))
        }
        WireCommand::Cancel(coid) => Ok((Command::Order(OrderIntent::Cancel(coid.clone())), coid)),
        WireCommand::Modify { client_order_id, new_qty, new_price } => {
            let coid = client_order_id.clone();
            Ok((Command::Order(OrderIntent::Modify { client_order_id, new_qty, new_price }), coid))
        }
        WireCommand::MassCancel { venue, symbol, .. } => {
            Ok((Command::Order(OrderIntent::MassCancel { venue, symbol }), String::new()))
        }
        WireCommand::Flatten { venue, symbol, .. } => {
            Ok((Command::Order(OrderIntent::Flatten { venue, symbol }), String::new()))
        }
        WireCommand::MarketExit { venue, .. } => {
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
            account,
            symbol,
            interval,
            controller_id,
            name,
            rhai,
            params,
        } => {
            // ⚠ **THE WIRE CAN NAME ONE NOW, AND THE ARGUMENT THAT SAID IT MUST NOT IS ANSWERED
            // RATHER THAN DROPPED.** This arm hard-coded `account: None` and said why: *"a remote
            // peer mounting onto a second account would be naming a book the operator never armed
            // for that channel"*, with `DaemonProfile::account` — a file the operator edits — as
            // the only door to a second account.
            //
            // What answers it is that the premise is not reachable. A peer cannot name a book the
            // operator never armed, because naming one resolves NOTHING: `mount_engine_resolution`
            // matches the label against the route keys of the engines this core actually runs, and
            // an account with no `policy.accounts.<venue>.<LABEL>` row and no `__<LABEL>`
            // credentials has no engine and no route key. It is refused BY NAME. So the wire's
            // reach is exactly the set of books the operator already armed — which is the same
            // authority the profile door has, arrived at through a frame this peer signed.
            //
            // What the old comment got RIGHT and this keeps: absence still means the default
            // account, so every frame an existing client sends is unchanged, and
            // `DaemonProfile::account` is untouched as the file-shaped door.
            //
            // `parse_wire_account` is the reader, and it is the same one the order plane uses:
            // `DEFAULT` is admitted here where `policy.toml` refuses it, because on the wire that
            // spelling is how a client says *"the unlabelled account, deliberately"* as opposed to
            // saying nothing at all.
            let account = match account
                .as_deref()
                .map(vike_model::account_keys::parse_wire_account)
                .transpose()
            {
                Ok(a) => a,
                Err(e) => return Err(format!("mount_strategy: `account` — {e}")),
            };
            let spec =
                MountSpec { venue, symbol, interval, account, controller_id, name, rhai, params };
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

/// Log the end of a PUSH write — the writer-side twin of [`log_read_end`], and the one place the
/// two ways a subscribed connection ends are told apart. A peer that went AWAY fails the write
/// outright (reset, broken pipe, or TCP giving up on a socket nobody ACKs): an ordinary close, at
/// info, under the exact line it has always had. A peer that is still THERE and stopped reading
/// never fails a write — it parks it — and only [`PUSH_WRITE_TIMEOUT`] ends that, surfacing as
/// `WouldBlock` (Linux: `EAGAIN` from `SO_SNDTIMEO`) or `TimedOut` (Windows); that arm names the
/// bound and logs at warn, because the node just dropped an AUTHENTICATED subscriber and an
/// operator reading the log must be able to attribute the close to the peer rather than to the
/// node. Every path leads to closing the connection: a timed-out write leaves a PARTIAL frame on
/// the wire, so the stream is desynced and cannot be resumed (the rule `log_read_end` states for
/// reads).
fn log_push_write_end(
    e: &io::Error,
    peer: Option<std::net::SocketAddr>,
    what: &str,
    timeout: Option<Duration>,
) {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            tracing::warn!(
                ?peer,
                ?timeout,
                "vike-tradehub observe: {what} timed out — the subscriber kept its socket open and \
                 read nothing for the whole push write bound; closing"
            );
        }
        _ => {
            tracing::info!(?peer, error = %e, "vike-tradehub observe: {what} ended, closing");
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
        let features = served_features(None, false);
        assert!(advertised_datahub(&features).is_none());
        assert!(features.iter().all(|f| !f.starts_with("datahub=")));
    }

    /// Configured — the entry rides BESIDE the named capabilities (nothing is displaced; the
    /// exact-match feature guards still find their strings) and round-trips through the client
    /// parser verbatim.
    #[test]
    fn a_configured_daemon_advertises_its_datahub_beside_the_named_capabilities() {
        let features = served_features(Some("127.0.0.1:7878"), false);
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
            let mut unconfigured = served_features(None, false);
            unconfigured.push(datahub_feature("127.0.0.1:7878"));
            unconfigured
        });
    }

    /// The tearsheet capability and its renderer ship TOGETHER — this is the half a wire client
    /// negotiates on, and it was deliberately absent while the arm was an IOU. Pinned so that
    /// removing [`tearsheet_reply`] without removing the string (or the reverse) is caught here
    /// rather than by an operator meeting an opaque server error.
    #[test]
    fn the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it() {
        assert!(served_features(None, false).iter().any(|f| f == FEATURE_TEARSHEET));
    }

    /// The claim is true again: `crates/vike-core/src/runtime/apply.rs`'s `apply_intent_routed` now
    /// reads the account `lower_command` copies onto `vike_model::OrderRequest`, so a node running
    /// this build really does route a labelled order rather than dropping it onto the venue's
    /// default engine. This test was
    /// `the_node_does_not_advertise_account_routing_while_the_field_is_unread` — its assertion is
    /// inverted here rather than deleted, per
    /// `docs/superpowers/plans/2026-09-22-the-order-payload-names-its-account.md`'s Task 5, so the
    /// history of the withholding stays attached to the pin that replaced it.
    #[test]
    fn the_node_advertises_account_routing_now_that_routing_reads_the_field() {
        let features = served_features(None, false);
        assert!(
            features.iter().any(|f| f == FEATURE_ACCOUNT_ROUTING),
            "the node does not advertise account routing even though `apply_intent_routed` reads \
             the field: {features:?}"
        );
    }
}

/// [`tearsheet_reply`] — every way the LIVE-JOURNAL report verb can answer, decided without a
/// socket. The shape under test is the HONESTY rule the retired IOU arm followed: each failure
/// names its own cause, and in particular "this node has no journal" never wears the words of
/// "this build cannot serve the verb".
#[cfg(test)]
mod tearsheet_reply_tests {
    use super::*;

    /// A source whose env sweep is exactly `vars` and which resolves no project — the journal
    /// resolution under test reads ONLY `env`, so the other two fields are inert here.
    fn source(vars: &[(&str, &str)]) -> SettingsShowSource {
        SettingsShowSource {
            settings_dir: None,
            env: vars.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect(),
            hot: None,
            booted_authority: vike_config::Authority::Files,
        }
    }

    fn error_text(r: Response) -> String {
        match r {
            Response::Error(m) => m,
            other => panic!("expected Response::Error, got {other:?}"),
        }
    }

    /// A server built through `serve(.., None)` cannot resolve its own journal, and says that
    /// rather than reporting an empty tearsheet — the identity-less `StrategyStatus` rule.
    #[test]
    fn a_source_less_server_refuses_by_name() {
        let msg = error_text(tearsheet_reply(None, None, None));
        assert!(msg.contains("without a settings source"), "{msg}");
    }

    /// ⚠ The load-bearing refusal. A node journalling through `config.toml`'s `journal_dir` is
    /// INDISTINGUISHABLE from one with journaling off through the env sweep this arm reads, so the
    /// message must name that rung outright. Without this sentence the reply asserts something
    /// false about a live node — that it is not journalling when it may well be.
    #[test]
    fn no_journal_in_the_environment_names_the_rung_this_build_cannot_see() {
        let msg = error_text(tearsheet_reply(Some(&source(&[])), None, None));
        assert!(msg.contains("VIKE_JOURNAL_DIR"), "the env knob is named: {msg}");
        assert!(msg.contains("journal_dir"), "…and the settings key it cannot see: {msg}");
        assert!(msg.contains("CANNOT SEE IT"), "…as a limitation, not as a verdict: {msg}");
    }

    /// A directory that is not there is an UNREADABLE journal, not an absent one: the operator
    /// configured a path and the path is wrong, which is a different fact from "journaling is off"
    /// and gets a different sentence (and names the path, so it can be fixed).
    #[test]
    fn a_missing_journal_directory_reports_the_read_failure_and_the_path() {
        let dir = std::env::temp_dir().join("vike-tradehub-tearsheet-absent-983471");
        let src = source(&[("VIKE_JOURNAL_DIR", &dir.display().to_string())]);
        let msg = error_text(tearsheet_reply(Some(&src), None, None));
        assert!(msg.contains("could not be read"), "{msg}");
        assert!(msg.contains("983471"), "…naming the directory it tried: {msg}");
    }

    /// An EMPTY journal directory is a journal with no fills in it — a zero-trade tearsheet, which
    /// is an answer — and the reply is the pretty JSON spelling `tearsheet --json` prints, because
    /// `remote_handle::tearsheet` hands this text to its caller verbatim.
    #[test]
    fn an_empty_journal_folds_to_a_zero_trade_tearsheet_in_pretty_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = source(&[("VIKE_JOURNAL_DIR", &dir.path().display().to_string())]);
        let Response::Tearsheet(json) = tearsheet_reply(Some(&src), None, None) else {
            panic!("an empty journal is an answer, not a refusal");
        };
        assert!(json.contains('\n'), "the payload is to_string_pretty, not compact: {json}");
        let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(doc["n_trades"], serde_json::json!(0));
        // Seed `None` ⇒ the LOCAL door's default, so the remote and local reports of one journal
        // cannot scale `total_return`/`cagr`/`sharpe` off different equity bases.
        assert_eq!(doc["final_equity"], serde_json::json!(TEARSHEET_DEFAULT_SEED));
    }

    /// …and a caller WITH an opinion gets it: the wire's optionals are honoured, not clamped to the
    /// defaults above.
    #[test]
    fn a_supplied_seed_reaches_the_fold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = source(&[("VIKE_JOURNAL_DIR", &dir.path().display().to_string())]);
        let Response::Tearsheet(json) = tearsheet_reply(Some(&src), Some(25_000.0), Some(365.0))
        else {
            panic!("an empty journal is an answer, not a refusal");
        };
        let doc: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(doc["final_equity"], serde_json::json!(25_000.0));
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
        write_frame(&mut buf, &Request::Auth { scope: Scope::Write, mac: vec![0u8; 32] }).unwrap();
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
            account: None,
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
            l.vet(&WireCommand::Flatten {
                venue: "hyperliquid".into(),
                symbol: "BTC".into(),
                account: None
            })
            .is_none()
        );
        assert!(l.vet(&WireCommand::MarketExit { venue: None, account: None }).is_none());
        assert!(
            l.vet(&WireCommand::MassCancel { venue: None, symbol: None, account: None }).is_none()
        );
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
mod venue_refusal_tests {
    //! [`venue_refusal`] in isolation — the pure verdict. The end-to-end proof (the refusal
    //! reaching a real control peer as a `Response::Error`, and a matched address reaching the
    //! engine it names on a two-engine core) is `tests/daemon/venue_routing.rs`.

    use super::*;

    fn roster() -> Vec<String> {
        vec!["polymarket".to_string(), "binance".to_string()]
    }

    fn submit(venue: &str) -> WireCommand {
        WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: venue.into(),
            symbol: "SYM".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
        })
    }

    #[test]
    fn a_venue_this_node_runs_is_not_refused() {
        assert_eq!(venue_refusal(&submit("binance"), &roster()), None);
        assert_eq!(venue_refusal(&submit("polymarket"), &roster()), None);
    }

    #[test]
    fn an_unmatched_venue_is_refused_and_the_message_names_the_whole_roster() {
        let msg = venue_refusal(&submit("okx"), &roster()).expect("refused");
        assert!(msg.contains("okx"), "{msg}");
        assert!(msg.contains("binance") && msg.contains("polymarket"), "{msg}");
    }

    /// The roster is printed SORTED and DEDUPED, so a two-account node does not name one exchange
    /// twice and the list reads the same however the engines were registered. The refusal DECISION
    /// is unaffected by either (it is a membership test).
    #[test]
    fn the_named_roster_is_sorted_and_deduplicated() {
        let two_accounts =
            vec!["binance".to_string(), "polymarket".to_string(), "binance".to_string()];
        let msg = venue_refusal(&submit("okx"), &two_accounts).expect("refused");
        assert!(msg.contains("binance, polymarket"), "{msg}");
        assert!(!msg.contains("binance, binance"), "{msg}");
    }

    /// ⚠ EXACT match, never case-folded or trimmed — the core selects an engine by string equality
    /// on its route key, so accepting `"BINANCE"` here would hand it a string it fails to route and
    /// silently sends to engine 0: this gate's own defect, reintroduced by being helpful.
    #[test]
    fn the_match_is_exact_so_a_case_or_space_slip_is_refused_rather_than_guessed() {
        for slip in ["BINANCE", "Binance", " binance", "binance "] {
            let msg = venue_refusal(&submit(slip), &roster())
                .unwrap_or_else(|| panic!("`{slip}` must not be read as `binance`"));
            assert!(msg.contains(slip), "the refusal quotes what was asked for: {msg}");
        }
    }

    /// An EMPTY roster is "the core has not published yet", NOT "this node runs no engines", and
    /// refuses nothing. Refusing on it would be permanent: a core publishes when its state goes
    /// dirty, and a refused command never reaches the core to make it dirty.
    #[test]
    fn an_empty_roster_refuses_nothing_because_it_means_unknown() {
        assert_eq!(venue_refusal(&submit("okx"), &[]), None);
        assert_eq!(venue_refusal(&submit("anything-at-all"), &[]), None);
    }

    /// Every ADDRESS-LESS verb passes untouched — including the UNSCOPED panic button, which must
    /// never need an argument.
    #[test]
    fn an_address_less_command_is_never_refused() {
        let roster = roster();
        for cmd in [
            WireCommand::Cancel("c-1".into()),
            WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
            WireCommand::SetTradingState(WireTradingState::Halted),
            WireCommand::MassCancel { venue: None, symbol: None, account: None },
            WireCommand::MarketExit { venue: None, account: None },
            WireCommand::UnmountStrategy { controller_id: "m-1".into() },
        ] {
            assert_eq!(venue_refusal(&cmd, &roster), None, "{cmd:?} names no venue");
        }
    }

    /// ...and every SCOPED one is checked, whichever verb it is. A `flatten okx` or a
    /// `market-exit okx` on a node with no okx engine acts on the PRIMARY's book without this.
    #[test]
    fn every_scoped_verb_is_checked_not_just_submit() {
        let roster = roster();
        for cmd in [
            WireCommand::Flatten { venue: "okx".into(), symbol: "SYM".into(), account: None },
            WireCommand::MassCancel { venue: Some("okx".into()), symbol: None, account: None },
            WireCommand::MarketExit { venue: Some("okx".into()), account: None },
            WireCommand::UpdateParams {
                venue: "okx".into(),
                symbol: "SYM".into(),
                interval: "1m".into(),
                params: serde_json::json!({}),
            },
            WireCommand::MountStrategy {
                venue: "okx".into(),
                account: None,
                symbol: "SYM".into(),
                interval: "1m".into(),
                controller_id: None,
                name: Some("buy_hold".into()),
                rhai: None,
                params: serde_json::json!({}),
            },
        ] {
            assert!(
                venue_refusal(&cmd, &roster).is_some(),
                "{cmd:?} addresses okx and must refuse"
            );
        }
    }
}

#[cfg(test)]
mod account_refusal_tests {
    //! [`account_refusal`] in isolation — the pure verdict for the fault
    //! [`venue_refusal`]'s evidence cannot see. The symptom this closes is the one the spec opens
    //! with: `vike-cli trade order submit binance/NOSUCH BTCUSDT buy 1` answered `accepted`,
    //! because the node Acked the frame and the core refused the order out of band, on the far side
    //! of the single-writer lane where no wire response can be reached.
    //!
    //! ⚠ The roster these tests are written against is the ROUTE-KEY one
    //! ([`crate::publish::PublisherHandle::engine_route_keys`]), not the venue one — a two-account
    //! node publishes `binance` twice under `venues[].venue` and `["binance", "binance#ALT"]` under
    //! `venues[].route_key`, and only the second can tell the two books apart.

    use super::*;

    /// A node running TWO accounts of one exchange plus a single-account venue — the configuration
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s *What would reopen
    /// this* named in advance, and the only one in which this gate has anything to say.
    fn roster() -> Vec<String> {
        vec!["binance".to_string(), "binance#ALT".to_string(), "polymarket".to_string()]
    }

    fn submit(venue: &str, account: Option<&str>) -> WireCommand {
        WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
            client_order_id: "c-1".into(),
            venue: venue.into(),
            symbol: "SYM".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: account.map(str::to_string),
        })
    }

    /// A submit naming an account this node does not run is REFUSED ON THE WIRE, naming the
    /// accounts that do exist. Before this it was Acked and refused out of band as a recent-events
    /// note, so the client printed `accepted` over an order that never existed.
    #[test]
    fn a_submit_naming_an_unheld_account_is_refused_naming_the_roster() {
        let msg = account_refusal(&submit("binance", Some("NOSUCH")), &roster()).expect("refused");
        assert!(msg.contains("NOSUCH"), "the refusal quotes what was asked for: {msg}");
        assert!(msg.contains("binance"), "…and the venue it was asked for on: {msg}");
        assert!(msg.contains("ALT"), "…and the labelled account that DOES exist: {msg}");
        assert!(
            msg.contains("DEFAULT"),
            "…and the unlabelled one, under the one spelling a client can send back: {msg}"
        );
        // ⚠ The VENUE gate is silent on exactly this frame, which is why a second gate exists at
        // all rather than a wider first one: `binance` IS a venue this node runs.
        assert_eq!(
            venue_refusal(&submit("binance", Some("NOSUCH")), &["binance".to_string()]),
            None,
            "the venue question has a different, correct answer here — this is a second question"
        );
    }

    /// A submit naming an account this node DOES run is untouched — both spellings, because the
    /// unlabelled account's key is the BARE VENUE ID and a gate that suffixed the label text
    /// unconditionally would refuse the one book it names.
    #[test]
    fn a_submit_naming_a_held_account_is_not_refused() {
        assert_eq!(account_refusal(&submit("binance", Some("ALT")), &roster()), None);
        assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &roster()), None);
        assert_eq!(
            account_refusal(&submit("polymarket", Some("DEFAULT")), &roster()),
            None,
            "a single-account venue's route key IS its venue id, so `DEFAULT` addresses it"
        );
    }

    /// ⚠ INHERITED FROM [`venue_refusal`]: an EMPTY roster means the core has not published yet and
    /// is treated as UNKNOWN. Refusing here is a DEADLOCK, not a conservative default — a core
    /// publishes when its state goes dirty and a refused command never reaches the core, so a
    /// feed-less daemon that refused on an empty roster would refuse every command for ever.
    ///
    /// The inheritance is structural as well as asserted: `engine_venues` and `engine_route_keys`
    /// both project one `portfolio.venues`, so a caller can never hold one populated roster and one
    /// empty one — which is what keeps this gate from being armed while the venue gate is blind.
    #[test]
    fn an_empty_roster_refuses_nothing() {
        assert_eq!(account_refusal(&submit("binance", Some("NOSUCH")), &[]), None);
        assert_eq!(account_refusal(&submit("anything-at-all", Some("ALT")), &[]), None);
        assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &[]), None);
    }

    /// ⚠ A submit naming NO account is unchanged on every roster shape, including a multi-account
    /// one. Its case belongs to `vike_core`'s `CoreThread::ambiguous_accounts` — a sender that
    /// named NOTHING on a venue with several accounts — and this gate must not take it over: the
    /// refusals read differently (*which book did you mean* against *you named a book I do not
    /// have*), and folding them would tell an operator to name an account while this gate had no
    /// account to check.
    #[test]
    fn a_submit_naming_no_account_is_never_refused_by_this_gate() {
        for roster in [roster(), vec!["binance".to_string()], Vec::new()] {
            assert_eq!(account_refusal(&submit("binance", None), &roster), None, "{roster:?}");
            assert_eq!(account_refusal(&submit("okx", None), &roster), None, "{roster:?}");
        }
    }

    /// ⚠ **THE RULING ON `DEFAULT`, and it is the same one the core made.** `DEFAULT` is a NAME,
    /// never an omission, and it resolves through `route_key_of` — which renders the unlabelled
    /// account as the BARE VENUE ID. So it matches a roster holding that id and is refused by a
    /// roster that does not, which is the sharp edge: a node whose binance accounts are ALL
    /// labelled runs no `binance` key, so `DEFAULT` names a book it does not have.
    ///
    /// `vike_core`'s `naming_default_resolves_the_unlabelled_engine_of_a_two_engine_venue` and
    /// `a_payload_naming_default_on_an_unmounted_venue_refuses_while_its_account_less_twin_does_not`
    /// pin the two halves one plane down, and this gate composes the key with the SAME function
    /// `CoreThread::route_for_payload_account` composes it with, so the two planes cannot disagree
    /// about a spelling.
    #[test]
    fn naming_default_resolves_the_bare_venue_key_exactly_as_the_core_resolves_it() {
        // Held: the two-account node publishes the bare id beside the labelled one.
        assert_eq!(account_refusal(&submit("binance", Some("DEFAULT")), &roster()), None);
        // NOT held: every account of this venue is labelled, so there is no bare key to match.
        let all_labelled = vec!["binance#A".to_string(), "binance#B".to_string()];
        let msg =
            account_refusal(&submit("binance", Some("DEFAULT")), &all_labelled).expect("refused");
        assert!(msg.contains("`DEFAULT`"), "names the account that was asked for: {msg}");
        assert!(msg.contains("it runs: A, B"), "…and the two that exist: {msg}");
        // ⚠ …and `binance#DEFAULT` is NOT the key that was looked for. A gate that suffixed the
        // label text unconditionally would MATCH this roster and REFUSE the held one above — both
        // wrong, and both wrong silently. The key is unmintable (`route_key_of` never produces it,
        // because `AccountLabel::parse` refuses the reserved spelling), so the account beside it is
        // what the refusal names.
        let decoy = vec!["binance#DEFAULT".to_string(), "binance#ALT".to_string()];
        let msg = account_refusal(&submit("binance", Some("DEFAULT")), &decoy).expect("refused");
        assert!(
            msg.contains("it runs: ALT"),
            "the unmintable key names no account at all, so only `ALT` is held: {msg}"
        );
    }

    /// The venue is the OTHER half of the key, and a submit on a venue this roster knows nothing
    /// about is [`venue_refusal`]'s refusal, not this one's. Answering here too would put two
    /// sentences on one fault — `crate::config::no_engine_refusal` makes that argument for the
    /// sentence it owns.
    #[test]
    fn a_venue_with_no_engine_at_all_is_the_venue_gates_refusal_not_this_ones() {
        assert_eq!(account_refusal(&submit("okx", Some("ALT")), &roster()), None);
        assert!(
            venue_refusal(&submit("okx", Some("ALT")), &["binance".to_string()]).is_some(),
            "…and the venue gate, which runs FIRST in `accept_command`, does answer it"
        );
    }

    /// ⚠ EXACT match, never case-folded or trimmed — [`venue_refusal`]'s rule one field along.
    /// A label this gate cannot PARSE is deliberately not its refusal either: `parse_wire_account`
    /// owns the charset, the length bound and the case-sensitivity that makes `alt` a different
    /// string from `ALT`, and `lower_command` refuses on it INSIDE `accept_command`, before the
    /// Ack. So `alt` is still refused at the edge — by the parser, in one sentence rather than two.
    #[test]
    fn the_match_is_exact_and_an_unparseable_label_belongs_to_the_parser() {
        let msg = account_refusal(&submit("binance", Some("ALTX")), &roster()).expect("refused");
        assert!(msg.contains("ALTX"), "a well-formed label that names no book is ours: {msg}");
        for slip in ["alt", "Alt", " ALT", "ALT "] {
            assert_eq!(
                account_refusal(&submit("binance", Some(slip)), &roster()),
                None,
                "`{slip}` is not readable as `ALT` here, and its refusal is `lower_command`'s"
            );
            assert!(
                vike_model::account_keys::parse_wire_account(slip).is_err(),
                "…which is only true because the PARSER refuses it: `{slip}`"
            );
        }
    }

    /// The printed roster is SORTED and DEDUPLICATED, like the venue gate's, and it names ONLY the
    /// accounts of the venue that was asked about — a node running forty other books must not
    /// answer a binance typo with all of them.
    #[test]
    fn the_named_accounts_are_scoped_to_the_venue_sorted_and_deduplicated() {
        let wide = vec![
            "binance#ALT".to_string(),
            "binance".to_string(),
            "binance#ALT".to_string(),
            "okx#TREASURY".to_string(),
            "polymarket".to_string(),
        ];
        let msg = account_refusal(&submit("binance", Some("NOSUCH")), &wide).expect("refused");
        assert!(msg.contains("ALT, DEFAULT"), "sorted, and deduped: {msg}");
        assert!(!msg.contains("TREASURY"), "another venue's accounts are not this answer: {msg}");
        assert!(!msg.contains("polymarket"), "…nor another venue at all: {msg}");
    }

    /// ⚠ **THE RISK-DIRECTION LAW, gated rather than trusted.** The three risk-REDUCING verbs are
    /// NOT this gate's, whatever account they name: §4.5 of the spec rules that a reducing venue
    /// verb naming no account fans out to every account of that venue, and [`venue_refusal`]'s own
    /// doc records that the UNSCOPED panic button can never be refused by a gate of this family.
    /// Narrowing a way OUT of a position is the one thing an account-routing change must not do.
    ///
    /// `MountStrategy` is here for a DIFFERENT reason and is a declared residual, not a law: it
    /// names an account too and is refused only by the core, out of band — the same shape this gate
    /// fixes for orders. See `account_refusal`'s own arm for why it is reported rather than widened
    /// into here.
    #[test]
    fn no_verb_but_submit_is_checked_by_this_gate() {
        let roster = roster();
        for cmd in [
            WireCommand::MassCancel {
                venue: Some("binance".into()),
                symbol: None,
                account: Some("NOSUCH".into()),
            },
            WireCommand::Flatten {
                venue: "binance".into(),
                symbol: "SYM".into(),
                account: Some("NOSUCH".into()),
            },
            WireCommand::MarketExit {
                venue: Some("binance".into()),
                account: Some("NOSUCH".into()),
            },
            WireCommand::MarketExit { venue: None, account: None },
            WireCommand::MountStrategy {
                venue: "binance".into(),
                account: Some("NOSUCH".into()),
                symbol: "SYM".into(),
                interval: "1m".into(),
                controller_id: None,
                name: Some("buy_hold".into()),
                rhai: None,
                params: serde_json::json!({}),
            },
            WireCommand::Cancel("c-1".into()),
            WireCommand::Modify { client_order_id: "c-1".into(), new_qty: None, new_price: None },
            WireCommand::SetTradingState(WireTradingState::Halted),
        ] {
            assert_eq!(account_refusal(&cmd, &roster), None, "{cmd:?} is not this gate's");
        }
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

    /// **THE WIRE-TO-CORE HOP (this task).** A wire `Submit` naming an account must reach the
    /// core `OrderRequest` carrying it. Before this arm read the field, the `OrderRequest`
    /// literal's `..Default::default()` silently dropped it and every order routed to whichever
    /// engine happened to be first, regardless of what the client asked for — routing is still
    /// blind to it after this task (Task 4's job), but the value now survives the hop for that
    /// task to consume.
    mod submit_account_tests {
        use super::*;
        use vike_tradehub_client::wire::WireOrderRequest;

        /// Every field `client_order_id`'s non-empty gate and `OrderRequest`'s construction need,
        /// with `account` the one axis under test.
        fn submit(account: Option<&str>) -> WireCommand {
            WireCommand::Submit(WireOrderRequest {
                client_order_id: "c-1".into(),
                venue: "binance".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 1.0,
                order_type: "limit".into(),
                price: Some(1.0),
                trigger_price: None,
                reduce_only: false,
                account: account.map(str::to_string),
            })
        }

        fn submitted_order(wire: WireCommand) -> Result<vike_model::OrderRequest, String> {
            let (cmd, _coid) = lower_command(wire)?;
            let Command::Order(OrderIntent::Submit(order)) = cmd else {
                panic!(
                    "must lower to Command::Order(OrderIntent::Submit), got a different command"
                );
            };
            Ok(*order)
        }

        #[test]
        fn a_submit_naming_an_account_carries_it_into_the_core_order() {
            let order = submitted_order(submit(Some("ALT"))).expect("a valid label lowers");
            assert_eq!(
                order.account,
                Some(vike_model::account_keys::AccountLabel::Named("ALT".to_string())),
                "the account named on the wire must reach the core order"
            );
        }

        /// The wire's reserved spelling for "the unlabelled account, deliberately" —
        /// `parse_wire_account`'s own contract, admitted here exactly as `MountStrategy`'s arm
        /// admits it, where `policy.toml` refuses it.
        #[test]
        fn a_submit_naming_default_on_the_wire_carries_the_default_account() {
            let order = submitted_order(submit(Some("DEFAULT"))).expect("DEFAULT lowers");
            assert_eq!(order.account, Some(vike_model::account_keys::AccountLabel::Default));
        }

        /// Absence is unchanged: a client naming no account — every pre-existing caller — still
        /// lowers to `None`, byte-identical to before this task.
        #[test]
        fn a_submit_naming_no_account_lowers_to_none() {
            let order = submitted_order(submit(None)).expect("an account-less submit lowers");
            assert_eq!(order.account, None);
        }

        /// ⚠ The failure this task exists to prevent: `None` routes to the venue's DEFAULT book,
        /// so swallowing a malformed label into `None` (`.ok()`, `.unwrap_or_default()`, …) would
        /// silently trade the wrong account for an operator's typo. It must be REFUSED instead —
        /// the same shape `MountStrategy`'s arm already gives for the same parser's error.
        #[test]
        fn a_submit_naming_an_invalid_account_label_is_refused_not_dropped() {
            let err = submitted_order(submit(Some("bad label"))).expect_err("must refuse");
            assert!(err.contains("account"), "names the field: {err}");
        }
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

// ---------------------------------------------------------------------------------------------
// THE ACCOUNT PLANE'S CEREMONY AND ADVERTISEMENT (`docs/decisions/0065-accounts-are-managed-and-
// the-barrier-is-declared.md`).
//
// ⚠ Every test below hands the source a settings directory that DOES NOT EXIST, and that is the
// assertion rather than a shortcut: the ceremony is decided BEFORE the store is opened —
// `apply_set_setting`'s ordering, so a refused ceremony costs no lock and cannot be distinguished
// from an accepted one by timing what the store did. A refusal that reached the store first would
// fail here by returning the store's message instead of the ceremony's.
// ---------------------------------------------------------------------------------------------
#[cfg(test)]
mod account_plane_tests {
    use super::*;
    use vike_tradehub_client::wire::{AccountRequest, AccountVerb};

    /// A key name that IS in `vike_model::credential_keys` — so a refusal below is the ceremony's
    /// and never the validator's.
    ///
    /// ⚠ **DERIVED from the grid rather than spelled**, and not for elegance: a venue-prefixed
    /// literal in a file under `src/` is harvested by `crates/vike-ops/tests/settings_registry.rs`'s
    /// loose sweep, which then demands a `SETTINGS` row for it — and the NEAR-MISS spellings below
    /// name no key at all, so no row could honestly exist. Taking the first grid entry also keeps
    /// these tests true when the grid is reordered.
    fn real_key() -> String {
        vike_model::credential_keys::lookup_keys()
            .first()
            .expect("the credential grid is never empty")
            .clone()
    }

    /// `{BASE}__{LABEL}` — a LABELLED account's key, the second settable shape.
    fn labelled_key() -> String {
        format!("{}{}{}", real_key(), vike_model::account_keys::ACCOUNT_SEPARATOR, "HEDGE")
    }

    /// A SINGLE-underscore near-miss of the same base. Nothing reads it, and the separator is a
    /// DOUBLE underscore, so it is correctly outside the labelled grammar.
    fn near_miss_key() -> String {
        format!("{}_HEDGE", real_key())
    }

    /// The value these tests send. ⚠ It is a fixed non-secret token whose only job is to be
    /// searched for in every message: no refusal, no reply and no `Debug` may contain it.
    const SENT_VALUE: &str = "zzz-value-that-must-never-be-echoed-zzz";

    fn source() -> AccountAdminSource {
        AccountAdminSource {
            // Deliberately absent — see the module comment.
            settings_dir: std::path::PathBuf::from("/nonexistent/settings"),
            barrier: AccountBarrier::Loopback,
        }
    }

    fn actor() -> AccountActor<'static> {
        AccountActor { peer: Some("127.0.0.1:50001"), scope: "admin", key_id: Some("ab12cd34") }
    }

    fn refusal(verb: AccountVerb, confirm: Option<&str>) -> String {
        let req = AccountRequest { verb, confirm: confirm.map(str::to_string) };
        match source().apply(&req, &actor()) {
            Ok(r) => panic!("this request must be REFUSED, and it was answered with {r:?}"),
            Err(reason) => reason,
        }
    }

    fn set_credential_verb() -> AccountVerb {
        AccountVerb::SetCredential { key: real_key(), value: SENT_VALUE.to_string() }
    }

    /// ⚠ **THE CEREMONY, on the verb that carries a credential.** `apply_set_setting`'s policy
    /// contract verbatim — the precedent this surface was told to follow rather than invent one:
    /// the write is REFUSED unless `confirm` equals the exact thing being changed.
    ///
    /// The client's job is to make the operator TYPE it and never pre-fill it; this method's job
    /// is to refuse anything else, so no client can quietly skip it.
    #[test]
    fn a_credential_write_with_no_typed_confirm_is_refused() {
        let msg = refusal(set_credential_verb(), None);
        assert!(
            msg.contains("typed confirm"),
            "the refusal must say a typed confirm is required: {msg}"
        );
        assert!(
            msg.contains(&real_key()),
            "…and must name the exact spelling the operator has to type: {msg}"
        );
    }

    /// **Missing and mismatched get DISTINCT messages**, which is `apply_set_setting`'s own split:
    /// an operator who typed the wrong thing and an operator who typed nothing need different
    /// instructions, and one message for both is how somebody retypes the same wrong token.
    #[test]
    fn a_mismatched_confirm_is_refused_with_a_different_message_than_a_missing_one() {
        let missing = refusal(set_credential_verb(), None);
        let wrong = refusal(set_credential_verb(), Some(&near_miss_key()));
        assert!(wrong.contains("mismatch"), "the mismatch arm must say so: {wrong}");
        assert_ne!(
            missing, wrong,
            "missing and mismatched must not share a message — see `apply_set_setting`'s split"
        );
    }

    /// ⚠ **NEITHER REFUSAL QUOTES THE VALUE, and neither quotes what the caller typed.** 0065
    /// §4.5: *"the refusal quotes nothing the caller sent"* — the `ARGV_VALUE_REFUSAL` rule
    /// arriving on a different channel. Any rule reading the token's SHAPE is a guess about the
    /// secret's alphabet, so the answer is to echo none of it.
    #[test]
    fn no_ceremony_refusal_echoes_the_value_or_the_typed_confirm() {
        let typed = "some-wrong-thing-the-operator-typed";
        for msg in
            [refusal(set_credential_verb(), None), refusal(set_credential_verb(), Some(typed))]
        {
            assert!(!msg.contains(SENT_VALUE), "a refusal named the credential VALUE: {msg}");
            assert!(!msg.contains(typed), "a refusal echoed the operator's own token: {msg}");
        }
    }

    /// The other destructive verb. A REMOVE is refused unless the confirm equals the row id — the
    /// id rather than a label, because the id is what addresses the row and a label is not
    /// identity (`crates/vike-model/src/account_confirmation.rs`).
    #[test]
    fn a_remove_requires_the_typed_row_id() {
        let missing = refusal(AccountVerb::Remove { id: 7 }, None);
        assert!(missing.contains("typed confirm"), "{missing}");
        assert!(missing.contains('7'), "the refusal must name the id to type: {missing}");
        let wrong = refusal(AccountVerb::Remove { id: 7 }, Some("8"));
        assert!(wrong.contains("mismatch"), "{wrong}");
    }

    /// ⚠ **ONE derivation of what the ceremony IS**, consulted by the client that prompts and by
    /// the server that refuses — so the two surfaces cannot answer differently about it. This pins
    /// which verbs carry one: exactly the two that change something the node cannot put back.
    ///
    /// A client may call this to know WHETHER to prompt. It may NOT call it to FILL the box: the
    /// contract is that the operator types it, and a pre-filled confirm is a click.
    #[test]
    fn exactly_the_two_destructive_verbs_carry_a_ceremony() {
        assert_eq!(AccountVerb::Remove { id: 3 }.required_confirm().as_deref(), Some("3"));
        assert_eq!(set_credential_verb().required_confirm().as_deref(), Some(real_key().as_str()));
        for quiet in [
            AccountVerb::List,
            AccountVerb::Add {
                venue: "binance".to_string(),
                tier: "live".to_string(),
                label: None,
            },
            AccountVerb::Rename { id: 1, label: None },
            AccountVerb::SetActive { id: 1, active: false },
        ] {
            assert!(
                quiet.required_confirm().is_none(),
                "`{}` is reversible and must not demand a retype — ceremony on a reversible act is \
                 friction that teaches an operator to type past it",
                quiet.word()
            );
        }
    }

    /// A key outside the grid is refused, naming the KEY and never the value — 0036's reason 3,
    /// reused on this surface verbatim. ⚠ The refusal deliberately offers NO nearest-name
    /// suggestions: the nearest name to `HYPERLIQUID_LIVE_API_KEY__ALT` is a DIFFERENT ACCOUNT's
    /// live key.
    #[test]
    fn a_key_outside_the_grid_is_refused_and_names_no_value() {
        let req = AccountRequest {
            verb: AccountVerb::SetCredential {
                key: near_miss_key(),
                value: SENT_VALUE.to_string(),
            },
            // The ceremony PASSES, so what refuses below is the validator and nothing else.
            confirm: Some(near_miss_key()),
        };
        let msg = match source().apply(&req, &actor()) {
            Ok(r) => {
                panic!("a single-underscore near-miss reads nothing and must be refused: {r:?}")
            }
            Err(reason) => reason,
        };
        assert!(msg.contains(&near_miss_key()), "the refusal names the key: {msg}");
        assert!(!msg.contains(SENT_VALUE), "…and never the value: {msg}");
    }

    /// The two shapes that ARE settable are the same two `vike-cli secrets set` accepts: a name in
    /// the grid, and a LABELLED account's `{BASE}__{LABEL}` whose base resolves. The separator is a
    /// DOUBLE underscore, so the single-underscore near-miss above is correctly outside it.
    #[test]
    fn the_settable_key_shapes_are_the_grid_and_the_labelled_grammar() {
        assert!(is_settable_credential_key(&real_key()));
        assert!(is_settable_credential_key(&labelled_key()));
        assert!(!is_settable_credential_key(&near_miss_key()));
        assert!(!is_settable_credential_key(&labelled_key().to_lowercase()));
        assert!(!is_settable_credential_key("NOT_A_KEY_AT_ALL"));
        assert!(!is_settable_credential_key(""));
    }

    /// ⚠ **AN ABSENT STORE IS REFUSED, NOT CREATED** — 0036's reason 1 at its sharpest on this
    /// surface: the mere EXISTENCE of the settings database is the whole of `vike_secrets::Backend`'s
    /// per-run choice, so a writer that created one would make every credential in `secrets.env`
    /// unread on that box in the same act. `secrets migrate` stays the one creator.
    #[test]
    fn a_credential_write_against_an_absent_store_is_refused_and_creates_nothing() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let src = AccountAdminSource {
            settings_dir: dir.path().to_path_buf(),
            barrier: AccountBarrier::Contained,
        };
        let req = AccountRequest { verb: set_credential_verb(), confirm: Some(real_key()) };
        let msg = match src.apply(&req, &actor()) {
            Ok(r) => panic!("an absent store must be refused, not created: {r:?}"),
            Err(reason) => reason,
        };
        assert!(!msg.contains(SENT_VALUE), "the refusal named the value: {msg}");
        assert!(
            std::fs::read_dir(dir.path()).expect("the dir survives").next().is_none(),
            "nothing may be created on this path — a store is the operator's only copy of their \
             live venue keys, and bringing the DATABASE into existence would additionally retire \
             every credential in `secrets.env` on the box"
        );
    }

    /// ⚠ **THE REDACTING `Debug`.** `WireCommand` DERIVES `Debug` and 0065 §4.1 is the rule this
    /// impl exists for: a variant carrying a value may not ride that derive. The reachable formats
    /// are not hypothetical — a panic message in a test prints to a CI log.
    #[test]
    fn the_request_debug_prints_the_key_name_and_never_the_value() {
        let req = AccountRequest { verb: set_credential_verb(), confirm: Some(real_key()) };
        let rendered = format!("{req:?}");
        assert!(!rendered.contains(SENT_VALUE), "Debug printed the credential: {rendered}");
        assert!(rendered.contains(&real_key()), "…it must still name the KEY: {rendered}");
        assert!(rendered.contains("<set>"), "…as a presence mark: {rendered}");
    }

    /// The ADVERTISEMENT tracks the capability's ABSENCE, which is why this is the one string this
    /// node withholds conditionally. A node that advertised the verbs and then refused every frame
    /// would be telling its clients a barrier had been declared on a box where it had not.
    #[test]
    fn the_capability_string_is_advertised_only_when_the_capability_exists() {
        let armed = served_features(None, true);
        let bare = served_features(None, false);
        assert!(armed.iter().any(|f| f == FEATURE_ACCOUNT_VERBS));
        assert!(
            !bare.iter().any(|f| f == FEATURE_ACCOUNT_VERBS),
            "an unarmed node advertises nothing — the absence is what a conforming client reads \
             instead of sending a frame"
        );
        // …and the rest of the list is byte-identical, so arming this capability advertises no
        // second thing by accident.
        let without: Vec<&String> = armed.iter().filter(|f| *f != FEATURE_ACCOUNT_VERBS).collect();
        assert_eq!(without, bare.iter().collect::<Vec<&String>>());
    }

    /// **MAJOR 1 of the 2026-09-23 task-3/7 review.** The `sim` -> `paper` rename (ruling 7) put
    /// `paper` INTO `vike_secrets::ACCOUNT_TIERS`, and this refusal denied it by name while
    /// printing the very roster that now contains it — a five-path sweep of the rename's
    /// operator-facing prose that stopped one call site short of its own conclusion. Derived from
    /// the roster rather than a hand-copied word list, so the NEXT tier rename reddens this
    /// instead of rotting the way this one did.
    #[test]
    fn the_add_tier_refusal_does_not_deny_a_tier_the_roster_contains() {
        let msg = refusal(
            AccountVerb::Add {
                venue: "binance".to_string(),
                tier: "not-a-real-tier".to_string(),
                label: None,
            },
            None,
        );
        assert!(
            msg.contains(&vike_secrets::ACCOUNT_TIERS.join(" | ")),
            "must print the real roster: {msg}"
        );
        for tier in vike_secrets::ACCOUNT_TIERS {
            assert!(
                !msg.contains(&format!("`{tier}` is not"))
                    && !msg.contains(&format!("no `{tier}`")),
                "{tier} is in ACCOUNT_TIERS but this refusal denies it: {msg}"
            );
        }
    }
}

/// [`account_admission`] — the AUTHORIZATION half of `docs/decisions/0065`'s barrier, driven over
/// every scope the wire has.
///
/// ⚠ **This module exists because a mutation proved the decision was unratcheted.** On 2026-09-17
/// the accepting arm was widened to `(Some(src), _)` and the scope refusal deleted — any
/// authenticated peer, Observe included, reaching `SetCredential` — and `cargo nextest run` over
/// `vike-tradehub`, `vike-tradehub-client`, `vike-ops`, `vike-cli`, `vike-connections` and
/// `vike-secrets` reported **2911 passed**. Nothing looked. The decision could not be driven from a
/// test at all while it lived inside the `Request::Account` arm, because `Request::Account` has one
/// construction site in the tree and it is that arm; every account test called
/// `AccountAdminSource::apply` directly, BELOW the check.
///
/// So the assertions below are deliberately exhaustive over `Scope` rather than a happy-path pair:
/// a fourth variant must redden this file rather than silently inherit whichever branch it falls in.
#[cfg(test)]
mod account_admission_tests {
    use super::*;

    /// Every scope the wire can authenticate under. Spelled as a literal list rather than derived,
    /// so that ADDING a variant breaks this line and forces a decision about it here.
    const EVERY_SCOPE: [Scope; 3] = [Scope::Read, Scope::Write, Scope::Account];

    #[test]
    fn only_the_admin_scope_is_admitted_and_only_when_the_capability_exists() {
        for scope in EVERY_SCOPE {
            let verdict = account_admission(true, scope);
            if scope == Scope::Account {
                assert!(
                    verdict.is_ok(),
                    "the ADMIN scope is the one this surface is for, and it was refused"
                );
            } else {
                let refusal = verdict.expect_err(
                    "a non-Admin scope must be REFUSED on an armed box — this is the half that \
                     keeps the key a desktop carries to place orders from being the key that \
                     writes key material",
                );
                assert!(
                    refusal.contains("requires the ADMIN scope"),
                    "an armed box must refuse by AUTHORIZATION, naming the scope; got: {refusal}"
                );
            }
        }
    }

    #[test]
    fn an_unarmed_node_refuses_every_scope_including_admin() {
        for scope in EVERY_SCOPE {
            let refusal = account_admission(false, scope).expect_err(
                "a node that declared no barrier holds no writer, so there is nothing to refuse \
                 WITH — and that must hold for Admin too, or the declaration is not the gate",
            );
            assert!(
                refusal.contains("is not armed on this node"),
                "an unarmed box must refuse by ABSENCE, not by authorization; got: {refusal}"
            );
        }
    }

    /// The two refusals must not be confusable. An operator who reads one and goes looking for the
    /// other's cause has been sent to the wrong place, which is the whole reason they are distinct
    /// strings rather than one message.
    #[test]
    fn the_absence_refusal_and_the_authorization_refusal_are_different_facts() {
        let unarmed = account_admission(false, Scope::Write).expect_err("unarmed refuses");
        let unauthorized = account_admission(true, Scope::Write).expect_err("wrong scope refuses");
        assert_ne!(unarmed, unauthorized);
        assert!(
            !unarmed.contains("requires the ADMIN scope"),
            "an unarmed box must not tell the operator to go find an admin key — the key is not \
             what is missing"
        );
        assert!(
            !unauthorized.contains("is not armed on this node"),
            "an armed box must not report itself unarmed — the operator would go declare a \
             barrier that is already declared"
        );
    }
}

/// **The handshake's closed-gate check** — [`scope_admission`], the FIRST of the account
/// boundary's two layers (`docs/decisions/0070`).
///
/// ⚠ These tests did not exist before the function did, and could not: the check lived inside
/// `run_handshake`, which takes a `&mut TcpStream`. Measured on `main` at the time — nothing under
/// `crates/vike-tradehub/tests/` named `run_handshake` or `Scope::Account` at all, so the one check
/// between a peer CLAIMING admin and the account plane was covered by nothing.
#[cfg(test)]
mod scope_admission_tests {
    use super::*;

    /// Every scope the wire can authenticate under. A literal list, matching
    /// `account_admission_tests`' reason: ADDING a variant must break this line and force a
    /// decision about it here rather than defaulting into the permissive arm.
    const EVERY_SCOPE: [Scope; 3] = [Scope::Read, Scope::Write, Scope::Account];

    fn keys(control: bool, admin: bool) -> NodeKeys {
        let k = NodeKeys::new(
            b"observe-key".to_vec(),
            if control { b"control-key".to_vec() } else { Vec::new() },
        );
        if admin { k.with_admin(b"admin-key".to_vec()) } else { k }
    }

    /// **A node holding no admin key refuses the ADMIN claim — without consulting a key.** That is
    /// the closed-gate property: an absent key is not an empty key to compare a MAC against, it is
    /// a scope that cannot be reached.
    #[test]
    fn a_node_with_no_admin_key_refuses_the_admin_claim() {
        let reason = scope_admission(Scope::Account, &keys(true, false))
            .expect_err("a node holding no admin key must refuse the claim");
        assert!(reason.contains("account administration is not armed"), "{reason}");
    }

    /// ...and the same for CONTROL, which is the older half of the same gate.
    #[test]
    fn a_node_with_no_control_key_refuses_the_control_claim() {
        let reason = scope_admission(Scope::Write, &keys(false, false))
            .expect_err("a node with control disabled must refuse the claim");
        assert!(reason.contains("control disabled"), "{reason}");
    }

    /// ⚠ **The complement, and it is the half that stops this gate being a capability regression:**
    /// an ARMED node admits every scope it holds a key for. Without this, both tests above are
    /// satisfied by a function that refuses everything.
    #[test]
    fn an_armed_node_admits_every_scope_it_holds_a_key_for() {
        let armed = keys(true, true);
        for scope in EVERY_SCOPE {
            assert!(
                scope_admission(scope, &armed).is_ok(),
                "an armed node must admit {scope:?} — the MAC below is what then decides"
            );
        }
    }

    /// **OBSERVE is never gated here**, on any node, and that is deliberate rather than an
    /// oversight: the observe key is the one capability every node has, so there is no "unarmed"
    /// state for it to be refused from. Pinned so a future arm cannot quietly start gating it and
    /// take every read-only client down with it.
    #[test]
    fn the_observe_scope_is_admitted_even_by_a_node_armed_for_nothing_else() {
        assert!(scope_admission(Scope::Read, &keys(false, false)).is_ok());
    }

    /// The two refusals are DIFFERENT facts and must read differently — a post-incident review
    /// searches the log by text, and `crates/vike-tradehub/src/server.rs`'s two `warn!` sentences
    /// are what it finds. Same shape as `account_admission_tests`' own distinctness assertion.
    #[test]
    fn the_admin_and_control_refusals_are_different_facts() {
        let admin = scope_admission(Scope::Account, &keys(true, false)).expect_err("unarmed admin");
        let control =
            scope_admission(Scope::Write, &keys(false, true)).expect_err("unarmed control");
        assert_ne!(admin, control, "two different absences may not print one sentence");
    }
}
