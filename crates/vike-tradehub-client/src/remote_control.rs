//! `RemoteControlHandle`: the WRITE half of the thin-client node connection, the
//! `try_command`-shaped twin of [`crate::remote_handle::RemoteCoreHandle`].
//!
//! It authenticates under [`Scope::Write`] and ⚠ NEVER subscribes: a subscribed connection is a
//! one-way push pipe, so control stays in the server's request/response loop on its OWN
//! connection. A background worker owns the socket and drains a bounded [`SyncSender`] queue,
//! writing `Request::Command` and reading the `Ack`/`Error` reply. The UI thread never blocks on
//! network I/O: a full queue is [`ControlRejected::Busy`], a dead worker [`ControlRejected::Gone`].
//!
//! # Fire-and-forget
//!
//! [`try_command`](RemoteControlHandle::try_command) returns once the command is enqueued, not on
//! its `Ack`. A transport fault ends the worker and flips
//! [`is_connected`](RemoteControlHandle::is_connected) to `false`.
//!
//! # Two outcome surfaces, and which one a caller must use
//!
//! Enqueueing returns a [`CommandTicket`]; [`await_outcome`](RemoteControlHandle::await_outcome)
//! resolves it to THAT command's [`CommandOutcome`] and no other. **Any caller that REPORTS a
//! command's result must use this surface.** [`last_error`](RemoteControlHandle::last_error) is a
//! LATCHED "most recent refusal" for a status strip (a node's `Response::Error`, or a client
//! refusal latched with [`latch_client_refusal`](RemoteControlHandle::latch_client_refusal)),
//! cleared only by [`clear_last_error`](RemoteControlHandle::clear_last_error). The two are
//! independent: awaiting never clears the latch, clearing the latch never touches a ticket.
//!
//! ⚠ The latch cannot answer "how did MY command go": polled after each write it reports the
//! FIRST refusal for every later command, including ones the node EXECUTED (an operator told a
//! `market-exit` was rejected believes they still hold the position). Gated by `vike-tradehub`'s
//! `a_refusal_is_not_reported_again_for_the_next_accepted_command`.
//!
//! # NEVER-SENT is not UNKNOWN, and the caller may act on the difference
//!
//! A command the worker WROTE and got no answer for is [`CommandOutcome::Disconnected`]: unknown,
//! it may have executed, nobody may resend it. A command the worker found the link already dead
//! for, or that was still queued when the worker stopped, is [`CommandOutcome::NeverSent`]: not
//! one byte reached the node, so a caller may reconnect and send THAT command (`vike-cli mcp`'s
//! `Server::execute` does). The evidence for `NeverSent` is positive, never inferred
//! ([`link_is_dead`] seeing a FIN before the write, or the queue outliving the worker); a FAILED
//! write stays `Disconnected`, because `write_all` cannot say whether half a frame went. A wrong
//! `Disconnected` costs a `node_snapshot`; a wrong `NeverSent` places an order twice.
//!
//! # A link that dies in SILENCE
//!
//! [`link_is_dead`] cannot see a death with no FIN or RST: silence peeks `WouldBlock`, the healthy
//! quiet verdict. The reply read is where the truth is, deadlined at [`CONTROL_REPLY_TIMEOUT`]
//! (armed once at connect). On expiry the in-flight command is `Disconnected` (it WAS written) and
//! everything still queued is `NeverSent`. Without the deadline the worker parked forever with
//! `is_connected()` still `true` and every later command wedged behind it.
//!
//! # How a reply is correlated, with NO wire change
//!
//! The protocol carries no request id, and adding one bumps [`crate::proto::NODE_PROTO_VERSION`],
//! which is signed into auth (the handshake would fail against every running node). None is
//! needed: a control connection never subscribes, the server answers each request with EXACTLY one
//! frame, and the worker is a strictly serial `write_frame` -> `read_frame` loop, so the reply it
//! reads belongs to the command it just wrote. The ticket is a client-side sequence carried WITH
//! the queued command (not derived from queue order), so racing senders cannot mis-attribute.
//!
//! # vike-core-free by construction
//!
//! [`ControlRejected`] is CRATE-LOCAL (not `vike_core::CommandRejected`); std only.
//!
//! # Preview, and the optional rationale (v4)
//!
//! [`preview_command`] is the synchronous per-call twin: a fresh short-lived connection, one
//! [`Request::Preview`], one [`Response::Preview`] verdict; nothing executes on the node.
//! [`try_command_with_reason`](RemoteControlHandle::try_command_with_reason) carries an OPTIONAL
//! rationale BESIDE the command: the node records it, sanitized, in its audit trail and it never
//! reaches the order; [`try_command`](RemoteControlHandle::try_command) passes `None`.

