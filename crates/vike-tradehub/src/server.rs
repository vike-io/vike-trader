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
//! request/response loop, and each [`Request::Command`] is lowered ([`control::lower_command`]) into the core's
//! real `Command`/`OrderIntent` and handed to the [`vike_core::CommandSink`] the daemon threaded in.
//! Control is DOUBLE-GATED: the node must hold a control key ([`NodeKeys::has`]) AND the daemon must
//! pass `Some(sink)` (gated on `VIKE_TRADEHUB_CONTROL=1`). Absent either, a `Control` auth or a
//! `Command` is refused. An [`Scope::Read`] peer's `Command` is always refused (read-only). Every
//! ACCEPTED command is audit-logged ([`crate::audit`]). Beyond the double gate, the server edge
//! NOTIONAL- and RATE-limits each command ([`ControlLimits`], built per connection from the
//! [`ControlLimitsConfig`] the daemon BINARY resolves once — the notional ceiling from the POLICY
//! setting (`policy.max_notional_per_order`; it has NO env layer since Phase 5
//! of the settings-unification design), the rate from `VIKE_TRADEHUB_CONTROL_RATE` — and passes
//! into [`serve`] — audit F13: this module reads no env itself, per the settings-registry rule
//! that libraries take configuration as parameters) —
//! defense-in-depth so a leaked control key can't place an oversized order or flood the core, on
//! top of the core `RiskGate` every command still passes through.
//!
//! A [`Request::Command`] may carry an optional operator/agent RATIONALE (v4) beside the command.
//! It is recorded in the audit trail and NOWHERE else: this arm runs it through
//! [`crate::audit::sanitize_reason`] (control characters stripped, length capped — it is remote free text
//! landing in a structured JSON log line) and hands the result to [`crate::audit::record`]. It is never
//! part of [`control::lower_command`]'s input, so it can never reach `OrderRequest`, the core fold, the
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
//! - **`WireCommand::UpdateParams`** (write, `Scope::Write`): lowered by [`control::lower_command`] into
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
//! [`crate::audit::sanitize_reason`] -> [`control::lower_command`] ->
//! `CommandSink::try_command` ->
//! [`crate::audit::record`], in that order — and BOTH surfaces call it. Identical gating by CONSTRUCTION,
//! not by review. The TCP arm keeps its own scope/sink gates (they are connection properties) and
//! renders [`control::AcceptError`] back onto the exact `Response::Error` strings it always sent.
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
//! [`vike_tradehub_client::proto::FEATURE_VENUE_ROUTING`] so a client can tell it from one that still misroutes silently; the
//! client half of that is `vike_app_core::backend::tradehub_control::venue_routing_verdict`.
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
//! ⚠ **…and a runtime MOUNT is checked by the same gate**, which it was NOT when the order plane
//! shipped: [`vike_tradehub_client::wire::WireCommand::MountStrategy`] names an account too and was a declared residual left
//! for a follow-up. The stakes are a rung higher than an order's — a misrouted mount is every
//! order that strategy will ever place — and the failure wore a worse coat, because a mount's SPEC
//! validates fine ([`crate::mount_factory::validate_spec`] reads the strategy, never the account),
//! so the un-gated reply was a plain `Ack` rather than a differently-worded error. The two planes
//! share one sentence and differ only in its subject.
//!
//! ⚠ **…and so are the three risk-REDUCING verbs, when they name one** (owner ruling "B",
//! 2026-09-26). `MassCancel`, `Flatten` and `MarketExit` carried an `account` on the wire that
//! [`control::lower_command`] dropped, and the core then fanned the verb over every account of the venue —
//! so a `market-exit binance ALT` cancelled and flattened the DEFAULT account too. The field is
//! carried and honoured now, and [`account_refusal`] refuses a named account this node does not
//! hold in the same sentence with its own subject, plus one refusal of its own: an account named
//! with NO venue, which can resolve on no roster and would otherwise reach the global arm. An
//! ABSENT account is never checked — §4.5's fan-out, and the unscoped panic button, which no gate
//! of this family may refuse.
//!
//! ## The LIVE-JOURNAL REPORT verb (ruling 16) — the one verb that READS THIS NODE'S DISK
//!
//! [`Request::Tearsheet`] asks this node to fold the command journal IT is writing into a
//! `vike_analytics::LiveTearsheet` and reply with the document's JSON ([`tearsheet_reply`]).
//! Post-auth under EITHER scope, like `Snapshot`/`StrategyStatus` — it executes nothing, so no
//! audit record — and advertised as [`vike_tradehub_client::proto::FEATURE_TEARSHEET`].
//!
//! ⚠ It is the first verb here whose answer comes from the FILESYSTEM rather than from the
//! publisher's snapshot cell, and that is worth knowing for two reasons. The read happens on the
//! CONNECTION thread, so a large journal costs that one peer's thread and nothing else — the
//! hot-path guarantee below is unchanged, since nothing on this path touches the core. And the
//! journal is resolved from the daemon's own startup environment sweep, which is one rung short of
//! what the daemon itself resolves; [`tearsheet_reply`] names the missing rung and what the reply
//! says instead of pretending.
//!
//! [`control::AcceptError`] is an ENUM rather than the plain `String` a first sketch had, for one behavioral
//! reason: a `CommandRejected::Gone` (the core is shutting down) must CLOSE the connection, while a
//! `Busy`/refusal must not. Collapsing both into one string would silently drop that distinction on
//! the security-sensitive write path, so the variant carries it ([`control::AcceptError::is_fatal`]) and
//! [`control::AcceptError::message`] carries the operator-visible text both surfaces render.
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
//!   [`vike_tradehub_client::proto::FEATURE_OBSERVE_HEARTBEAT`] so a client may deadline its read and stop mistaking a dead link
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
//! ## The GUI's observe mode — BUILT (PR-13; `vike-app --observe` then)
//!
//! This server + [`crate::publish`] + `vike_tradehub_client::{RemoteCoreHandle, RemoteControlHandle}`
//! are the CI-testable core. `vike-desktop` — every launch, since the desktop lost its local core —
//! drives the GUI panels from a `RemoteCoreHandle` (watch) and, with the control gate/key, its
//! order buttons from a `RemoteControlHandle` (trade). That GUI wiring is local-verify (the
//! desktop's own tests run only in the `app-check` job); the wire + server + limits here are
//! covered by `tests/control_roundtrip.rs` and the unit tests below.

