//! `RemoteControlHandle` — the WRITE half of the thin-client node connection (headless two-layer
//! plan, Layer 2, PR-12/PR-13): the `try_command`-shaped twin of [`crate::remote_handle::RemoteCoreHandle`].
//!
//! A laptop GUI holds this handle to send order commands to a live headless node's CONTROL server. It
//! authenticates under [`Scope::Control`] and — load-bearing — NEVER subscribes: a subscribed connection
//! is a one-way push pipe (the server only writes after `Subscribe`), so control MUST stay in the
//! server's request/response loop on its OWN dedicated connection. After the handshake it hands each
//! [`WireCommand`] to a background WORKER thread over a bounded [`SyncSender`]; the worker owns the
//! socket, writes `Request::Command`, and reads the `Ack`/`Error` reply. This mirrors
//! `CoreHandle::try_command`'s non-blocking contract: the UI thread NEVER blocks on network I/O — a
//! full queue is `ControlRejected::Busy`, a dead worker is `ControlRejected::Gone`.
//!
//! # Fire-and-forget, like the in-process command path
//!
//! [`try_command`](RemoteControlHandle::try_command) returns the instant the command is enqueued — it
//! does NOT wait for the server's `Ack` (the worker consumes it). A transport fault ends the worker
//! and flips [`is_connected`](RemoteControlHandle::is_connected) to `false`.
//!
//! # Two outcome surfaces, and which one a caller must use
//!
//! Enqueueing hands back a [`CommandTicket`] — the IDENTITY of that ONE command — and
//! [`await_outcome`](RemoteControlHandle::await_outcome) resolves that ticket to a
//! [`CommandOutcome`] and no other. **Any caller that REPORTS a command's result must use this
//! surface.** [`last_error`](RemoteControlHandle::last_error) is the other one: a LATCHING "most
//! recent server error" for a status strip, cleared only by an explicit
//! [`clear_last_error`](RemoteControlHandle::clear_last_error). The two are deliberately
//! independent — awaiting an outcome never clears the latch, clearing the latch never touches a
//! ticket — because they answer different questions.
//!
//! ⚠ **Why the latch cannot answer "how did MY command go".** It is written on every
//! `Response::Error` and never cleared, so a caller that polls it after each write reports the
//! FIRST refusal for every later command in the session — including commands the node accepted and
//! EXECUTED. It points the dangerous way: an operator whose oversized order was correctly refused
//! then fires `market-exit`, is told it was rejected, and believes they still hold a position they
//! have closed. `vike-cli trade` and `vike-cli mcp` both did exactly that; both await a ticket now.
//! Gated by `vike-tradehub`'s `a_refusal_is_not_reported_again_for_the_next_accepted_command`.
//!
//! # NEVER-SENT is not UNKNOWN, and the caller may act on the difference
//!
//! [`CommandOutcome`] separates the two ways a command can fail to produce a verdict. A command the
//! worker WROTE and never got an answer for is [`CommandOutcome::Disconnected`] — genuinely unknown,
//! it may have executed, and nobody may resend it. A command the worker found the link already dead
//! for, or that was still queued when the worker stopped, is [`CommandOutcome::NeverSent`] — not one
//! byte of it reached the node, so a caller may reconnect and send THAT command with no
//! double-execution risk (`vike-cli mcp`'s `Server::execute` does exactly this, on the same call).
//!
//! The evidence for `NeverSent` is positive, never inferred: [`link_is_dead`] peeking a peer FIN
//! before the write, or the queue outliving the worker. A FAILED WRITE stays `Disconnected`,
//! because `write_all` cannot say whether it wrote half a frame first. The asymmetry is deliberate
//! — a wrong `Disconnected` costs a `node_snapshot`; a wrong `NeverSent` places an order twice.
//!
//! # A link that dies in SILENCE — the write half's version, and what the probe cannot see
//!
//! [`link_is_dead`] answers for a close the socket REPORTED, and cannot answer for one it did not:
//! silence peeks `WouldBlock`, which is the correct verdict for the ordinary quiet link and is
//! therefore unavailable as a verdict for a dead one. So the probe is not, and cannot be, the
//! detector for a tunnel that dies without a FIN or an RST — the write into the kernel send buffer
//! succeeds, and the reply read is where the truth is.
//!
//! That read is now deadlined at [`crate::liveness::CONTROL_REPLY_TIMEOUT`], armed once at connect.
//! Before it, the read blocked with no deadline and no OS keepalive behind it, so the worker parked
//! for the life of the process: `connected` stayed `true` (it is stored `false` only after the
//! loop), nothing reconnected, every later command queued behind the wedge, and `vike-cli mcp`
//! answered each of them "sent, outcome unknown — it may still execute" for a command that had
//! never left this process. On expiry the in-flight command is `Disconnected` (it WAS written),
//! everything still queued is `NeverSent`, and the caller's existing recovery does the rest.
//!
//! # How a reply is correlated with its request — with NO wire change
//!
//! The node protocol carries no request id, and adding one would mean bumping
//! [`crate::proto::NODE_PROTO_VERSION`] — which is folded INTO the signed auth message, so the bump
//! would fail the HANDSHAKE against every already-running node. It is also unnecessary: the
//! correlation already exists structurally. A control connection NEVER subscribes, the server
//! answers each request with EXACTLY one frame before reading the next, and the worker below is a
//! strictly serial `write_frame` → `read_frame` loop over that one socket — so the reply it reads
//! always belongs to the command it just wrote. The defect was never a missing correlation; it was
//! throwing that knowledge away into an identity-free cell. The ticket is a CLIENT-side monotonic
//! sequence carried ALONGSIDE the queued command (not derived from queue order), so concurrent
//! senders racing on `try_send` can never mis-attribute an outcome either.
//!
//! # vike-core-free by construction
//!
//! [`ControlRejected`] is CRATE-LOCAL (not `vike_core::CommandRejected`): this crate stays the LIGHT,
//! DataFusion-free, vike-core-free thin-client wire crate. std only — no new external dependency.
//!
//! # Preview (dry-run) — a synchronous, per-call twin
//!
//! [`preview_command`] is the SYNCHRONOUS request/response sibling of the fire-and-forget command
//! path: it opens a fresh short-lived `Control` connection (the same handshake as [`RemoteControlHandle::connect`]),
//! sends [`Request::Preview`], reads the [`Response::Preview`] verdict, and drops the connection —
//! nothing is executed on the node. A per-call connection is correct because a preview is a single
//! request/response with no ordering concern (unlike a command, which needs the persistent worker).
//!
//! # The optional rationale (v4)
//!
//! [`try_command_with_reason`](RemoteControlHandle::try_command_with_reason) carries an OPTIONAL
//! operator/agent rationale BESIDE the command; the node records it (sanitized) in its audit trail
//! and it never reaches the order itself. [`try_command`](RemoteControlHandle::try_command) keeps
//! its exact pre-v4 signature as a thin wrapper passing `None`, so every existing caller (the
//! `vike-app --observe` GUI's order buttons) is unchanged.

use std::collections::VecDeque;
use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::handshake::{node_handshake, resp_kind};
use crate::liveness::CONTROL_REPLY_TIMEOUT;
use crate::proto::{
    FEATURE_MOUNT_VERBS, FEATURE_SETTINGS_WRITE, FEATURE_STRATEGY_VERBS, Request, Response, Scope,
    read_frame, write_frame,
};
use crate::wire::WireCommand;

