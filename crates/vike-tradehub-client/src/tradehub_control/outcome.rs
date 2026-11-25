//! Per-command outcomes: the ticket a send hands back, the verdict it resolves to, the board, the node's no
//! to one command as it came ([`CommandRefusal`]), the one wording of a command that has no answer
//! ([`UnansweredCommand`]) and of one this client did not queue (`ControlRejected`'s `Display`).

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::refusal::HubRefusal;
use vike_utility::errors::{FailureOutcome, render_refusal};

use super::{CONTROL_QUEUE_CAP, ControlRejected};
use crate::liveness::ONE_SHOT_REPLY_TIMEOUT;
use crate::proto::refusal_outcome;
use crate::refusals::frame_outcome;

/// How many resolved outcomes the board retains. A fire-and-forget caller (`vike-desktop`'s order
/// buttons) never awaits, so an unbounded map would leak; sized to [`CONTROL_QUEUE_CAP`] so every
/// command that can be in flight at once keeps its answer.
const OUTCOME_RETENTION: usize = CONTROL_QUEUE_CAP;

/// The identity of ONE enqueued command, returned by
/// [`try_command`](super::TradehubControlHandle::try_command) and redeemed at
/// [`await_outcome`](super::TradehubControlHandle::await_outcome). Opaque and `Copy`. Deliberately
/// NOT `#[must_use]`: fire-and-forget is legitimate, and a lint would teach `let _ =`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandTicket(pub(super) u64);

/// What the node did with ONE command. Exhaustive over the worker's serial reply loop: every
/// command either gets its reply or loses the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The node ACCEPTED it (`Response::Ack`).
    Accepted {
        /// The minted/echoed order-client-id; empty for an account-wide verb (mass-cancel,
        /// flatten, close-all, trading-state, a settings write).
        order_client_id: String,
        /// The node's notes on the order it sent (`Response::Ack`'s `notes`): plain-words sentences, today
        /// "placed into a closed market". A caller that reports the acceptance prints them next to it;
        /// they never change what happened (the order WAS sent). Empty for most commands.
        notes: Vec<String>,
    },
    /// NOT NOW: the node answered that it cannot take a command yet (it is still starting, or its core is
    /// busy). Nothing was executed, and the same command is taken shortly. Decided ONCE, by the worker
    /// (`CommandOutcome::of_refusal`), from the frame's id or an older node's words: a surface matches the
    /// variant.
    NotNow(CommandRefusal),
    /// The node REFUSED it, its final no (`Response::Refused`, `Response::Error`), or its door denied this peer
    /// (`AuthDenied`, a device revoked since connect). Nothing was executed.
    Refused(CommandRefusal),
    /// The command WAS written and no answer to it was read: the connection died, or the node sent a reply of
    /// another verb (the worker then ends the link, its serial correlation gone). The outcome is genuinely
    /// UNKNOWN, it may have executed. Never report it as "sent", never resend it.
    Disconnected,
    /// The link was ALREADY dead when this command reached the worker, so not one byte of it went
    /// on the wire: a caller may reconnect and send THIS command; never a `Disconnected` one.
    ///
    /// ⚠ CONSERVATIVE on purpose: produced only on POSITIVE evidence (the pre-write probe
    /// `link_is_dead` seeing EOF, or a command still queued behind a stopped worker). A FAILED
    /// write stays [`Self::Disconnected`]: `write_all` cannot say whether half a frame went. A
    /// wrong `Disconnected` costs a `node_snapshot`; a wrong `NeverSent` places an order twice.
    NeverSent,
}

impl CommandOutcome {
    /// The node's no to one command, read ONCE and on its type (decision 0301): [`Self::NotNow`] for a node
    /// still starting or a busy core, by the frame's id ([`frame_outcome`]) or an older node's words
    /// ([`refusal_outcome`]), else [`Self::Refused`].
    pub(super) fn of_refusal(refusal: CommandRefusal) -> Self {
        match refusal.failure_outcome() {
            FailureOutcome::NotNow => CommandOutcome::NotNow(refusal),
            _ => CommandOutcome::Refused(refusal),
        }
    }

    /// The [`UnansweredCommand`] this outcome is; `None` for an answer (accepted, not now or refused), which
    /// each surface words itself.
    #[must_use]
    pub fn unanswered(&self) -> Option<UnansweredCommand> {
        match self {
            CommandOutcome::Disconnected => Some(UnansweredCommand::LinkDropped),
            CommandOutcome::NeverSent => Some(UnansweredCommand::LinkAlreadyClosed),
            CommandOutcome::Accepted { .. }
            | CommandOutcome::NotNow(_)
            | CommandOutcome::Refused(_) => None,
        }
    }
}