use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_core::CommandSink;
use vike_tradehub_client::proto::{Request, Response, Scope, read_frame_raw, write_frame};
use vike_tradehub_client::wire::{AccountRequest, WireMountRow, WireStrategyStatus};
use vike_tradehub_client::{NodeKeys, liveness};

use crate::publish::{PublisherHandle, Recv};

use self::accounts::{AccountActor, AccountAdminSource, account_admission};
use self::control::{Accepted, ControlLimits, ControlLimitsConfig, accept_command, command_kind};
use self::handshake::{HandshakeOutcome, run_handshake};
use self::refusal::{account_refusal, bracket_refusal, venue_refusal};
use self::session::serve_session;
use self::settings::SettingsShowSource;
use self::tearsheet::tearsheet_reply;

// The node server's parts, split out of this file as pure moves — what each holds is its own module
// doc. The `pub` ones carry public items (`vike_tradehub::server::settings::SettingsShowSource`,
// `vike_tradehub::server::control::accept_command`, …); `handshake` and `tearsheet` carry none.
pub mod accounts;
pub mod control;
mod handshake;
pub mod refusal;
mod session;
pub mod settings;
mod tearsheet;

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
/// `datahub_advertise` is the REQ-2 datahub advertisement (`config.datahub_advertise_addr`,
/// resolved once by the daemon binary like `limits`): `Some(addr)` ⇒
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
    // Same shape for the REQ-2 advertisement (`config.datahub_advertise_addr`, resolved
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
    if !open_connection(&stream, peer, policy) {
        return;
    }

    // --- Phase 1: handshake (carries the granted scope forward) ---
    let Some(scope) = authenticate(
        &mut stream,
        &keys,
        peer,
        datahub_advertise.as_deref(),
        accounts.is_some(),
        policy,
    ) else {
        return;
    };

    // --- Phase 2: authed Observe session, until Subscribe ---
    serve_session(
        stream, publisher, &keys, commands, limits, settings, accounts, scope, peer, policy,
    );
}