/// The `Welcome.features` capability a [`WireCommand`] requires before it may be SENT, or `None`
/// for the pre-negotiation vocabulary every server speaks. The one authority both the async
/// ([`RemoteControlHandle::try_command_with_reason`]) and per-call ([`preview_command`]) write
/// paths consult, so the two cannot disagree about which verbs are gated.
fn required_feature(cmd: &WireCommand) -> Option<&'static str> {
    match cmd {
        WireCommand::UpdateParams { .. } => Some(FEATURE_STRATEGY_VERBS),
        // The B5 mount verbs ride their OWN capability, not `strategy-verbs`: a B4-era node
        // advertises that string yet cannot decode these variants — see `FEATURE_MOUNT_VERBS`'s
        // doc for why re-using the old string would defeat the client-side refusal.
        WireCommand::MountStrategy { .. } | WireCommand::UnmountStrategy { .. } => {
            Some(FEATURE_MOUNT_VERBS)
        }
        // The REQ-7 settings write rides its OWN capability (not `settings-show`): a read-half
        // node advertises that string yet cannot decode this variant — the mount-verbs argument
        // verbatim.
        WireCommand::SetSetting { .. } => Some(FEATURE_SETTINGS_WRITE),
        _ => None,
    }
}

/// Has the peer already closed this control connection? A NON-DESTRUCTIVE, NON-BLOCKING probe the
/// worker runs immediately before writing a command, so a command offered to a link the node has
/// already closed is reported [`CommandOutcome::NeverSent`] instead of UNKNOWN.
///
/// # Why a probe exists at all
///
/// A control connection is request/response and the worker parks in its COMMAND CHANNEL between
/// commands, never in a socket read — so a `FIN` that arrived while it was idle sits unread in the
/// receive queue and the worker learns nothing from it. Adding a reader thread just to notice would
/// mean two threads on one socket and a correlation problem where there is currently none (the
/// module doc's "the reply it reads always belongs to the command it just wrote"). Peeking is the
/// same knowledge without the thread: the FIN is already there, so ask.
///
/// # The mechanics, and the one thing that must not be got wrong
///
/// `peek` is `recv(MSG_PEEK)`: it inspects without consuming, so a legitimately-queued reply is
/// still there for the read that follows. The socket is put in NON-BLOCKING mode for the probe and
/// put back: a probe under a read TIMEOUT would block for that timeout on every healthy idle link
/// (there is nothing to read — that is the normal state), taxing every command; non-blocking
/// answers `WouldBlock` instantly, which is exactly the "alive and quiet" verdict.
///
/// ⚠ If the mode cannot be RESTORED the connection is declared dead. Leaving a non-blocking socket
/// behind would turn the worker's next `read_frame` into a `WouldBlock` error that reads as a
/// transport fault — an UNKNOWN outcome for a command that really was sent. Failing here instead
/// costs a reconnect and cannot mis-report anything.
///
/// Verdicts: `Ok(0)` = the peer sent FIN (a clean close — the node's own, or a tunnel's) ⇒ DEAD.
/// `Ok(n>0)` = unread bytes, which on a strictly serial request/response socket means the peer is
/// very much alive (and a desync the reply read will report honestly) ⇒ alive. `WouldBlock` = the
/// ordinary quiet link ⇒ alive. Any other error (an RST, a socket the OS has torn down) ⇒ DEAD.
///
/// ⚠ **What this probe structurally CANNOT see: a link that died in silence.** No FIN arrives, so
/// the peek is `WouldBlock` and the verdict is ALIVE — correctly, because that is the same thing a
/// healthy quiet link says, and a probe that called silence death would report `NeverSent` for
/// every command on every idle connection. Detecting a silent death is the REPLY READ's job, under
/// [`crate::liveness::CONTROL_REPLY_TIMEOUT`]; see the module doc. Do not "strengthen" this
/// function to cover it — the two answers are not distinguishable at this point in the loop, and
/// the wrong one here is the one that places an order twice.
fn link_is_dead(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut probe = [0u8; 1];
    let dead = match stream.peek(&mut probe) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
        Err(_) => true,
    };
    // Restore BEFORE answering: every later read on this socket must block again.
    if stream.set_nonblocking(false).is_err() {
        return true;
    }
    dead
}

/// Outbound command queue depth. A handful of commands is ample slack for a briefly-busy worker
/// (each command is a sub-millisecond localhost round-trip); once exceeded, the UI thread gets
/// `ControlRejected::Busy` rather than blocking — the never-block contract.
const CONTROL_QUEUE_CAP: usize = 64;

/// How many resolved command outcomes the board retains. A caller awaits the ticket it just got, so
/// one is enough in practice; the ring exists because a FIRE-AND-FORGET caller (the `vike-app`
/// order buttons) never awaits anything, and an unbounded map of outcomes nobody will ever read is
/// a leak. Sized to [`CONTROL_QUEUE_CAP`] so every command that can be in flight at once keeps its
/// answer.
const OUTCOME_RETENTION: usize = CONTROL_QUEUE_CAP;

/// One queued outbound command: its [`CommandTicket`] sequence, the [`WireCommand`], and the
/// OPTIONAL operator/agent rationale (v4). The command and rationale travel together only as far as
/// the wire — the node records the rationale in its audit trail and lowers ONLY the command, so the
/// reason never reaches an order, the fold, or a venue. The sequence never reaches the wire at all:
/// it rides with the payload purely so the worker can file the reply under the right ticket, which
/// keeps attribution correct even when two threads race on `try_send`.
struct Outbound {
    seq: u64,
    cmd: WireCommand,
    reason: Option<String>,
}

/// The identity of ONE enqueued command, handed back by
/// [`RemoteControlHandle::try_command`]/[`try_command_with_reason`](RemoteControlHandle::try_command_with_reason)
/// and redeemed at [`RemoteControlHandle::await_outcome`]. Opaque and `Copy`: a caller that does not
/// care (the fire-and-forget GUI order buttons) simply drops it. Deliberately NOT `#[must_use]` —
/// fire-and-forget is a legitimate use of this handle, and a lint on the common path teaches people
/// to write `let _ =`, which is precisely the reflex that hid this bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandTicket(u64);

/// What the node did with ONE command — the answer [`RemoteControlHandle::await_outcome`] resolves a
/// [`CommandTicket`] to. Exhaustive over the worker's serial reply loop: every command either gets
/// its reply or loses the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The node ACCEPTED it (`Response::Ack`), carrying the client-order-id it minted or echoed.
    /// Empty for the account-wide verbs (mass-cancel / flatten / market-exit / trading-state),
    /// which have no single order to name.
    Accepted {
        /// The minted/echoed client-order-id; empty for an account-wide verb.
        coid: String,
    },
    /// The node REFUSED it (`Response::Error` — an edge-limit refusal, control not enabled, a
    /// command it declined to lower) or answered something that is not a reply to a command at all
    /// (e.g. `AuthDenied` on a read-only peer). Nothing was executed. The string is the node's own
    /// text, unmodified.
    Refused(String),
    /// The control connection died before the node's reply was read, so **the outcome is genuinely
    /// UNKNOWN** — the command may or may not have executed. Never report this as "sent": before
    /// this arm existed a mid-flight transport fault left `last_error` empty and every caller
    /// cheerfully printed success for a command that was never confirmed.
    Disconnected,
    /// The link was ALREADY dead when this command reached the worker, so **not one byte of it went
    /// on the wire**. Distinct from [`Self::Disconnected`] in exactly the way that matters to a
    /// caller: there is no double-execution risk in sending it again, because there was no first
    /// execution to double. A caller may reconnect and send THIS command; it must never do that for
    /// a `Disconnected`.
    ///
    /// ⚠ The distinction is CONSERVATIVE and asymmetric on purpose. This variant is produced only
    /// where the worker has POSITIVE evidence that nothing was written — the pre-write liveness
    /// probe seeing EOF ([`link_is_dead`]), or a command still queued behind a worker that had
    /// already stopped. Everything ambiguous — above all a `write_frame` that FAILED, which
    /// `write_all` cannot tell apart from one that wrote half a frame first — stays
    /// [`Self::Disconnected`]. Being wrong in this direction costs a `node_snapshot` round trip;
    /// being wrong in the other direction places an order twice.
    ///
    /// Its ROUTINE trigger used to be the node's own idle timer (an authed control connection
    /// closed after five minutes of quiet, so every write after a long pause met a socket the node
    /// had stopped reading). That is fixed at the node — `crate::liveness::AUTHED_IDLE_TIMEOUT` —
    /// so what remains here is a daemon restart, a tunnel blip, and any other clean close.
    NeverSent,
}

