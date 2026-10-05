//! `RemoteControlHandle` — the WRITE half of the thin-client node connection (headless two-layer
//! plan, Layer 2, PR-12/PR-13): the `try_command`-shaped twin of [`crate::remote_handle::RemoteCoreHandle`].
//!
//! A laptop GUI holds this handle to send order commands to a live headless node's CONTROL server. It
//! authenticates under [`Scope::Write`] and — load-bearing — NEVER subscribes: a subscribed connection
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
//! recent refusal" for a status strip — a node's `Response::Error`, or a refusal the CLIENT made
//! before the wire and latched with
//! [`latch_client_refusal`](RemoteControlHandle::latch_client_refusal) — cleared only by an explicit
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
//! its exact pre-v4 signature as a thin wrapper passing `None`, so every existing caller
//! (`vike-desktop`'s order buttons, over the handle `vike-app-core`'s observe bridge connects) is
//! unchanged.

use std::collections::VecDeque;
use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::handshake::{node_handshake, resp_kind};
use crate::liveness::CONTROL_REPLY_TIMEOUT;
use crate::proto::{
    FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT, FEATURE_BRACKET,
    FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_VERBS, FEATURE_SETTINGS_WRITE, FEATURE_STRATEGY_VERBS,
    Request, Response, Scope, read_frame, write_frame,
};
use crate::wire::WireCommand;