mod capability;
mod outcome;

pub use outcome::{CommandOutcome, CommandTicket};

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::handshake::{Refusal, ReplyWords, node_handshake, require_feature, resp_kind};
use crate::liveness::CONTROL_REPLY_TIMEOUT;
use crate::proto::{FEATURE_SETTINGS_WRITE, Request, Response, Scope, read_frame, write_frame};
use crate::wire::WireCommand;
use capability::required_feature;
use outcome::OutcomeBoard;

/// Has the peer already closed this control connection? A non-destructive, non-blocking probe the
/// worker runs just before each write, so a command offered to a link the node already closed is
/// [`CommandOutcome::NeverSent`] instead of UNKNOWN.
///
/// The worker parks in its command channel between commands, never in a socket read, so a FIN
/// that arrived while idle sits unread; a `peek` finds it without a second reader thread (which
/// would break the serial correlation). The socket is NON-BLOCKING for the probe only: a probe
/// under the read timeout would stall every healthy idle link.
///
/// Verdicts: `Ok(0)` (FIN) or any error but `WouldBlock` (RST, torn-down socket) = DEAD; unread
/// bytes or `WouldBlock` = alive.
///
/// ⚠ If blocking mode cannot be RESTORED the link is declared dead: a leftover non-blocking socket
/// turns the next `read_frame` into a fake transport fault, an UNKNOWN for a command really sent.
///
/// ⚠ It structurally CANNOT see a link that died in silence (that peeks `WouldBlock`, exactly like
/// a healthy quiet link); that is the reply read's job under [`CONTROL_REPLY_TIMEOUT`]. Do not
/// "strengthen" this probe: the wrong answer here is the one that places an order twice.
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

/// Outbound command queue depth: ample slack for a briefly-busy worker; once full the UI thread
/// gets `ControlRejected::Busy` instead of blocking.
const CONTROL_QUEUE_CAP: usize = 64;

/// One queued command: its ticket sequence, the [`WireCommand`] and the OPTIONAL rationale (v4).
/// The node records the rationale and lowers ONLY the command; the sequence never reaches the
/// wire, it lets the worker file the reply under the right ticket even when senders race.
struct Outbound {
    seq: u64,
    cmd: WireCommand,
    reason: Option<String>,
}

/// Why a remote command was not enqueued: the CRATE-LOCAL twin of `vike_core::CommandRejected`.
/// The first two arms mirror that type so a GUI treats a remote and an in-process lane alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRejected {
    /// The bounded queue is full; retry after a repaint.
    Busy,
    /// The worker has exited (transport fault or local shutdown); nothing more can be sent.
    Gone,
    /// The node's `Welcome.features` lacks the capability this command requires: REFUSED
    /// CLIENT-SIDE, nothing was enqueued or sent. A node that predates a VERB cannot decode it; one
    /// that predates a FIELD (a mount on `UpdateParams`, `account` on a mount or a reduce) decodes
    /// it, ignores the field and acts on the wrong target behind a normal Ack. Not transient:
    /// upgrade the node or do not send that shape to it.
    UnsupportedByNode,
}