/// The worker → caller outcome board: resolved `(ticket, outcome)` pairs plus the worker's
/// end-of-life flag, behind one [`Mutex`] with a [`Condvar`] so a waiter is woken the instant its
/// reply lands instead of sleeping a fixed settle window.
#[derive(Default)]
struct BoardState {
    /// Resolved outcomes, oldest first, bounded to [`OUTCOME_RETENTION`].
    done: VecDeque<(u64, CommandOutcome)>,
    /// `Some(outcome)` once the worker has exited: every ticket it never resolved answers THAT
    /// outcome forever, so a waiter fails fast instead of burning its whole timeout on a dead
    /// connection.
    ///
    /// It carries the outcome rather than a bare "finished" flag because the honest answer differs
    /// by how the worker died, and the difference is the whole point of
    /// [`CommandOutcome::NeverSent`]: a command still sitting in the queue when the worker stopped
    /// was never written, whatever killed the worker — while the ONE command that was in flight
    /// (written, its reply never read) is `Disconnected`, and the worker records that explicitly
    /// under its own ticket BEFORE finishing, so this fallback never overwrites it.
    terminal: Option<CommandOutcome>,
}

#[derive(Default)]
struct OutcomeBoard {
    state: Mutex<BoardState>,
    resolved: Condvar,
}

impl OutcomeBoard {
    /// File one command's outcome under its ticket sequence, evicting the oldest entry once the
    /// retention window is full, and wake every waiter.
    fn record(&self, seq: u64, outcome: CommandOutcome) {
        let mut st = self.state.lock().expect("outcome board poisoned");
        if st.done.len() >= OUTCOME_RETENTION {
            st.done.pop_front();
        }
        st.done.push_back((seq, outcome));
        drop(st);
        self.resolved.notify_all();
    }

    /// Mark the worker dead and wake every waiter — each unresolved ticket now answers `terminal`
    /// immediately. `terminal` is what is true of a command that is STILL QUEUED at this moment:
    /// [`CommandOutcome::NeverSent`] in every case the worker can see (it stopped taking commands,
    /// so a queued one cannot have been written), and only ever `Disconnected` if a future exit
    /// path cannot say that much.
    fn finish(&self, terminal: CommandOutcome) {
        let mut st = self.state.lock().expect("outcome board poisoned");
        st.terminal = Some(terminal);
        drop(st);
        self.resolved.notify_all();
    }

    /// Block up to `timeout` for `seq`'s outcome. A RESOLVED ticket is checked before the
    /// worker-finished flag, so a command answered just as the connection closed still reports the
    /// node's real verdict rather than `Disconnected`.
    fn wait(&self, seq: u64, timeout: Duration) -> Option<CommandOutcome> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock().expect("outcome board poisoned");
        loop {
            if let Some((_, outcome)) = st.done.iter().find(|(s, _)| *s == seq) {
                return Some(outcome.clone());
            }
            if let Some(terminal) = &st.terminal {
                return Some(terminal.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            st = self.resolved.wait_timeout(st, left).expect("outcome board poisoned").0;
        }
    }
}

/// Why a remote command was not enqueued — the CRATE-LOCAL twin of `vike_core::CommandRejected` (this
/// crate never depends on vike-core). The first two arms mirror that type exactly so a GUI can treat
/// a remote and an in-process command lane identically; the third is REMOTE-ONLY by nature (an
/// in-process lane has no capability negotiation to fail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRejected {
    /// The bounded outbound queue is full — the worker has not drained the prior command(s) yet.
    /// Retry after a repaint (the twin of `CommandRejected::Busy`).
    Busy,
    /// The worker thread has exited (transport fault or local shutdown) — the channel is closed and
    /// no further command can be sent (the twin of `CommandRejected::Gone`).
    Gone,
    /// The connected node's `Welcome.features` does not advertise the capability this command
    /// requires (today: [`crate::proto::FEATURE_STRATEGY_VERBS`] for `WireCommand::UpdateParams`) —
    /// REFUSED CLIENT-SIDE, nothing was enqueued and nothing went on the wire. An older server
    /// cannot decode the variant at all (it would answer "undecodable request"), so refusing here
    /// is what makes the failure actionable: upgrade the node, or don't send strategy verbs to it.
    /// Not transient — retrying against the same node cannot succeed.
    UnsupportedByNode,
}

/// A connected, authenticated, WRITE-only control handle to a remote headless node.
///
/// Construct with [`RemoteControlHandle::connect`]. It owns a background worker thread that owns the
/// socket and drains an internal command queue; [`try_command`](Self::try_command) enqueues without
/// ever blocking. Drop it to tear down the connection and join the worker.
pub struct RemoteControlHandle {
    /// The UI → worker command queue (bounded, non-blocking `try_send`).
    cmd_tx: SyncSender<Outbound>,
    /// The server's advertised `Welcome.features`, captured at connect — what
    /// [`Self::try_command_with_reason`] checks a feature-gated command (`UpdateParams`) against,
    /// so an unadvertised verb is refused CLIENT-SIDE ([`ControlRejected::UnsupportedByNode`])
    /// instead of being sent as a frame the node cannot decode.
    features: Vec<String>,
    /// `false` once the worker thread has exited (transport fault / local shutdown).
    connected: Arc<AtomicBool>,
    /// Mints the next [`CommandTicket`] sequence. Monotonic and client-local; never on the wire.
    next_seq: AtomicU64,
    /// Per-command outcomes, keyed by ticket — the surface [`Self::await_outcome`] reads. This is
    /// what makes a reported outcome belong to the command that caused it.
    outcomes: Arc<OutcomeBoard>,
    /// The LATCHING "most recent server error" for a status strip (`vike-app`'s control segment),
    /// cleared only by [`Self::clear_last_error`]. ⚠ NOT a per-command answer — see the module doc.
    last_error: Arc<Mutex<Option<String>>>,
    /// A second handle to the same socket, kept ONLY so [`Drop`] can `shutdown` it and unblock the
    /// worker if it is parked in a socket read.
    shutdown_stream: TcpStream,
    /// The worker thread's join handle, taken and joined in [`Drop`].
    worker: Option<JoinHandle<()>>,
}

impl RemoteControlHandle {
    /// Connect to a node's control server at `addr`, run the full handshake under [`Scope::Control`]
    /// using `control_key` (the raw bytes of `VIKE_TRADEHUB_CONTROL_KEY`), and spawn the worker.
    ///
    /// The handshake is the shared `crate::handshake` sequence run under [`Scope::Control`] —
    /// exactly what [`crate::remote_handle::RemoteCoreHandle::connect`] runs THROUGH auth, but it
    /// SKIPS the `Subscribe` (step 3): a control connection stays in the request/response loop and
    /// never becomes a one-way push pipe. Send [`Request::Hello`], read the [`Response::Welcome`]
    /// challenge nonce (failing — naming both versions — on a protocol-version mismatch), sign it
    /// under `Control`, send [`Request::Auth`], and require [`Response::AuthOk`]. A
    /// [`Response::AuthDenied`] (wrong/absent control key, or control disabled on the node) surfaces as
    /// [`io::ErrorKind::PermissionDenied`] carrying the server's reason.
    pub fn connect<A: ToSocketAddrs>(addr: A, control_key: &[u8]) -> io::Result<Self> {
        Self::connect_with_reply_timeout(addr, control_key, CONTROL_REPLY_TIMEOUT)
    }