/// The `Welcome.features` capability a [`WireCommand`] requires before it may be SENT, or `None`
/// for the pre-negotiation vocabulary every server speaks. The one authority both the async
/// ([`RemoteControlHandle::try_command_with_reason`]) and per-call ([`preview_command`]) write
/// paths consult, so the two cannot disagree about which verbs are gated.
fn required_feature(cmd: &WireCommand) -> Option<&'static str> {
    match cmd {
        // The TP/SL bracket rides its OWN word: a node that predates it cannot decode the variant
        // (the mount-verbs argument). It names no account, so no account arm below can claim it.
        WireCommand::Bracket(_) => Some(FEATURE_BRACKET),
        WireCommand::UpdateParams { .. } => Some(FEATURE_STRATEGY_VERBS),
        // The B5 mount verbs ride their OWN capability, not `strategy-verbs`: a B4-era node
        // advertises that string yet cannot decode these variants — see `FEATURE_MOUNT_VERBS`'s
        // doc for why re-using the old string would defeat the client-side refusal.
        // ⚠ **BEFORE the arm below, and the order is the whole check.** A mount that NAMES an
        // account owes the sharper string, and `MountStrategy { .. }` would swallow it — the arm
        // below matches every mount, named or not, so putting this second would answer
        // `strategy-mount-verbs` for a labelled mount and let it fly at a node that drops the
        // field. That node decodes the frame, `#[serde(default)]` makes it `account: None`, and
        // the strategy mounts on the venue's default account for as long as it runs.
        WireCommand::MountStrategy { account: Some(_), .. } => Some(FEATURE_MOUNT_ACCOUNT),
        WireCommand::MountStrategy { .. } | WireCommand::UnmountStrategy { .. } => {
            Some(FEATURE_MOUNT_VERBS)
        }
        // The REQ-7 settings write rides its OWN capability (not `settings-show`): a read-half
        // node advertises that string yet cannot decode this variant — the mount-verbs argument
        // verbatim.
        WireCommand::SetSetting { .. } => Some(FEATURE_SETTINGS_WRITE),
        // ⚠ **CONDITIONAL on the PAYLOAD, not on the variant** — the only arm here that is, and the
        // reason is the forward-compat property the field was designed around: a command naming NO
        // account serialises byte-identically to the pre-field frame, so an old node decodes it
        // exactly as it always did and no capability is owed. One that NAMES an account owes it.
        //
        // And owes it more sharply than any capability above. Elsewhere an unadvertised feature
        // means the server cannot decode the frame and answers `Response::Error` — loud, and the
        // connection survives. Here `#[serde(default)]` makes the old node decode it FINE, with
        // `account: None`, which at `N = 1` routes silently to the venue's only engine. The refusal
        // below is the only thing between a labelled order and the wrong book.
        // ⚠ **BEFORE the `names_an_account` arm below, and the ordering is load-bearing for the
        // same reason the mount-account arm's is**: that guard matches these three too, and
        // answering `account-routing` for them would let a labelled reduce fly at a node that
        // advertises that string TRUTHFULLY — it does route a labelled `Submit` — and still drops
        // the field on these verbs. Every node built before the reducing verbs learned their
        // account is that node: its `lower_command` destructured them as `{ venue, symbol, .. }`,
        // its core fanned the verb over EVERY account of the exchange, and it answered `accepted`.
        // A node that honours the field advertises `FEATURE_ACCOUNT_SCOPED_REDUCE` (today's
        // `crates/vike-tradehub/src/server/handshake.rs` does, since owner ruling "B" of 2026-09-26), so this
        // arm refuses LOCALLY exactly against the older nodes that would widen the verb — and lets
        // it fly to every node that narrows it. That constant's doc carries what the claim means.
        //
        // ⚠ `account: Some(..)` ONLY, and the `None` case must never reach here: §4.5 rules that a
        // risk-REDUCING venue verb naming NO account fans out to every account of that venue
        // DELIBERATELY, and the UNSCOPED panic button (`MarketExit { venue: None, account: None }`)
        // must reach every engine and can never be refused by any gate in this family. Both fall
        // through to `_ => None` below, exactly as they did before this arm existed.
        WireCommand::MassCancel { account: Some(_), .. }
        | WireCommand::Flatten { account: Some(_), .. }
        | WireCommand::MarketExit { account: Some(_), .. } => Some(FEATURE_ACCOUNT_SCOPED_REDUCE),
        // After the reduce arm the only command naming an account that reaches here is a `Submit`,
        // and it owes `account-scoped-submit` — NOT `account-routing`, which this arm answered
        // until a review of the CLI's stage-5 deletion measured the fleet: every released node from
        // `v0.1.27` through `v0.1.32` advertises `account-routing` while its `lower_command` builds
        // an `OrderRequest` with no account field, so the string let a labelled submit fly at a
        // node that routed it by venue alone — onto whichever account the venue's one engine was,
        // printing success. A string a shipped node already advertises cannot be withdrawn from it,
        // so the client stops trusting it; `FEATURE_ACCOUNT_SCOPED_SUBMIT`'s doc carries the
        // measurement. EVERY named account owes it, the positive `DEFAULT` included — that node
        // drops `DEFAULT` too, and `parse_wire_account` is the one reader of that spelling, so
        // this arm does not grow a second one.
        c if names_an_account(c) => Some(FEATURE_ACCOUNT_SCOPED_SUBMIT),
        _ => None,
    }
}