/// Before [`handle_connection`]'s phase 1: the socket's own settings, applied before the first
/// frame is read — Nagle off, and the UNAUTHENTICATED read bound. `false` ⇒ the bound could not be
/// applied and the connection must close.
fn open_connection(stream: &TcpStream, peer: Option<SocketAddr>, policy: LinkPolicy) -> bool {
    // Nagle OFF before the first frame — a control command's answer, and every observe snapshot the
    // push writer sends after it. Why, and why a refusal is only logged:
    // `vike_node_proto::frame::configure_node_stream`.
    if let Err(e) = vike_node_proto::frame::configure_node_stream(stream) {
        tracing::warn!(?peer, error = %e, "vike-tradehub node: TCP_NODELAY refused; frames may wait for an ACK");
    }

    if let Err(e) = stream.set_read_timeout(Some(policy.handshake_read_timeout)) {
        tracing::warn!(?peer, error = %e, "vike-tradehub node: could not set read timeout, closing");
        return false;
    }
    true
}

/// [`handle_connection`]'s phase 1: the handshake, then the REPLACEMENT of the read bound it ran
/// under. `Some(scope)` is the [`Scope`] the peer authenticated under; `None` ⇒ the connection
/// must close.
fn authenticate(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    peer: Option<SocketAddr>,
    datahub_advertise: Option<&str>,
    account_admin: bool,
    policy: LinkPolicy,
) -> Option<Scope> {
    let scope = match run_handshake(stream, keys, peer, datahub_advertise, account_admin) {
        HandshakeOutcome::Authed(scope) => scope,
        HandshakeOutcome::Closed => {
            tracing::info!(?peer, "vike-tradehub node: connection closed (handshake refused)");
            return None;
        }
    };

    // THE REPLACEMENT (see `handle_connection`'s doc). The bound `open_connection` set exists to
    // stop an UNAUTHENTICATED peer pinning this thread; the peer has now proved possession of a scoped
    // key, so it gets the authed policy instead. A failure here is fatal to the connection rather
    // than ignored: carrying on would leave the handshake's bound in force, which is precisely the
    // defect — an authed connection silently living under a stranger's timer.
    if let Err(e) = stream.set_read_timeout(policy.authed_idle_timeout) {
        tracing::warn!(
            ?peer, ?scope, error = %e,
            "vike-tradehub node: could not apply the authed idle policy, closing"
        );
        return None;
    }
    Some(scope)
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
/// change (`vike-app-core`'s observe bridge gates on `seq != last_seq`; `mcp`'s snapshot tool waits
/// for a stamped `identity` or `seq > 0`, and its order path for `seq > 0` — all fine today, none
/// structurally guaranteed). `Pong` cannot be mistaken for anything: `RemoteCoreHandle`'s receive
/// loop has always dropped every non-`SnapshotFrame` reply on the floor, so this is invisible to an
/// old client by CONSTRUCTION rather than by convention.
/// It is also the smallest frame the protocol has, and it is not a wire change at all — the variant
/// has existed since PR-10 — which is what lets the capability be advertised
/// ([`vike_tradehub_client::proto::FEATURE_OBSERVE_HEARTBEAT`]) instead of forcing a `NODE_PROTO_VERSION` bump the signed auth
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

#[cfg(test)]
mod tests;