    /// [`Self::connect`] with the worker's reply deadline as a PARAMETER — the seam the
    /// silent-death tests below drive, so "a link that dies without a FIN ends the worker instead
    /// of wedging it" is proven in milliseconds rather than [`CONTROL_REPLY_TIMEOUT`]'s 30 s.
    /// Crate-private and single-caller in production ([`Self::connect`], which passes the
    /// constant): the window is a protocol fact, not something a caller may choose. It is the exact
    /// shape [`crate::remote_handle::RemoteCoreHandle::connect_with_read_timeout`] already uses for
    /// the read half, deliberately, so the two halves of one connection pair read the same.
    ///
    /// ⚠ Unlike the observe half's deadline this one is NOT feature-negotiated, and the asymmetry
    /// is not an oversight. The observe deadline is armed against a node that PROMISED to fill the
    /// silence, because an idle node with nothing to publish is the ordinary state and deadlining it
    /// would tear healthy streams down. This deadline is armed against a reply the client has just
    /// ASKED FOR: silence here is never ordinary, whatever the node's vintage, so there is no
    /// capability to negotiate and every node — including one that predates the heartbeat entirely —
    /// gets it.
    pub(crate) fn connect_with_reply_timeout<A: ToSocketAddrs>(
        addr: A,
        control_key: &[u8],
        reply_timeout: Duration,
    ) -> io::Result<Self> {
        let (stream, features) = node_handshake(addr, control_key, Scope::Control)?;

        // THE SILENT-DEATH DEADLINE for the WRITE half (see [`CONTROL_REPLY_TIMEOUT`]). The
        // handshake's own bound is cleared by `node_handshake` on the way out, and until this line
        // existed nothing replaced it: the worker's reply read below blocked with no deadline, and
        // a link that died without a FIN or an RST parked it for the life of the process — with
        // `connected` still `true`, every later command queued behind it, and `vike-cli mcp`
        // answering each of them "sent, outcome unknown" for commands that never left this
        // process. Armed once here rather than around each read: the worker only ever reads a
        // reply it just asked for, `link_is_dead`'s probe restores BLOCKING mode and not the
        // timeout (they are independent socket options), and a deadline that must be re-armed each
        // pass is a deadline somebody eventually forgets.
        stream.set_read_timeout(Some(reply_timeout))?;

        // NO Subscribe — control stays in the request/response loop. Spawn the worker over the
        // authed stream; keep a clone for a Drop-time shutdown.
        let connected = Arc::new(AtomicBool::new(true));
        let last_error = Arc::new(Mutex::new(None));
        let outcomes = Arc::new(OutcomeBoard::default());
        let shutdown_stream = stream.try_clone()?;
        let (cmd_tx, cmd_rx) = sync_channel::<Outbound>(CONTROL_QUEUE_CAP);

        let worker = {
            let connected = Arc::clone(&connected);
            let last_error = Arc::clone(&last_error);
            let outcomes = Arc::clone(&outcomes);
            let mut io_stream = stream;
            std::thread::Builder::new()
                .name("vt-remote-control".into())
                .spawn(move || {
                    // Drain the internal queue: write each command, read ITS reply, file the
                    // verdict under that command's own ticket. The loop is strictly serial and the
                    // server writes exactly one frame per request, so the reply read here always
                    // belongs to the command just written — that is the whole correlation. A
                    // channel disconnect (the handle dropped) ends the loop cleanly; a transport
                    // error (write or read) ends it and flips `connected` false, and `finish()`
                    // then resolves every unanswered ticket to `Disconnected` rather than leaving
                    // its caller to time out. A refusal is recorded, never fatal.
                    // ⚠ THE FATAL VERDICT IS HELD, NOT PUBLISHED HERE — and that is the whole of the
                    // ordering fix. Every exit below used to `outcomes.record(seq, …)` and THEN
                    // `break`, so the record's `notify_all` released a waiter while `connected` was
                    // still `true`: the store happens after the loop. An observer that was already
                    // awake — polling rather than parked, which is what a GUI supervisor loop is —
                    // could read the terminal verdict and then read a LIVE flag, in that order.
                    //
                    // The two answers contradict each other and the second is the one a caller acts
                    // on: `NeverSent` means "the link was dead, nothing was written, resend is
                    // safe", and `is_connected() == true` immediately after says the link is fine.
                    // MEASURED as a CI flake first (one failure in 10,981 tests, on commits that
                    // could not reach this crate) and then deterministically:
                    // `a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected`
                    // fails on round 0 in 0.39 s once the observer spins instead of parking.
                    //
                    // Holding it makes the invariant STRUCTURAL rather than lucky: nothing that ends
                    // this worker can be observed before the flag says the worker is ending. The
                    // happy-path `record` stays inside the loop — it does not end the worker, and
                    // its outcome must be visible at once.
                    let mut fatal: Option<(u64, CommandOutcome)> = None;
                    while let Ok(Outbound { seq, cmd, reason }) = cmd_rx.recv() {
                        // (0) IS THE LINK STILL THERE? The worker parks in the command channel, not
                        // in a socket read, so a close that arrived while it was idle is unread
                        // until now. Ask the socket before writing: a peer FIN is already sitting
                        // in the receive queue and a non-blocking peek costs nothing to find it.
                        // Without this, a command offered to a link the node closed CLEANLY was
                        // written into a dead socket and reported `Disconnected` — "may have
                        // executed" — for a command the node had stopped reading before it existed.
                        if link_is_dead(&io_stream) {
                            fatal = Some((seq, CommandOutcome::NeverSent));
                            break;
                        }
                        if write_frame(&mut io_stream, &Request::Command { cmd, reason }).is_err() {
                            // ⚠ UNKNOWN, not never-sent. `write_all` reports failure without
                            // reporting progress, so a frame half-written before the fault is
                            // indistinguishable here from one that never left — and half a frame
                            // the node then completes is an executed command. The probe above is
                            // the only place that may say never-sent, because it is the only place
                            // that knows nothing was attempted.
                            fatal = Some((seq, CommandOutcome::Disconnected));
                            break;
                        }
                        let outcome = match read_frame::<_, Response>(&mut io_stream) {
                            Ok(Response::Ack { coid }) => CommandOutcome::Accepted { coid },
                            // An accepted `SetSetting` (REQ-7): the empty coid is the
                            // account-wide-verb convention. This fire-and-forget path DROPS the
                            // reply's `restart_required` — a caller that must surface it uses the
                            // synchronous [`set_setting`] instead.
                            Ok(Response::SettingsWritten { .. }) => {
                                CommandOutcome::Accepted { coid: String::new() }
                            }
                            Ok(Response::Error(msg)) => CommandOutcome::Refused(msg),
                            Ok(other) => CommandOutcome::Refused(format!(
                                "unexpected reply to a command: {}",
                                resp_kind(&other)
                            )),
                            // Written, and the reply never came: genuinely UNKNOWN for THIS
                            // command, and recorded under its own ticket so the terminal fallback
                            // below (never-sent, for whatever is still queued behind it) cannot
                            // answer for it.
                            //
                            // ⚠ This arm is now reached by SILENCE as well as by a reported fault,
                            // and that is the whole of the silent-death fix on this half: the
                            // `reply_timeout` armed at connect expires, `read_frame` returns an
                            // error, and this existing arm ends the worker. Nothing new decides
                            // anything — the read merely stops blocking forever. `Disconnected`
                            // stays the verdict either way: the frame WAS written, so the module
                            // doc's asymmetry (a wrong `Disconnected` costs a `node_snapshot`; a
                            // wrong `NeverSent` places an order twice) forbids the other answer.
                            Err(_) => {
                                fatal = Some((seq, CommandOutcome::Disconnected));
                                break;
                            }
                        };
                        // The status-strip latch keeps its historical semantics: set on a refusal,
                        // never cleared here. It is the GUI's view, NOT this command's answer.
                        if let CommandOutcome::Refused(msg) = &outcome {
                            *last_error.lock().expect("last_error poisoned") = Some(msg.clone());
                        }
                        outcomes.record(seq, outcome);
                    }
                    // ⚠ THE STORE COMES FIRST, AND EVERYTHING BELOW IS A PUBLISH. Both lines after
                    // it wake waiters (`record` and `finish` each `notify_all`), so this ordering is
                    // what makes "a terminal verdict implies a dead handle" true by construction
                    // rather than by scheduling luck. It is a `Release` store and `is_connected`
                    // loads `Acquire`; the board's mutex, taken by both publishes below and by every
                    // waiter, is what carries the edge to a reader that never touches the flag until
                    // after it has the verdict.
                    connected.store(false, Ordering::Release);
                    // The verdict for the ONE command that ended the worker, held back by each break
                    // above precisely so it lands on this side of the store. `None` is the ordinary
                    // exit — the handle was dropped and the channel disconnected, so no command was
                    // in hand.
                    if let Some((seq, outcome)) = fatal {
                        outcomes.record(seq, outcome);
                    }
                    // The terminal answer for anything STILL QUEUED, and it is never-sent on every
                    // exit path by CONSTRUCTION: this loop writes only a command it has already
                    // taken out of the queue, and each of the three breaks above files that one
                    // command's own verdict under its own ticket first. So whatever is still
                    // waiting behind it was, provably, never written — including on the ordinary
                    // exit (the handle dropped, the channel disconnected).
                    outcomes.finish(CommandOutcome::NeverSent);
                })
                .expect("spawn vt-remote-control thread")
        };

        Ok(RemoteControlHandle {
            cmd_tx,
            features,
            connected,
            next_seq: AtomicU64::new(0),
            outcomes,
            last_error,
            shutdown_stream,
            worker: Some(worker),
        })
    }