/// A connected, authenticated, WRITE-only control handle to a remote headless node.
///
/// Construct with [`RemoteControlHandle::connect`]. A background worker owns the socket and drains
/// the command queue; [`try_command`](Self::try_command) never blocks. Drop it to tear down the
/// connection and join the worker.
pub struct RemoteControlHandle {
    /// The UI -> worker command queue (bounded, non-blocking `try_send`).
    cmd_tx: SyncSender<Outbound>,
    /// The node's `Welcome.features`, captured at connect: what a feature-gated command is checked
    /// against before the queue ([`ControlRejected::UnsupportedByNode`]).
    features: Vec<String>,
    /// `false` once the worker thread has exited (transport fault / local shutdown).
    connected: Arc<AtomicBool>,
    /// Mints the next [`CommandTicket`] sequence. Monotonic and client-local; never on the wire.
    next_seq: AtomicU64,
    /// Per-command outcomes keyed by ticket: the surface [`Self::await_outcome`] reads.
    outcomes: Arc<OutcomeBoard>,
    /// The LATCHED most-recent refusal for a status strip. ⚠ NOT a per-command answer (module doc).
    last_error: Arc<Mutex<Option<String>>>,
    /// A second handle to the socket, kept ONLY so [`Drop`] can `shutdown` it and unblock a worker
    /// parked in a socket read.
    shutdown_stream: TcpStream,
    /// The worker thread's join handle, taken and joined in [`Drop`].
    worker: Option<JoinHandle<()>>,
}

impl RemoteControlHandle {
    /// Connect to a node's control server at `addr`, run the shared handshake under
    /// [`Scope::Write`] with `control_key` (the raw bytes of `VIKE_TRADEHUB_CONTROL_KEY`), and
    /// spawn the worker. Unlike [`crate::remote_handle::RemoteCoreHandle::connect`] it SKIPS the
    /// `Subscribe`. A [`Response::AuthDenied`] (wrong/absent key, control disabled on the node) is
    /// [`io::ErrorKind::PermissionDenied`] carrying the server's reason; a protocol-version
    /// mismatch fails naming both versions.
    pub fn connect<A: ToSocketAddrs>(addr: A, control_key: &[u8]) -> io::Result<Self> {
        Self::connect_with_reply_timeout(addr, control_key, CONTROL_REPLY_TIMEOUT)
    }

    /// [`Self::connect`] with the reply deadline as a PARAMETER: the test seam that proves the
    /// silent-death property in milliseconds. Production passes [`CONTROL_REPLY_TIMEOUT`] only.
    ///
    /// ⚠ Unlike the observe half's deadline this one is NOT feature-negotiated, deliberately: that
    /// one waits on a node that PROMISED to fill an otherwise ordinary silence, this one on a reply
    /// the client just ASKED for, so silence is never ordinary here, whatever the node's vintage.
    pub(crate) fn connect_with_reply_timeout<A: ToSocketAddrs>(
        addr: A,
        control_key: &[u8],
        reply_timeout: Duration,
    ) -> io::Result<Self> {
        Self::connect_gated(addr, control_key, reply_timeout, None)
    }