/// Does this command NAME an account? — the payload half of [`required_feature`]'s account arm.
///
/// ⚠ `Cancel`/`Modify` are absent deliberately rather than forgotten: they address an order by its
/// client-order-id, and the id already names the engine that owns it, so there is no account for a
/// caller to state and none for a node to misread.
fn names_an_account(cmd: &WireCommand) -> bool {
    match cmd {
        WireCommand::Submit(req) => req.account.is_some(),
        WireCommand::MassCancel { account, .. }
        | WireCommand::Flatten { account, .. }
        | WireCommand::MarketExit { account, .. } => account.is_some(),
        _ => false,
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
/// one is enough in practice; the ring exists because a FIRE-AND-FORGET caller (`vike-desktop`'s
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
    /// The HIGHEST ticket sequence ever EVICTED from [`Self::done`] — the mark that separates "this
    /// answer fell out of the retention window" from "this command was never recorded at all".
    ///
    /// ⚠ It exists because those two cases had ONE answer and it was the dangerous one.
    /// [`OutcomeBoard::wait`] checks `done` and then falls through to `terminal`, which is
    /// [`CommandOutcome::NeverSent`] on every real exit path — so once the worker had stopped, a
    /// ticket whose verdict had been EVICTED resolved to "not one byte went, resending is safe" for
    /// a command that may have been written AND executed. A mark makes the evicted case answer
    /// `None` — "unknown", which is what [`RemoteControlHandle::await_outcome`]'s doc has always
    /// promised an over-retention ticket.
    ///
    /// ⚠ It is a WATERMARK and not a comparison against `done`'s front, deliberately: sequences are
    /// minted by `next_seq.fetch_add` BEFORE `try_send`, so under concurrent senders `done` is NOT
    /// strictly ascending and its front's sequence is not a boundary. The watermark only ever
    /// rises, and it is consulted AFTER `done` — so a recorded answer always wins, and the only
    /// thing the mark can do to a ticket is turn a `NeverSent` into a `None`. That is the safe
    /// direction: a caller that gets `None` reconnects and reads the book, where a caller that gets
    /// a wrong `NeverSent` places the order twice.
    evicted_high_water: Option<u64>,
}

#[derive(Default)]
struct OutcomeBoard {
    state: Mutex<BoardState>,
    resolved: Condvar,
}

impl OutcomeBoard {
    /// File one command's outcome under its ticket sequence, evicting the oldest entry once the
    /// retention window is full, and wake every waiter.
    ///
    /// An eviction RAISES [`BoardState::evicted_high_water`], because an answer that has fallen out
    /// of the window must not be indistinguishable from one that was never recorded: the fallthrough
    /// for the second is [`CommandOutcome::NeverSent`], and telling a caller that about a command
    /// that WAS written is how one order becomes two.
    fn record(&self, seq: u64, outcome: CommandOutcome) {
        let mut st = self.state.lock().expect("outcome board poisoned");
        if st.done.len() >= OUTCOME_RETENTION
            && let Some((evicted, _)) = st.done.pop_front()
        {
            let raised = st.evicted_high_water.map_or(evicted, |hw| hw.max(evicted));
            st.evicted_high_water = Some(raised);
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
    ///
    /// ⚠ And an EVICTED ticket is checked before that flag too, answering `None`. The three checks
    /// are in the only order that is honest: what the board KNOWS about this command, then what it
    /// has FORGOTTEN about it, and only then the blanket verdict for commands it never saw. Without
    /// the middle one a ticket whose answer aged out of the window inherited `terminal` —
    /// [`CommandOutcome::NeverSent`] on every exit path the worker actually has — so a command that
    /// had been written, acked and executed came back as "not one byte went, resending is safe" the
    /// moment the connection dropped. `None` is both the safe answer and the one
    /// [`RemoteControlHandle::await_outcome`]'s doc already promises for an over-retention ticket.
    fn wait(&self, seq: u64, timeout: Duration) -> Option<CommandOutcome> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock().expect("outcome board poisoned");
        loop {
            if let Some((_, outcome)) = st.done.iter().find(|(s, _)| *s == seq) {
                return Some(outcome.clone());
            }
            // Evicted, and therefore unanswerable FOREVER — sequences are unique and a sequence is
            // never recorded twice, so waiting out the timeout could only produce the same `None`
            // more slowly.
            if st.evicted_high_water.is_some_and(|hw| seq <= hw) {
                return None;
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
    /// The LATCHING "most recent refusal" for a status strip (`vike-desktop`'s control segment): a
    /// node's `Response::Error`, written by the worker, or a client-side refusal a caller latched
    /// with [`Self::latch_client_refusal`]. Cleared only by [`Self::clear_last_error`]. ⚠ NOT a
    /// per-command answer — see the module doc.
    last_error: Arc<Mutex<Option<String>>>,
    /// A second handle to the same socket, kept ONLY so [`Drop`] can `shutdown` it and unblock the
    /// worker if it is parked in a socket read.
    shutdown_stream: TcpStream,
    /// The worker thread's join handle, taken and joined in [`Drop`].
    worker: Option<JoinHandle<()>>,
}

impl RemoteControlHandle {
    /// Connect to a node's control server at `addr`, run the full handshake under [`Scope::Write`]
    /// using `control_key` (the raw bytes of `VIKE_TRADEHUB_CONTROL_KEY`), and spawn the worker.
    ///
    /// The handshake is the shared `crate::handshake` sequence run under [`Scope::Write`] —
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
        Self::connect_gated(addr, control_key, reply_timeout, None)
    }

    /// [`Self::connect_with_reply_timeout`] with a START GATE for the worker — the second seam of
    /// the same shape, and for the same reason: a property that is otherwise proven by racing the
    /// scheduler is proven deterministically instead.
    ///
    /// `start`, when `Some`, is waited on ONCE as the worker's first act, OUTSIDE the command loop,
    /// so the worker is parked before it has taken anything out of the queue. A test can then
    /// enqueue N commands that are PROVABLY enqueued — the channel cannot have closed, because the
    /// thread that closes it is parked — release the gate, and assert on the outcomes.
    ///
    /// ⚠ What it fixes is a real flake with a real cause, not a slow test.
    /// `a_command_queued_behind_a_dead_link_is_never_sent` sends two commands and its SECOND
    /// `try_command` failed on CI with `Gone`. Nothing ordered the two: `try_send` wakes the worker
    /// from inside the call, the worker's first command meets an already-dead link, and it runs
    /// `break` → `finish` → `drop(cmd_rx)` to completion — so on a loaded runner the main thread
    /// can be preempted for exactly that long between its two sends and find a closed channel. The
    /// gate removes the scheduler from the enqueue entirely. Widening the test's existing sleep
    /// could not have helped: that sleep is for the FIN, and the worker is parked in `recv()`
    /// throughout it.
    ///
    /// [`Self::connect`] and [`Self::connect_with_reply_timeout`] pass `None`, so the SHIPPED path
    /// is byte-identical — one moved `Option` that is never `Some` outside `#[cfg(test)]`.
    pub(crate) fn connect_gated<A: ToSocketAddrs>(
        addr: A,
        control_key: &[u8],
        reply_timeout: Duration,
        start: Option<Arc<Barrier>>,
    ) -> io::Result<Self> {
        let (stream, features) = node_handshake(addr, control_key, Scope::Write)?;

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
                    // THE START GATE, once per connection and OUTSIDE the loop below: `None` in
                    // every shipped binary (see `connect_gated`), `Some` only where a test needs
                    // the queue filled before this thread has taken anything out of it. It must
                    // never move INTO the command loop — a per-command rendezvous would change the
                    // worker's timing on every pass and prove a different program than the one that
                    // ships.
                    if let Some(gate) = start {
                        gate.wait();
                    }
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
    /// `vike-desktop`'s order buttons, which `let _ =` the result) is unaffected by the ticket: they
    /// send byte-identical `reason: None` commands and ignore the answer.
    pub fn try_command(&self, cmd: WireCommand) -> Result<CommandTicket, ControlRejected> {
        self.try_command_with_reason(cmd, None)
    }

    /// **Does the connected node REFUSE a command naming a venue it runs no engine for?** — read
    /// off the `Welcome.features` captured at [`Self::connect`]
    /// ([`crate::proto::FEATURE_VENUE_ROUTING`]).
    ///
    /// ⚠ Deliberately NOT wired into [`required_feature`], and the distinction is the whole point:
    /// that gate withholds a VERB a node cannot decode, and every venue-carrying verb here is one
    /// every node decodes perfectly well. What an un-advertising node does wrong is ROUTE — it
    /// takes the venue an operator picked and applies the order to its primary engine — so the
    /// thing to withhold is a venue CHOICE, which only the caller holding the node's published
    /// engine list can evaluate. Feed this to
    /// `vike_app_core::backend::tradehub_control::venue_routing_verdict` beside that list; do not branch on
    /// it alone (`false` does not mean "send nothing", it means "send only a venue you can see").
    pub fn routes_by_venue(&self) -> bool {
        self.features.iter().any(|f| f == crate::proto::FEATURE_VENUE_ROUTING)
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
    ///
    /// ⚠ That last parenthesis was TRUE ONLY WHILE THE WORKER LIVED, and the gap was the dangerous
    /// direction. An over-retention ticket found no entry and fell through to the board's terminal
    /// verdict, which is [`CommandOutcome::NeverSent`] on every exit path the worker has — so once
    /// the connection dropped, a command whose answer had aged out of the window reported "not one
    /// byte of it went on the wire, resending is safe" for a command that may have been written and
    /// EXECUTED. It is `None` in both cases now ([`BoardState::evicted_high_water`]). Not reachable
    /// from today's callers — `vike-cli mcp` and `vike-cli trade` each await on the line after the
    /// send, and the desktop drops its tickets — but it becomes reachable the moment anything
    /// BATCHES (fire N commands, then collect N outcomes), which is precisely when more than
    /// [`OUTCOME_RETENTION`] tickets are outstanding at once.
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

    /// The LATCHED most-recent refusal — a node's error (a refused command, control-not-enabled, or
    /// an unexpected reply) or a client-side refusal latched with [`Self::latch_client_refusal`] —
    /// or `None` if nothing has been refused since the last [`Self::clear_last_error`]. This is a
    /// STATUS-STRIP view — `vike-desktop`'s control segment paints it, and it deliberately persists
    /// so a refusal is not lost between repaints.
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

    /// Latch a refusal the CLIENT made before the wire (`line`, the operator-facing words) into
    /// the same channel a node's `Response::Error` uses, so the status strip shows it and it
    /// persists until [`Self::clear_last_error`]; a later refusal of either kind replaces it.
    ///
    /// ⚠ **Why the client's refusals need this door.** [`Self::try_command`]'s
    /// [`ControlRejected`] answers reach only the caller; the worker never sees a command it was
    /// never handed, so nothing latched them. A GUI that painted the answer into its own status
    /// line lost it the next frame (`vike-desktop` rewrites that line every frame), so a TP/SL
    /// click against a node that predates the bracket was a click that did nothing. Like the
    /// latch it writes, this names no command and is no command's answer.
    pub fn latch_client_refusal(&self, line: String) {
        *self.last_error.lock().expect("last_error poisoned") = Some(line);
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
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Write)?;
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
/// shape: open a fresh short-lived [`Scope::Write`] connection (the same handshake
/// [`RemoteControlHandle::connect`] runs), send the `WireCommand::SetSetting` as a
/// [`Request::Command`] (with the optional audit `reason` beside it), read the reply, and drop
/// the connection. SYNCHRONOUS on purpose: unlike an order, a settings write's caller always
/// needs the verdict — above all the reply's `restart_required`, which the fire-and-forget
/// [`RemoteControlHandle`] path cannot surface (its worker maps the reply to an empty-coid
/// acceptance).
///
/// The write is ONE ROW of the node's settings database, named by `key` — the FULL dotted key
/// exactly as the read half renders it — and `value` is the new value as text. The node validates
/// the row with its own loader before it commits, so a refusal echoes the loader's message.
///
/// ⚠ `file` and `confirm` are FILE-ERA wire fields the node ignores (see
/// `WireCommand::SetSetting`'s doc for the two-step retirement). They stay parameters here only
/// until step 2 deletes them from the wire: a caller passes the key's own section word as `file`
/// (the released v0.1.35 daemon requires the field on decode) and `None` as `confirm` (the typed
/// confirm was deleted for every key by `docs/decisions/0086` point 7).
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
    let (mut stream, features) = node_handshake(addr, control_key, Scope::Write)?;
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

#[path = "remote_control_tests.rs"]
#[cfg(test)]
mod remote_control_tests;

/// NEVER-SENT vs UNKNOWN, against a scripted node — the two ways a control command can end without
/// a verdict, and the evidence that separates them.
///
/// The distinction is not cosmetic: `vike-cli mcp` reconnects and re-sends on one of these and
/// refuses to on the other, so a test that could not tell them apart would be the whole safety
/// argument taken on trust. The node here is scripted rather than real because the two cases differ
/// only in WHEN the peer closes — before the command is offered, or after it has been read — and
/// that is a script, not a configuration.
#[path = "never_sent_tests.rs"]
#[cfg(test)]
mod never_sent_tests;