    /// Enqueue one command for the worker to send — NON-BLOCKING (`SyncSender::try_send`), the wire
    /// twin of `CoreHandle::try_command`. A full queue is [`ControlRejected::Busy`]; a closed queue
    /// (the worker exited) is [`ControlRejected::Gone`]. Returns THIS command's [`CommandTicket`];
    /// redeem it at [`Self::await_outcome`] to learn what the node did with it, or drop it to keep
    /// the historical fire-and-forget behaviour.
    ///
    /// Exactly [`Self::try_command_with_reason`] with no rationale — every existing caller (notably
    /// the `vike-app --observe` GUI's order buttons, which `let _ =` the result) is unaffected by
    /// the ticket: they send byte-identical `reason: None` commands and ignore the answer.
    pub fn try_command(&self, cmd: WireCommand) -> Result<CommandTicket, ControlRejected> {
        self.try_command_with_reason(cmd, None)
    }

    /// [`Self::try_command`] plus an OPTIONAL operator/agent RATIONALE (v4) — *why* this command was
    /// issued. The rationale rides BESIDE the command: the node records it (sanitized: control
    /// characters stripped, length capped) in its audit trail and lowers only the command, so it
    /// never reaches `OrderRequest`, the core fold, the journal, or any venue. Same non-blocking
    /// contract and same rejection arms as [`Self::try_command`].
    ///
    /// The ticket sequence is minted here and travels IN the queued payload, so it is bound to this
    /// exact command even if another thread's `try_send` wins the race to the channel.
    ///
    /// A FEATURE-GATED command (`UpdateParams`, requiring
    /// [`crate::proto::FEATURE_STRATEGY_VERBS`]) is refused with
    /// [`ControlRejected::UnsupportedByNode`] — before the queue, so nothing is enqueued and
    /// nothing goes on the wire — when the connected node's `Welcome.features` (captured at
    /// [`Self::connect`]) does not advertise it.
    pub fn try_command_with_reason(
        &self,
        cmd: WireCommand,
        reason: Option<String>,
    ) -> Result<CommandTicket, ControlRejected> {
        if let Some(feature) = required_feature(&cmd)
            && !self.features.iter().any(|f| f == feature)
        {
            return Err(ControlRejected::UnsupportedByNode);
        }
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.cmd_tx.try_send(Outbound { seq, cmd, reason }).map_err(|e| match e {
            TrySendError::Full(_) => ControlRejected::Busy,
            TrySendError::Disconnected(_) => ControlRejected::Gone,
        })?;
        Ok(CommandTicket(seq))
    }

    /// Wait up to `timeout` for what the node did with the ONE command `ticket` names — the
    /// per-command outcome surface, and the only correct way to REPORT a command's result.
    ///
    /// Returns as soon as that command's reply lands (a condvar, not a fixed settle sleep), so the
    /// common localhost case answers in well under a millisecond. `None` means the outcome is not
    /// known within `timeout`: the command was sent and the node has not answered it yet — NOT that
    /// it failed, and never that it succeeded. (`None` is also what a ticket older than the board's
    /// [`OUTCOME_RETENTION`] window resolves to, which a caller awaiting the ticket it just minted
    /// cannot reach.)
    pub fn await_outcome(
        &self,
        ticket: CommandTicket,
        timeout: Duration,
    ) -> Option<CommandOutcome> {
        self.outcomes.wait(ticket.0, timeout)
    }

    /// `true` while the worker thread is alive (the control connection is usable). Flips to `false`
    /// once the node closes the stream or a transport fault ends the worker — a supervisor probe for
    /// the GUI, mirroring `CoreHandle::is_alive`.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// The LATCHED most-recent server error (a refused command, control-not-enabled, or an
    /// unexpected reply), or `None` if nothing has been refused since the last
    /// [`Self::clear_last_error`]. This is a STATUS-STRIP view — `vike-app`'s control segment paints
    /// it, and it deliberately persists so a refusal is not lost between repaints.
    ///
    /// ⚠ **It is not this-command's answer, and must never be read as one.** It names no command:
    /// once anything is refused it stays set, so polling it after a write reports that refusal for
    /// every later command too, including ones the node executed. Use [`Self::await_outcome`] to
    /// report an outcome; use this only to show "something was refused, here is the last one".
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("last_error poisoned").clone()
    }

    /// Dismiss the latched [`Self::last_error`] — the explicit clear a status strip needs so a
    /// transient refusal does not sit on the bar for the rest of the session. Nothing clears the
    /// latch implicitly (a later successful command does not, and neither does
    /// [`Self::await_outcome`]): the owner of the banner decides when it has been seen.
    pub fn clear_last_error(&self) {
        *self.last_error.lock().expect("last_error poisoned") = None;
    }
}

impl Drop for RemoteControlHandle {
    fn drop(&mut self) {
        // The worker parks in EITHER `cmd_rx.recv()` (idle) OR a socket read (awaiting an Ack), so
        // close BOTH wake paths or the join could hang: (1) drop the real command sender — replacing
        // it with a throwaway — so an idle `recv()` returns a channel disconnect; (2) shut the socket
        // so a mid-flight `read_frame` errors out. Then join, so no thread outlives the handle.
        let (dead_tx, _dead_rx) = sync_channel::<Outbound>(0);
        drop(std::mem::replace(&mut self.cmd_tx, dead_tx));
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(join) = self.worker.take() {
            let _ = join.join();
        }
    }
}