/// The node's no to ONE command, as it came ([`CommandOutcome::NotNow`], [`CommandOutcome::Refused`]): nothing
/// was executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandRefusal {
    /// The refusal FRAME: a refusal a node of this build declared with an id (`crate::refusals`), decided on by
    /// that id; or the door's `AuthDenied`, under the id this client gives it
    /// (`vike_hub_protocol::call::HubAccessRefusal`, a final no).
    Frame(HubRefusal),
    /// Text alone: a node's `Response::Error` (an older node, which sends no frame, or a refusal not yet
    /// declared with an id), or this client's words for a reply that is not a command's.
    Text(String),
}

impl CommandRefusal {
    /// What the refused command DID: [`FailureOutcome::NotNow`] for a node still starting or a busy core
    /// (nothing executed; the same command is taken shortly), else the node's final [`FailureOutcome::Refused`].
    /// A frame is read by its id ([`frame_outcome`]), a text by [`refusal_outcome`] (an older node's words).
    /// Private: the worker asks it once ([`CommandOutcome::of_refusal`]), and so does the wording below; a
    /// caller matches the [`CommandOutcome`] variant.
    fn failure_outcome(&self) -> FailureOutcome {
        match self {
            CommandRefusal::Frame(refusal) => frame_outcome(refusal),
            CommandRefusal::Text(text) => refusal_outcome(text),
        }
    }
}

/// The ONE wording every surface shows for a refused command (the desktop's Trade window strip, `vike-cli`,
/// `vike-mcp`, `vike-telegram`): a NOT NOW says so and that the same command is taken shortly, a final no is the
/// node's words. A frame's words carry its id (`vike_utility::errors::render_refusal`'s `error [<id>]: <text>`).
impl fmt::Display for CommandRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let words = match self {
            CommandRefusal::Frame(refusal) => render_refusal(&refusal.id, &refusal.text),
            CommandRefusal::Text(text) => text.clone(),
        };
        match self.failure_outcome() {
            FailureOutcome::NotNow => write!(
                f,
                "NOT NOW ({words}): nothing was executed; send the same command again shortly."
            ),
            _ => f.write_str(&words),
        }
    }
}

/// A command this client offered and holds no answer to. Its `Display` is the ONE wording every surface
/// shows for it (the desktop's Trade window strip, `vike-cli`, `vike-mcp`); a surface adds only its pointer
/// to the read that settles an unknown one (`node_snapshot`, `trade order ls`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnansweredCommand {
    /// [`CommandOutcome::Disconnected`]: written, and the link died before the answer was read.
    LinkDropped,
    /// No answer within the caller's wait (`TradehubControlHandle::await_outcome` gave `None`): the node
    /// may still answer, and the command may still execute.
    NoAnswerWithin(Duration),
    /// [`CommandOutcome::NeverSent`]: the link was already closed, so not one byte of it went.
    LinkAlreadyClosed,
}

impl UnansweredCommand {
    /// What the command DID: OUTCOME UNKNOWN (never resend it under a new id) or NOT SENT (safe to send).
    #[must_use]
    pub const fn failure_outcome(self) -> FailureOutcome {
        match self {
            UnansweredCommand::LinkDropped | UnansweredCommand::NoAnswerWithin(_) => {
                FailureOutcome::OutcomeUnknown
            }
            UnansweredCommand::LinkAlreadyClosed => FailureOutcome::NotSent,
        }
    }

    /// The case of a [`set_setting`](super::set_setting) error that is [`HubCallError::OutcomeUnknown`] (the
    /// row may be written and its answer was lost): a reply read that ran out is [`Self::NoAnswerWithin`] the
    /// one-shot reply bound, any other loss [`Self::LinkDropped`]. `None` for every other variant: the write
    /// did not leave, or the node answered.
    #[must_use]
    pub fn of_settings_write(error: &HubCallError) -> Option<Self> {
        match error {
            HubCallError::OutcomeUnknown(lost) => Some(match lost.kind() {
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    UnansweredCommand::NoAnswerWithin(ONE_SHOT_REPLY_TIMEOUT)
                }
                _ => UnansweredCommand::LinkDropped,
            }),
            _ => None,
        }
    }
}

/// What an OUTCOME UNKNOWN says to do, true of every write it words (an order, a control command, a settings
/// write, whose surface adds the read that settles it): the node's door refuses an order client id an earlier
/// order carries (`crates/vike-tradehub/src/server/refusal.rs`'s `order_client_id_reuse_refusal`), so the id is
/// the retry's idempotency key.
const RESEND_UNDER_THE_SAME_ID: &str = "Read what the node holds before sending it again: an order \
     sent again under the same order client id is refused if the first one reached the node.";