    /// [`Self::connect_with_reply_timeout`] with a START GATE: `start`, when `Some`, is waited on
    /// ONCE as the worker's first act, before it takes anything from the queue, so a test can
    /// enqueue N commands provably (the thread that closes the channel is parked). It removes a
    /// real CI flake: a second `try_command` met `Gone` because the worker had already found the
    /// link dead and dropped its receiver. Shipped paths pass `None`.
    pub(crate) fn connect_gated<A: ToSocketAddrs>(
        addr: A,
        control_key: &[u8],
        reply_timeout: Duration,
        start: Option<Arc<Barrier>>,
    ) -> io::Result<Self> {
        let (stream, features) = node_handshake(addr, control_key, Scope::Write)?;

        // THE SILENT-DEATH DEADLINE (module doc). `node_handshake` clears its own bound on the way
        // out. Armed ONCE here, not per read: the worker only reads a reply it just asked for,
        // `link_is_dead` restores blocking mode but not the timeout (independent options), and a
        // per-pass deadline is one somebody eventually forgets.
        stream.set_read_timeout(Some(reply_timeout))?;

        // NO Subscribe. Keep a clone of the stream for a Drop-time shutdown.
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
                    // The start gate: once, OUTSIDE the loop. Never move it inside: a per-command
                    // rendezvous would change the timing of the program that ships.
                    if let Some(gate) = start {
                        gate.wait();
                    }
                    // Strictly serial: write a command, read ITS reply, file it under its ticket.
                    // A channel disconnect (handle dropped) ends the loop cleanly; a transport
                    // error ends it too. A refusal is recorded, never fatal.
                    // ⚠ THE FATAL VERDICT IS HELD, NOT PUBLISHED IN THE LOOP: `record` wakes
                    // waiters, so publishing before `connected.store(false)` let a polling observer
                    // read `NeverSent` ("link dead, resend is safe") and THEN `is_connected() ==
                    // true`. Pinned by
                    // `a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected`.
                    // The happy-path `record` stays in the loop: it does not end the worker.
                    let mut fatal: Option<(u64, CommandOutcome)> = None;
                    while let Ok(Outbound { seq, cmd, reason }) = cmd_rx.recv() {
                        // A close that arrived while the worker was idle is unread until now:
                        // ask before writing, so that command is `NeverSent`, not "may have run".
                        if link_is_dead(&io_stream) {
                            fatal = Some((seq, CommandOutcome::NeverSent));
                            break;
                        }
                        if write_frame(&mut io_stream, &Request::Command { cmd, reason }).is_err() {
                            // ⚠ UNKNOWN, not never-sent: `write_all` does not report progress,
                            // and half a frame the node completes is an executed command.
                            fatal = Some((seq, CommandOutcome::Disconnected));
                            break;
                        }
                        let outcome = match read_frame::<_, Response>(&mut io_stream) {
                            Ok(Response::Ack { coid }) => CommandOutcome::Accepted { coid },
                            // An accepted `SetSetting`: this path DROPS `restart_required`; a
                            // caller that needs it uses the synchronous [`set_setting`].
                            Ok(Response::SettingsWritten { .. }) => {
                                CommandOutcome::Accepted { coid: String::new() }
                            }
                            Ok(Response::Error(msg)) => CommandOutcome::Refused(msg),
                            Ok(other) => CommandOutcome::Refused(format!(
                                "unexpected reply to a command: {}",
                                resp_kind(&other)
                            )),
                            // Written, reply never came (a reported fault OR the reply deadline
                            // expiring on a silent link): UNKNOWN for THIS command, filed under
                            // its own ticket so the never-sent fallback cannot answer for it.
                            Err(_) => {
                                fatal = Some((seq, CommandOutcome::Disconnected));
                                break;
                            }
                        };
                        // The status-strip latch: set on a refusal, never cleared here.
                        if let CommandOutcome::Refused(msg) = &outcome {
                            *last_error.lock().expect("last_error poisoned") = Some(msg.clone());
                        }
                        outcomes.record(seq, outcome);
                    }
                    // ⚠ THE STORE COMES FIRST; everything below publishes (`record` and `finish`
                    // each `notify_all`), so "a terminal verdict implies a dead handle" holds by
                    // construction. `Release` here pairs with `is_connected`'s `Acquire`; the
                    // board's mutex carries the edge to a reader that has the verdict first.
                    connected.store(false, Ordering::Release);
                    // The verdict for the ONE command that ended the worker (`None`: the handle
                    // was dropped, no command in hand).
                    if let Some((seq, outcome)) = fatal {
                        outcomes.record(seq, outcome);
                    }
                    // Anything STILL QUEUED was provably never written: the loop writes only a
                    // command it has taken out, and each break filed that one's verdict first.
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

    /// Enqueue one command for the worker, NON-BLOCKING (`SyncSender::try_send`): a full queue is
    /// [`ControlRejected::Busy`], a closed one [`ControlRejected::Gone`]. Returns THIS command's
    /// [`CommandTicket`]: redeem it at [`Self::await_outcome`], or drop it (fire-and-forget).
    /// Exactly [`Self::try_command_with_reason`] with no rationale.
    pub fn try_command(&self, cmd: WireCommand) -> Result<CommandTicket, ControlRejected> {
        self.try_command_with_reason(cmd, None)
    }

    /// **Does the connected node REFUSE a command naming a venue it runs no engine for?** Read off
    /// the connect-time `Welcome.features` ([`crate::proto::FEATURE_VENUE_ROUTING`]).
    ///
    /// ⚠ Deliberately NOT in [`required_feature`]: that gate withholds a VERB a node cannot
    /// decode, and every node decodes these. An un-advertising node mis-ROUTES (applies the order to
    /// its primary engine), so what to withhold is a venue CHOICE, which only a caller holding the
    /// node's engine list can judge. Feed it to
    /// `vike_app_core::backend::tradehub_control::venue_routing_verdict` beside that list; `false`
    /// means "send only a venue you can see", not "send nothing".
    pub fn routes_by_venue(&self) -> bool {
        self.features.iter().any(|f| f == crate::proto::FEATURE_VENUE_ROUTING)
    }

    /// [`Self::try_command`] plus an OPTIONAL operator/agent RATIONALE (v4). The node records it
    /// (control characters stripped, length capped) in its audit trail and lowers only the
    /// command, so it never reaches `OrderRequest`, the fold, the journal or a venue.
    ///
    /// The ticket sequence is minted here and travels IN the queued payload, so it stays bound to
    /// this command even if another thread wins the race to the channel. A command whose
    /// [`required_feature`] the node does not advertise is [`ControlRejected::UnsupportedByNode`]
    /// before the queue: nothing is enqueued, nothing goes on the wire.
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

    /// Wait up to `timeout` for what the node did with the ONE command `ticket` names: the
    /// per-command outcome surface, and the only correct way to REPORT a command's result.
    ///
    /// Returns as soon as that reply lands (a condvar, not a settle sleep). `None` means not known
    /// within `timeout`: NOT that it failed, never that it succeeded. A ticket older than the
    /// board's retention window (`OUTCOME_RETENTION`, reachable only by a caller that BATCHES) is
    /// `None` too, also after the worker has died: never the `NeverSent` that would license a
    /// resend of a command that may have executed.
    pub fn await_outcome(
        &self,
        ticket: CommandTicket,
        timeout: Duration,
    ) -> Option<CommandOutcome> {
        self.outcomes.wait(ticket.0, timeout)
    }

    /// `true` while the worker thread is alive (the control connection is usable): a supervisor
    /// probe for the GUI, mirroring `CoreHandle::is_alive`.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// The LATCHED most-recent refusal (a node's error, or one latched with
    /// [`Self::latch_client_refusal`]), or `None` since the last [`Self::clear_last_error`]. A
    /// STATUS-STRIP view (`vike-desktop`'s control segment) that persists across repaints.
    ///
    /// ⚠ **Not this command's answer, and must never be read as one**: it names no command and stays
    /// set, so polled after a write it reports that refusal for every later command too, including
    /// ones the node executed. Report an outcome with [`Self::await_outcome`].
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("last_error poisoned").clone()
    }