/// DRY-RUN one command against a node's CONTROL gate — the SYNCHRONOUS request/response twin of
/// [`RemoteControlHandle::try_command`]. Opens a fresh short-lived `Control` connection (the same
/// handshake [`RemoteControlHandle::connect`] runs), sends [`Request::Preview`], reads the
/// [`Response::Preview`] verdict, and drops the connection. NOTHING is executed on the node: no
/// order is placed and no state changes — the server answers only what its edge gate WOULD decide.
///
/// A per-call connection is correct here (unlike a fire-and-forget command, which needs the
/// persistent worker): a preview is a single request/response with no ordering concern. Returns
/// `(accepted, reason)` — `accepted == true` ⇒ the command would pass (`reason` is `None`);
/// `accepted == false` ⇒ it would be refused, `reason` carrying the cause. A handshake failure
/// (wrong/absent control key, version skew, transport fault) surfaces as the [`io::Error`].
pub fn preview_command<A: ToSocketAddrs>(
    addr: A,
    control_key: &[u8],
    cmd: &WireCommand,
) -> io::Result<(bool, Option<String>)> {
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Control)?;
    // The same client-side feature gate as `try_command_with_reason`: previewing a verb the node
    // cannot DECODE would answer a generic "undecodable request" Error, not a gate verdict.
    if let Some(feature) = required_feature(cmd)
        && !features.iter().any(|f| f == feature)
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{feature}\" capability (an older \
                     vike-tradehub) — preview refused client-side, nothing was sent"
            ),
        ));
    }
    write_frame(&mut stream, &Request::Preview(cmd.clone()))?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::Preview { accepted, reason } => Ok((accepted, reason)),
        Response::AuthDenied { reason } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("tradehub preview denied: {reason}"),
        )),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub preview error: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub preview: expected Preview, got {}", resp_kind(&other)),
        )),
    }
}

/// Write ONE settings key on a node — the REQ-7 write verb, [`preview_command`]'s exact per-call
/// shape: open a fresh short-lived [`Scope::Control`] connection (the same handshake
/// [`RemoteControlHandle::connect`] runs), send the `WireCommand::SetSetting` as a
/// [`Request::Command`] (with the optional audit `reason` beside it), read the reply, and drop
/// the connection. SYNCHRONOUS on purpose: unlike an order, a settings write's caller always
/// needs the verdict — above all the reply's `restart_required`, which the fire-and-forget
/// [`RemoteControlHandle`] path cannot surface (its worker maps the reply to an empty-coid
/// acceptance).
///
/// `file` names one of the node's four settings files (`"policy.toml"` / `"config.toml"` /
/// `"preferences.toml"` / `"flags.toml"`; the bare stem is accepted); `key` is the FULL dotted
/// key exactly as the read half renders it; `value` is the new value as text; `confirm` is the
/// TYPED-CONFIRM for a `policy.toml` write — it must equal `key` exactly or the node refuses (see
/// `WireCommand::SetSetting`'s doc for the whole contract). The node validates the would-be file
/// with its own loader before writing, so a refusal echoes the loader's message.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: the request is sent ONLY when the server's
/// `Welcome.features` advertises [`FEATURE_SETTINGS_WRITE`]; against an older node this fails
/// with [`io::ErrorKind::Unsupported`] naming the missing capability and NOTHING goes on the wire
/// after the handshake. Returns `restart_required` on acceptance: `false` when the node HOT-
/// APPLIED the value (its own per-key classification decides; policy keys never are), `true` when
/// the write landed on disk and the backend must be restarted to apply it. A server-side refusal
/// surfaces as [`io::ErrorKind::InvalidData`] carrying the node's own text; a handshake failure as its usual
/// [`io::Error`].
pub fn set_setting<A: ToSocketAddrs>(
    addr: A,
    control_key: &[u8],
    file: &str,
    key: &str,
    value: &str,
    confirm: Option<&str>,
    reason: Option<&str>,
) -> io::Result<bool> {
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Control)?;
    if !features.iter().any(|f| f == FEATURE_SETTINGS_WRITE) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{FEATURE_SETTINGS_WRITE}\" capability (an \
                 older vike-tradehub) — SetSetting refused client-side, nothing was sent"
            ),
        ));
    }
    let cmd = WireCommand::SetSetting {
        file: file.to_string(),
        key: key.to_string(),
        value: value.to_string(),
        confirm: confirm.map(str::to_string),
    };
    write_frame(&mut stream, &Request::Command { cmd, reason: reason.map(str::to_string) })?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SettingsWritten { restart_required } => Ok(restart_required),
        Response::AuthDenied { reason } => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("tradehub settings write denied: {reason}"),
        )),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub settings write refused: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub settings write: expected SettingsWritten, got {}", resp_kind(&other)),
        )),
    }
}

#[cfg(test)]
mod tests {
    //! The [`OutcomeBoard`] in isolation — the piece that makes an outcome belong to ONE command.
    //! The end-to-end property (refuse one command, accept the next, report each correctly against a
    //! REAL node) is `vike-tradehub`'s `a_refusal_is_not_reported_again_for_the_next_accepted_command`;
    //! these pin the board's own arms, which that test can only reach one at a time.

    use super::*;

    const NOW: Duration = Duration::from_millis(0);

    fn accepted(coid: &str) -> CommandOutcome {
        CommandOutcome::Accepted { coid: coid.to_string() }
    }

    /// ⚠ THE DEFECT, at the data-structure level: two commands, two verdicts, and each ticket
    /// resolves to ITS OWN. A shared latch answers both with whichever error landed last.
    #[test]
    fn each_ticket_resolves_to_its_own_verdict() {
        let board = OutcomeBoard::default();
        board.record(0, CommandOutcome::Refused("too big".into()));
        board.record(1, accepted("c-2"));
        assert_eq!(board.wait(0, NOW), Some(CommandOutcome::Refused("too big".into())));
        assert_eq!(
            board.wait(1, NOW),
            Some(accepted("c-2")),
            "the SECOND command was accepted; reporting the first's refusal here is the defect"
        );
        // …and re-reading does not consume: a ticket is idempotent, unlike a taking accessor.
        assert_eq!(board.wait(0, NOW), Some(CommandOutcome::Refused("too big".into())));
    }

    /// An unresolved ticket is `None` — "not answered yet", which is neither success nor failure.
    #[test]
    fn an_unanswered_ticket_times_out_as_none() {
        let board = OutcomeBoard::default();
        board.record(0, accepted("c-1"));
        assert_eq!(board.wait(1, Duration::from_millis(20)), None);
    }

    /// A waiter parked on the condvar is woken by the worker's `record`, not by polling — the
    /// reason the caller no longer pays a fixed settle sleep.
    #[test]
    fn a_waiter_is_woken_when_its_outcome_lands() {
        let board = Arc::new(OutcomeBoard::default());
        let writer = Arc::clone(&board);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            writer.record(7, accepted("late"));
        });
        let started = Instant::now();
        assert_eq!(board.wait(7, Duration::from_secs(5)), Some(accepted("late")));
        assert!(started.elapsed() < Duration::from_secs(4), "woken by the record, not the timeout");
    }

    /// A dead worker resolves every UNANSWERED ticket at once — a waiter must fail fast rather
    /// than burn its whole timeout on a connection that is gone. The terminal answer is the
    /// worker's own verdict about what is STILL QUEUED, and the real loop always has one to give
    /// ([`CommandOutcome::NeverSent`]: a queued command is by construction one the worker never
    /// wrote). The `Disconnected` case is a command the worker had already SENT, and it is filed
    /// under its own ticket before finishing — the case below.
    #[test]
    fn a_dead_worker_resolves_pending_tickets_to_the_terminal_outcome() {
        let never = OutcomeBoard::default();
        never.finish(CommandOutcome::NeverSent);
        assert_eq!(never.wait(0, NOW), Some(CommandOutcome::NeverSent));

        let unknown = OutcomeBoard::default();
        unknown.finish(CommandOutcome::Disconnected);
        assert_eq!(unknown.wait(0, NOW), Some(CommandOutcome::Disconnected));
    }

    /// ⚠ The IN-FLIGHT command's own verdict beats the terminal one. The worker files a
    /// `Disconnected` for the command it wrote and never heard back about, THEN finishes with
    /// never-sent for whatever is still queued — so the two never blur into one answer, which is
    /// the whole reason the terminal carries an outcome instead of being a flag.
    #[test]
    fn an_in_flight_commands_own_verdict_beats_the_terminal_one() {
        let board = OutcomeBoard::default();
        board.record(5, CommandOutcome::Disconnected);
        board.finish(CommandOutcome::NeverSent);
        assert_eq!(
            board.wait(5, NOW),
            Some(CommandOutcome::Disconnected),
            "the command that WAS written keeps its unknown outcome — resending it is how one \
             order becomes two"
        );
        assert_eq!(
            board.wait(6, NOW),
            Some(CommandOutcome::NeverSent),
            "…while the one queued behind it never went at all"
        );
    }

    /// …but a ticket ALREADY answered keeps the node's real verdict, even though the worker has
    /// since exited (the ordinary shape: the last command is acked, then the handle is dropped).
    #[test]
    fn a_resolved_ticket_survives_the_worker_exiting() {
        let board = OutcomeBoard::default();
        board.record(3, accepted("c-3"));
        board.finish(CommandOutcome::NeverSent);
        assert_eq!(board.wait(3, NOW), Some(accepted("c-3")));
        assert_eq!(board.wait(4, NOW), Some(CommandOutcome::NeverSent), "…only unanswered ones");
    }

    /// The board is BOUNDED: a fire-and-forget caller that never awaits cannot grow it without
    /// limit. The newest [`OUTCOME_RETENTION`] answers are kept; older ones fall out.
    #[test]
    fn the_board_retains_a_bounded_window_of_outcomes() {
        let board = OutcomeBoard::default();
        for seq in 0..(OUTCOME_RETENTION as u64 + 5) {
            board.record(seq, accepted(&format!("c-{seq}")));
        }
        assert_eq!(board.state.lock().expect("board").done.len(), OUTCOME_RETENTION);
        assert_eq!(board.wait(4, NOW), None, "an evicted ticket is simply unanswerable");
        let newest = OUTCOME_RETENTION as u64 + 4;
        assert_eq!(board.wait(newest, NOW), Some(accepted(&format!("c-{newest}"))));
    }
}