impl fmt::Display for UnansweredCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnansweredCommand::LinkDropped => write!(
                f,
                "OUTCOME UNKNOWN: the control connection dropped after this command was written and \
                 before the node answered, so it may have executed. {RESEND_UNDER_THE_SAME_ID}"
            ),
            UnansweredCommand::NoAnswerWithin(wait) => write!(
                f,
                "OUTCOME UNKNOWN: the node did not answer within {wait:?}, so this command may still \
                 execute. {RESEND_UNDER_THE_SAME_ID}"
            ),
            UnansweredCommand::LinkAlreadyClosed => f.write_str(
                "NOT SENT: the control connection was already closed, so nothing of this command \
                 reached the node and nothing executed. It is safe to send it again.",
            ),
        }
    }
}

/// The ONE wording of a command this client did not queue, every surface's (`vike-cli`, `vike-mcp`,
/// `vike-telegram`): NOT SENT, nothing of it left this process. A surface adds only its own pointer (a
/// reconnect, the `account` an older node takes). The desktop's status strip keeps its own short lines
/// (`vike-app-core`'s `refused_send_status`), cut to fit the strip.
impl fmt::Display for ControlRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ControlRejected::Busy => f.write_str(
                "NOT SENT: the queue to the node was full, so nothing of this command was enqueued \
                 and nothing executed. Send it again shortly.",
            ),
            // The worker is gone, so nothing of the command was queued: the never-sent claim, in its words.
            ControlRejected::Gone => write!(f, "{}", UnansweredCommand::LinkAlreadyClosed),
            ControlRejected::UnsupportedByHub => f.write_str(
                "NOT SENT: this node does not advertise the capability this command requires, so it \
                 was refused CLIENT-side and nothing went on the wire.",
            ),
        }
    }
}

/// The worker -> caller board: resolved `(ticket, outcome)` pairs plus the worker's end-of-life
/// verdict, behind one [`Mutex`] with a [`Condvar`] so a waiter wakes the instant its reply lands.
#[derive(Default)]
struct BoardState {
    /// Resolved outcomes, oldest first, bounded to [`OUTCOME_RETENTION`].
    done: VecDeque<(u64, CommandOutcome)>,
    /// `Some(outcome)` once the worker has exited: every ticket it never resolved answers this,
    /// so a waiter fails fast. An outcome, not a flag: a still-QUEUED command is `NeverSent`,
    /// while the one in flight is filed `Disconnected` under its own ticket BEFORE this is set.
    terminal: Option<CommandOutcome>,
    /// The HIGHEST sequence ever EVICTED from [`Self::done`]: separates "aged out of the window"
    /// (answer `None`, unknown) from "never recorded" (answer `terminal`). Without it an evicted,
    /// possibly EXECUTED command answered `NeverSent` once the worker died.
    ///
    /// ⚠ A WATERMARK, not a comparison against `done`'s front: sequences are minted before
    /// `try_send`, so under concurrent senders `done` is not ascending. It only rises and is
    /// consulted AFTER `done`, so it can only turn a `NeverSent` into a `None` (the safe way).
    evicted_high_water: Option<u64>,
}

#[derive(Default)]
pub(super) struct CommandOutcomeBoard {
    state: Mutex<BoardState>,
    resolved: Condvar,
}

impl CommandOutcomeBoard {
    /// File one outcome under its ticket sequence, evicting the oldest once the window is full
    /// (raising [`BoardState::evicted_high_water`]), and wake every waiter.
    pub(super) fn record(&self, seq: u64, outcome: CommandOutcome) {
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

    /// Mark the worker dead and wake every waiter: each unresolved ticket now answers `terminal`,
    /// what is true of a command STILL QUEUED at this moment.
    pub(super) fn finish(&self, terminal: CommandOutcome) {
        let mut st = self.state.lock().expect("outcome board poisoned");
        st.terminal = Some(terminal);
        drop(st);
        self.resolved.notify_all();
    }

    /// Block up to `timeout` for `seq`'s outcome.
    ///
    /// ⚠ The three checks are in the only honest order: what the board KNOWS (a recorded answer,
    /// even after the worker exited), then what it has FORGOTTEN (an evicted ticket is `None`,
    /// never `terminal`), and only then the blanket verdict for commands it never saw.
    pub(super) fn wait(&self, seq: u64, timeout: Duration) -> Option<CommandOutcome> {
        let deadline = Instant::now() + timeout;
        let mut st = self.state.lock().expect("outcome board poisoned");
        loop {
            if let Some((_, outcome)) = st.done.iter().find(|(s, _)| *s == seq) {
                return Some(outcome.clone());
            }
            // Evicted: unanswerable forever (a sequence is never recorded twice).
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

#[path = "outcome_tests.rs"]
#[cfg(test)]
mod outcome_tests;