    /// Dismiss the latched [`Self::last_error`]. Nothing clears it implicitly (not a later success,
    /// not [`Self::await_outcome`]): the owner of the banner decides when it has been seen.
    pub fn clear_last_error(&self) {
        *self.last_error.lock().expect("last_error poisoned") = None;
    }

    /// Latch a refusal the CLIENT made before the wire (`line`, operator-facing words) into the same
    /// channel a node's `Response::Error` uses, until [`Self::clear_last_error`] or a later refusal.
    ///
    /// ⚠ The worker never sees a command it was not handed, so a [`ControlRejected`] reached only
    /// the caller, and a GUI that rewrites its status line every frame lost it (a TP/SL click
    /// against a pre-bracket node did nothing visible). Like the latch, this is no command's answer.
    pub fn latch_client_refusal(&self, line: String) {
        *self.last_error.lock().expect("last_error poisoned") = Some(line);
    }
}

impl Drop for RemoteControlHandle {
    fn drop(&mut self) {
        // The worker parks in `cmd_rx.recv()` (idle) OR a socket read, so close BOTH wake paths:
        // swap out the real sender (an idle `recv()` sees a disconnect), shut the socket (a
        // mid-flight `read_frame` errors), then join so no thread outlives the handle.
        let (dead_tx, _dead_rx) = sync_channel::<Outbound>(0);
        drop(std::mem::replace(&mut self.cmd_tx, dead_tx));
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(join) = self.worker.take() {
            let _ = join.join();
        }
    }
}