/// NEVER-SENT vs UNKNOWN, against a scripted node — the two ways a control command can end without
/// a verdict, and the evidence that separates them.
///
/// The distinction is not cosmetic: `vike-cli mcp` reconnects and re-sends on one of these and
/// refuses to on the other, so a test that could not tell them apart would be the whole safety
/// argument taken on trust. The node here is scripted rather than real because the two cases differ
/// only in WHEN the peer closes — before the command is offered, or after it has been read — and
/// that is a script, not a configuration.
#[cfg(test)]
mod never_sent_tests {
    use super::*;
    use std::net::{SocketAddr, TcpListener};
    use std::thread;

    use crate::auth;
    use crate::proto::{NODE_PROTO_VERSION, read_frame, write_frame};
    use crate::wire::WireOrderRequest;

    const KEY: &[u8] = b"control-key-for-the-never-sent-tests";
    const WAIT: Duration = Duration::from_secs(5);
    /// The scripted reply deadline the silent-death test drives through
    /// [`RemoteControlHandle::connect_with_reply_timeout`]. Production is
    /// [`CONTROL_REPLY_TIMEOUT`] (30 s); the property is the same at any scale.
    const TEST_REPLY_TIMEOUT: Duration = Duration::from_millis(300);
    /// How long a silent scripted node holds its socket open saying nothing — far past every
    /// assertion, so the test can never be passing because the node finally hung up.
    const SILENT_HOLD: Duration = Duration::from_secs(30);
    /// How long to let a loopback FIN arrive before offering the command the whole never-sent story
    /// turns on. Generous by three orders of magnitude — a `close()` on `127.0.0.1` is delivered in
    /// microseconds — because a test that raced the FIN would be testing the wrong thing entirely:
    /// the point is a link the worker finds ALREADY dead, not one dying under it.
    const FIN_SETTLE: Duration = Duration::from_millis(100);
    /// How many times
    /// [`a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected`] replays the
    /// scenario. Sized by what it costs rather than by a confidence target: each round is one
    /// loopback connection plus [`FIN_SETTLE`], so this is a few seconds — and the rounds are
    /// evidence only on the BROKEN ordering. Once the store precedes the publish, one round proves
    /// as much as a thousand, and these exist to keep the ordering from silently coming back.
    const ORDERING_ROUNDS: usize = 30;

    /// When the scripted node hangs up.
    #[derive(Clone, Copy)]
    enum Script {
        /// Close CLEANLY straight after `AuthOk`, having read no command — the node's own idle
        /// close, a daemon restart, a tunnel dropping one link. The client's next command meets a
        /// socket with a FIN already in its receive queue.
        CloseAfterAuth,
        /// Read ONE command and then close WITHOUT answering — the command reached the node and
        /// its verdict was lost on the way back. Whether it executed is genuinely unknowable from
        /// here, which is the point.
        ReadOneCommandThenClose,
        /// Read ONE command and then go SILENT while HOLDING the socket open — no answer, and no
        /// FIN or RST either. The silent death: a laptop sleeping, a Wi-Fi change, a VPN re-key
        /// under an `ssh -L` whose forwarded socket stays open locally. Distinct from
        /// [`Script::ReadOneCommandThenClose`] in exactly the way that matters — that one is a
        /// close the socket REPORTS, and every path here has always handled it.
        ReadOneCommandThenGoSilent,
    }

