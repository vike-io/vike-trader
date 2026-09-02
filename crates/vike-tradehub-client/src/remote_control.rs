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
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::handshake::{node_handshake, resp_kind};
use crate::proto::{
    read_frame, write_frame, Request, Response, Scope, FEATURE_MOUNT_VERBS, FEATURE_SETTINGS_WRITE,
    FEATURE_STRATEGY_VERBS,
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
}

/// The worker → caller outcome board: resolved `(ticket, outcome)` pairs plus the worker's
/// end-of-life flag, behind one [`Mutex`] with a [`Condvar`] so a waiter is woken the instant its
/// reply lands instead of sleeping a fixed settle window.
#[derive(Default)]
struct BoardState {
    /// Resolved outcomes, oldest first, bounded to [`OUTCOME_RETENTION`].
    done: VecDeque<(u64, CommandOutcome)>,
    /// Set once the worker has exited: every ticket it never resolved is [`CommandOutcome::Disconnected`]
    /// forever, so a waiter fails fast instead of burning its whole timeout on a dead connection.
    worker_finished: bool,
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

    /// Mark the worker dead and wake every waiter — each unresolved ticket now answers
    /// [`CommandOutcome::Disconnected`] immediately.
    fn finish(&self) {
        let mut st = self.state.lock().expect("outcome board poisoned");
        st.worker_finished = true;
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
            if st.worker_finished {
                return Some(CommandOutcome::Disconnected);
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
        let (stream, features) = node_handshake(addr, control_key, Scope::Control)?;

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
                    while let Ok(Outbound { seq, cmd, reason }) = cmd_rx.recv() {
                        if write_frame(&mut io_stream, &Request::Command { cmd, reason }).is_err() {
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
                            Err(_) => break,
                        };
                        // The status-strip latch keeps its historical semantics: set on a refusal,
                        // never cleared here. It is the GUI's view, NOT this command's answer.
                        if let CommandOutcome::Refused(msg) = &outcome {
                            *last_error.lock().expect("last_error poisoned") = Some(msg.clone());
                        }
                        outcomes.record(seq, outcome);
                    }
                    connected.store(false, Ordering::Release);
                    outcomes.finish();
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
        if let Some(feature) = required_feature(&cmd) {
            if !self.features.iter().any(|f| f == feature) {
                return Err(ControlRejected::UnsupportedByNode);
            }
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
    if let Some(feature) = required_feature(cmd) {
        if !features.iter().any(|f| f == feature) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "this node does not advertise the \"{feature}\" capability (an older \
                     vike-tradehub) — preview refused client-side, nothing was sent"
                ),
            ));
        }
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

    /// A dead worker resolves every UNANSWERED ticket to `Disconnected` at once — the outcome is
    /// genuinely unknown, and a caller must not print "sent" for it.
    #[test]
    fn a_dead_worker_resolves_pending_tickets_to_disconnected() {
        let board = OutcomeBoard::default();
        board.finish();
        assert_eq!(board.wait(0, NOW), Some(CommandOutcome::Disconnected));
    }

    /// …but a ticket ALREADY answered keeps the node's real verdict, even though the worker has
    /// since exited (the ordinary shape: the last command is acked, then the handle is dropped).
    #[test]
    fn a_resolved_ticket_survives_the_worker_exiting() {
        let board = OutcomeBoard::default();
        board.record(3, accepted("c-3"));
        board.finish();
        assert_eq!(board.wait(3, NOW), Some(accepted("c-3")));
        assert_eq!(board.wait(4, NOW), Some(CommandOutcome::Disconnected), "…only unanswered ones");
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