/// DRY-RUN one command against a node's CONTROL gate: the SYNCHRONOUS twin of
/// [`RemoteControlHandle::try_command`]. Opens a fresh short-lived `Control` connection, sends
/// [`Request::Preview`], reads the [`Response::Preview`] verdict and drops the connection. NOTHING
/// executes on the node; it answers only what its edge gate WOULD decide. A per-call connection is
/// right: a preview has no ordering concern.
///
/// Returns `(accepted, reason)`: `reason` is `None` when accepted, the cause when refused. A
/// handshake failure (wrong/absent key, version skew, transport fault) is the [`io::Error`].
pub fn preview_command<A: ToSocketAddrs>(
    addr: A,
    control_key: &[u8],
    cmd: &WireCommand,
) -> io::Result<(bool, Option<String>)> {
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Write)?;
    // The same client-side gate as `try_command_with_reason`: previewing a verb the node cannot
    // DECODE would answer a generic "undecodable request", not a gate verdict.
    if let Some(feature) = required_feature(cmd) {
        require_feature(&features, feature, Refusal::OlderNode("preview"))?;
    }
    write_frame(&mut stream, &Request::Preview(cmd.clone()))?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::Preview { accepted, reason } => Ok((accepted, reason)),
        other => Err(PREVIEW_REPLY.mismatch(other)),
    }
}

/// [`preview_command`]'s reply wording (a write-scope verb: `AuthDenied` is its own arm).
const PREVIEW_REPLY: ReplyWords =
    ReplyWords { name: "preview", expected: "Preview", error_word: "error", auth_denied: true };

/// [`set_setting`]'s reply wording: a node's `Error` is a REFUSAL carrying the loader's text.
const SET_SETTING_REPLY: ReplyWords = ReplyWords {
    name: "settings write",
    expected: "SettingsWritten",
    error_word: "refused",
    auth_denied: true,
};

/// Write ONE settings row on a node, [`preview_command`]'s per-call shape: a fresh short-lived
/// [`Scope::Write`] connection, the `WireCommand::SetSetting` sent as a [`Request::Command`] (with
/// the optional audit `reason`), one reply. SYNCHRONOUS because the caller needs the verdict, above
/// all `restart_required`, which the fire-and-forget [`RemoteControlHandle`] path drops.
///
/// `key` is the FULL dotted key as the read half renders it, `value` the new value as text; the
/// node validates the row with its own loader, so a refusal echoes the loader's message.
///
/// ⚠ `file` and `confirm` are FILE-ERA wire fields the node ignores (`WireCommand::SetSetting`'s
/// doc): pass the key's section word as `file` (the released v0.1.35 daemon requires it on decode)
/// and `None` as `confirm` (deleted for every key by `docs/decisions/0086` point 7).
///
/// Sent ONLY when the node advertises [`FEATURE_SETTINGS_WRITE`]; otherwise
/// [`io::ErrorKind::Unsupported`] and NOTHING goes on the wire after the handshake. Returns
/// `restart_required`: `false` when the node HOT-APPLIED the value, `true` when a restart is needed.
/// A refusal is [`io::ErrorKind::InvalidData`] carrying the node's text.
pub fn set_setting<A: ToSocketAddrs>(
    addr: A,
    control_key: &[u8],
    file: &str,
    key: &str,
    value: &str,
    confirm: Option<&str>,
    reason: Option<&str>,
) -> io::Result<bool> {
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Write)?;
    require_feature(&features, FEATURE_SETTINGS_WRITE, Refusal::OlderNode("SetSetting"))?;
    let cmd = WireCommand::SetSetting {
        file: file.to_string(),
        key: key.to_string(),
        value: value.to_string(),
        confirm: confirm.map(str::to_string),
    };
    write_frame(&mut stream, &Request::Command { cmd, reason: reason.map(str::to_string) })?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SettingsWritten { restart_required } => Ok(restart_required),
        other => Err(SET_SETTING_REPLY.mismatch(other)),
    }
}

#[path = "never_sent_tests.rs"]
#[cfg(test)]
mod never_sent_tests;