    /// A one-connection node speaking the REAL `Control` handshake, then following `script`.
    fn scripted_control_node(script: Script) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let nonce = [23u8; 32];
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Hello { .. }) => {}
                other => panic!("expected Hello, got {other:?}"),
            }
            write_frame(
                &mut stream,
                &Response::Welcome {
                    proto_version: NODE_PROTO_VERSION,
                    nonce,
                    features: vec!["observe".to_string()],
                },
            )
            .expect("welcome");
            let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
                return;
            };
            assert!(
                auth::verify(KEY, &nonce, NODE_PROTO_VERSION, scope, &mac),
                "the client must present a valid control mac"
            );
            write_frame(&mut stream, &Response::AuthOk { scope }).expect("authok");
            if let Script::ReadOneCommandThenClose | Script::ReadOneCommandThenGoSilent = script {
                match read_frame::<_, Request>(&mut stream) {
                    Ok(Request::Command { .. }) => {}
                    other => panic!("expected the one Command, got {other:?}"),
                }
            }
            if let Script::ReadOneCommandThenGoSilent = script {
                // …and NOTHING, with the socket still open. No reply, no FIN, no RST — the only
                // failure a `link_is_dead` peek can never see, and the one this node scripts.
                // Parked long past every assertion; the process exits before it does.
                thread::sleep(SILENT_HOLD);
                return;
            }
            // Drop the socket: a clean close, the same FIN a node's own `return` sends.
            drop(stream);
        });
        addr
    }

    fn a_resting_limit() -> WireCommand {
        WireCommand::Submit(WireOrderRequest {
            client_order_id: "never-sent-test-1".to_string(),
            venue: "polymarket".to_string(),
            symbol: "NEVER_SENT_TOKEN".to_string(),
            side: 1,
            qty: 20.0,
            order_type: "limit".to_string(),
            price: Some(0.40),
            trigger_price: None,
            reduce_only: false,
        })
    }

    /// **THE DEFECT.** The node closed the link before this command existed, so nothing was
    /// written — and saying UNKNOWN ("it may have executed") about a command the node never read
    /// is a false statement that costs the caller a round trip and forbids it from simply sending.
    ///
    /// The trigger this was written for was the node's five-minute idle close of an AUTHED control
    /// connection; that is fixed at the node (`crate::liveness::AUTHED_IDLE_TIMEOUT`), and a clean
    /// close still happens on a restart or a tunnel blip, which is what this scripts.
    #[test]
    fn a_never_sent_write_is_reported_as_never_sent_not_unknown() {
        let addr = scripted_control_node(Script::CloseAfterAuth);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
        // Let the FIN land before the command is offered — the whole point is that the close
        // arrived while the worker was parked in its command channel, unread.
        thread::sleep(Duration::from_millis(100));

        let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
        assert_eq!(
            handle.await_outcome(ticket, WAIT),
            Some(CommandOutcome::NeverSent),
            "the link was already closed, so not one byte went — reporting UNKNOWN here is the \
             defect: it forbids the caller from simply sending the command"
        );
        assert!(!handle.is_connected(), "…and the worker is done, so the handle is dead");
    }

    /// **THE ORDERING that assertion depends on, asserted on its own and many times over.**
    ///
    /// `a_never_sent_write_is_reported_as_never_sent_not_unknown` above ends by reading
    /// [`RemoteControlHandle::is_connected`] straight after `await_outcome` answered. That is not
    /// two assertions about one fact, it is one assertion about a HAPPENS-BEFORE: the worker must
    /// not publish a terminal verdict while it is still advertising a live link.
    ///
    /// It flaked on the shared CI runners — one failure in 10,981 tests, on commits that could not
    /// reach this crate — and the loop below is what turns "it flaked" into a measurement. Each
    /// round is the same script: connect, let the FIN land, offer one command, wait for the verdict,
    /// and read the flag in the same breath.
    ///
    /// ⚠ **What this test is worth is different before and after the fix, and saying so is the
    /// point.** Before, it is PROBABILISTIC: the window is the handful of instructions between
    /// `record`'s `notify_all` and the `connected.store` after the loop, so a round only fails if
    /// the worker is descheduled inside it — which is why the original flaked on a loaded box and
    /// never on a quiet one. After, it cannot fail at all: the verdict is published AFTER the store,
    /// so there is no window left to lose. A green run here is therefore weak evidence on the old
    /// code and a structural guarantee on the new — and the reason the fix is an ORDERING change
    /// rather than a poll-until-true in the test above.
    #[test]
    fn a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected() {
        for round in 0..ORDERING_ROUNDS {
            let addr = scripted_control_node(Script::CloseAfterAuth);
            let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
            thread::sleep(FIN_SETTLE);

            let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
            // ⚠ SPIN, do not block. `await_outcome(_, WAIT)` parks on the board's condvar, and a
            // parked observer is the WRONG instrument for this race: the worker calls `notify_all`
            // and then keeps its core through `break` and the store, so the sleeper is woken into a
            // world where the flag has already flipped. Pinning to one CPU makes it worse still —
            // the waker never yields. Polling with a ZERO timeout makes the observer already
            // runnable on another core: it needs only the board's mutex, which the worker drops
            // BEFORE it notifies, so it can read the verdict while the worker is still unwinding.
            // Measured: 30 blocking rounds pass on a quiet lane and pinned to one CPU; the spin is
            // what turns the window into something a test can stand on.
            let deadline = Instant::now() + WAIT;
            let outcome = loop {
                if let Some(o) = handle.await_outcome(ticket, Duration::ZERO) {
                    break Some(o);
                }
                assert!(Instant::now() < deadline, "round {round}: no verdict within {WAIT:?}");
                std::hint::spin_loop();
            };
            assert_eq!(outcome, Some(CommandOutcome::NeverSent), "round {round}");
            // ⚠ NO retry, NO poll, NO sleep between the two reads. A poll here would make this test
            // pass on the broken ordering and measure nothing — the defect IS that the two answers
            // can disagree for an instant, and an instant is all a caller needs to act on.
            assert!(
                !handle.is_connected(),
                "round {round}: the worker published `{outcome:?}` — a terminal verdict about a \
                 link it had already found dead — while `is_connected()` still said the link was \
                 up. A caller doing the obvious thing (await the outcome, then ask whether to \
                 reconnect) gets those two answers in that order and they contradict each other. \
                 The store must precede the publish, not follow it."
            );
        }
    }

    /// A command still QUEUED when the worker stops is never-sent too — same evidence (the worker
    /// never took it out of the queue), reached through the board's terminal answer rather than a
    /// per-command record.
    #[test]
    fn a_command_queued_behind_a_dead_link_is_never_sent() {
        let addr = scripted_control_node(Script::CloseAfterAuth);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
        thread::sleep(Duration::from_millis(100));

        let first = handle.try_command(a_resting_limit()).expect("the queue is open");
        let second = handle.try_command(a_resting_limit()).expect("the queue is open");
        assert_eq!(handle.await_outcome(first, WAIT), Some(CommandOutcome::NeverSent));
        assert_eq!(
            handle.await_outcome(second, WAIT),
            Some(CommandOutcome::NeverSent),
            "the second never reached the socket at all"
        );
    }

    /// **THE OTHER HALF, and the one that must NOT move.** The node READ this command and the
    /// reply was lost: it may have executed. That is `Disconnected`, and every caller's rule for
    /// it (never resend; go read the book) is unchanged.
    #[test]
    fn a_post_send_loss_is_unknown() {
        let addr = scripted_control_node(Script::ReadOneCommandThenClose);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");

        let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
        assert_eq!(
            handle.await_outcome(ticket, WAIT),
            Some(CommandOutcome::Disconnected),
            "the command was WRITTEN — its outcome is genuinely unknown and it must never be \
             reported as never-sent, which would license a resend"
        );
    }

    /// **THE WRITE HALF'S SILENT DEATH.** The observe half got a deadline and this one did not, so
    /// a control link that died WITHOUT a FIN parked the worker in its reply read for the life of
    /// the process. Everything downstream then lied by omission: `is_connected` stayed `true` (it
    /// is stored `false` only after the loop), so nothing let the handle go and nothing
    /// reconnected; every later command sat in the queue behind the wedge; and `vike-cli mcp`
    /// answered each of them `"sent": true, "outcome": "unknown", …"it may still execute"` for a
    /// command that had provably never left the process — the exact statement this branch exists
    /// to eliminate, made permanent.
    ///
    /// ⚠ Note WHICH command gets which verdict, because the pair is the property: the one that was
    /// WRITTEN is [`CommandOutcome::Disconnected`] (unknown — it may have executed, so nobody may
    /// resend it), and the one still QUEUED behind it is [`CommandOutcome::NeverSent`] (provably
    /// not one byte, so `vike-cli mcp`'s `Server::execute` may reconnect and send it). A test that
    /// asserted one outcome for both would pass over the bug that matters.
    ///
    /// ⚠ KILL PROOF: delete the `set_read_timeout` in
    /// [`RemoteControlHandle::connect_with_reply_timeout`] and this must fail on the FIRST
    /// assertion within `WAIT` — the worker's read never returns, so no ticket ever resolves. Run
    /// both ways; the commit message carries the outcomes.
    #[test]
    fn a_silently_dead_control_link_ends_the_worker_instead_of_wedging_it() {
        let addr = scripted_control_node(Script::ReadOneCommandThenGoSilent);
        let handle = RemoteControlHandle::connect_with_reply_timeout(addr, KEY, TEST_REPLY_TIMEOUT)
            .expect("the scripted node auths");

        let written = handle.try_command(a_resting_limit()).expect("the queue is open");
        let queued = handle.try_command(a_resting_limit()).expect("the queue is open");

        assert_eq!(
            handle.await_outcome(written, WAIT),
            Some(CommandOutcome::Disconnected),
            "the node read this one and answered nothing — silence is not evidence that it did \
             not execute, so UNKNOWN is the only honest verdict, and before the deadline existed \
             this ticket resolved to nothing at all"
        );
        assert_eq!(
            handle.await_outcome(queued, WAIT),
            Some(CommandOutcome::NeverSent),
            "…and the one still in the queue behind it provably never reached the socket, which \
             is what lets the caller reconnect and send THAT one"
        );
        assert!(
            !handle.is_connected(),
            "the worker is done, so the handle reports dead and the next offer is Gone — the \
             wedge is self-healing now instead of permanent"
        );
    }
}
