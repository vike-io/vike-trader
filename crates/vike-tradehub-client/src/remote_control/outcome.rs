//! Per-command outcomes: the ticket a send hands back, the verdict it resolves to, and the board.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::CONTROL_QUEUE_CAP;

/// How many resolved outcomes the board retains. A fire-and-forget caller (`vike-desktop`'s order
/// buttons) never awaits, so an unbounded map would leak; sized to [`CONTROL_QUEUE_CAP`] so every
/// command that can be in flight at once keeps its answer.
const OUTCOME_RETENTION: usize = CONTROL_QUEUE_CAP;

/// The identity of ONE enqueued command, returned by
/// [`try_command`](super::RemoteControlHandle::try_command) and redeemed at
/// [`await_outcome`](super::RemoteControlHandle::await_outcome). Opaque and `Copy`. Deliberately
/// NOT `#[must_use]`: fire-and-forget is legitimate, and a lint would teach `let _ =`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandTicket(pub(super) u64);

/// What the node did with ONE command. Exhaustive over the worker's serial reply loop: every
/// command either gets its reply or loses the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// The node ACCEPTED it (`Response::Ack`).
    Accepted {
        /// The minted/echoed client-order-id; empty for an account-wide verb (mass-cancel,
        /// flatten, market-exit, trading-state, a settings write).
        coid: String,
    },
    /// The node REFUSED it (`Response::Error`) or answered something that is not a command reply
    /// (e.g. `AuthDenied`). Nothing was executed. The string is the node's own text, unmodified.
    Refused(String),
    /// The command WAS written and the connection died before its reply was read: the outcome is
    /// genuinely UNKNOWN, it may have executed. Never report it as "sent", never resend it.
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
pub(super) struct OutcomeBoard {
    state: Mutex<BoardState>,
    resolved: Condvar,
}

impl OutcomeBoard {
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
