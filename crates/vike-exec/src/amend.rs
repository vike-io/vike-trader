//! The cancel-then-new AMEND state machine — pure. It decides, from what the engine's registry says
//! about the OLD order and the NEW (replacement) one, what to send next and when the amend has ended;
//! it sends nothing, reads no clock and touches no registry. The caller that acts on it is `vike-core`'s
//! `runtime/apply/amend.rs`, which observes the two orders at the drain-loop boundary and carries out
//! each [`Act`](crate::amend::Act) through the engine's ordinary submit and cancel paths.
//!
//! # Why a level-triggered machine
//!
//! It is handed the CURRENT status of both orders on every pass instead of reacting to events, so a
//! lost, reordered or late event cannot wedge it: the same pass that would have followed the event
//! follows the status. The one thing events cannot say, that nothing happened, is a clock the caller
//! passes in (`now_ms`).
//!
//! # The two orderings
//!
//! * [`Placement::NewFirst`](crate::amend::Placement::NewFirst) — a REDUCE-ONLY trigger order (a protective stop). The new one is placed
//!   first and the old one is cancelled once the new one is ANSWERED, so the position is never without a
//!   stop. Two reduce-only orders cannot close more than the position holds (the venue clips the second),
//!   so the overlap cannot over-close; if one of the two fills during the overlap the other is cancelled
//!   (the first to fill wins).
//! * [`Placement::CancelFirst`](crate::amend::Placement::CancelFirst) — everything else (a limit order, an entry stop): the old one is
//!   cancelled, and only when the venue says it is gone is the new one placed. A limit order loses its
//!   place in the queue; an entry stop has a moment with neither. A fill that beats the cancel ends the
//!   amend (nothing is placed twice).
//!
//! Either way a failure leaves the OLD order standing whenever it can (`End::old_stands`), and says why in
//! words ([`End::words`](crate::amend::End::words)); the one failure that cannot is the refusal of the new order after the old one
//! was cancelled, and its words say the order is GONE.

use vike_model::{OrderRequest, OrderStatus};

/// How long the venue may take to answer the NEW order before the amend gives up on it.
pub const PLACE_WAIT_MS: i64 = 30_000;
/// How long the venue may take to confirm the cancel of the OLD order before the amend says so.
pub const CANCEL_WAIT_MS: i64 = 15_000;
/// A cancel-first amend whose cancel is still unconfirmed after this long is dropped (the order is
/// said to STAND; if the cancel lands later the order is gone and the words said so).
pub const CANCEL_GIVE_UP_MS: i64 = 600_000;

/// What the registry says about one order, reduced to what the machine needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Not in the registry (never placed here, or already swept away as terminal).
    Unknown,
    /// Sent, not answered yet (INITIALIZED, SUBMITTED).
    Pending,
    /// Answered and resting (ACCEPTED, TRIGGERED, PENDING_CANCEL, the emulated and released states).
    Resting,
    /// PARTIALLY_FILLED: resting with some of it executed.
    Partly,
    /// FILLED.
    Filled,
    /// CANCELED, EXPIRED, LIQUIDATED: ended without executing in full.
    Gone,
    /// REJECTED, DENIED: the venue (or the gate) refused it.
    Refused,
}

impl Seen {
    /// Reduce a registry status (`None` = not in the registry).
    #[must_use]
    pub fn of(status: Option<OrderStatus>) -> Seen {
        match status {
            None => Seen::Unknown,
            Some(OrderStatus::Initialized | OrderStatus::Submitted) => Seen::Pending,
            Some(
                OrderStatus::Accepted
                | OrderStatus::Triggered
                | OrderStatus::PendingCancel
                | OrderStatus::Emulated
                | OrderStatus::Released,
            ) => Seen::Resting,
            Some(OrderStatus::PartiallyFilled) => Seen::Partly,
            Some(OrderStatus::Filled) => Seen::Filled,
            Some(OrderStatus::Canceled | OrderStatus::Expired | OrderStatus::Liquidated) => {
                Seen::Gone
            }
            Some(OrderStatus::Rejected | OrderStatus::Denied) => Seen::Refused,
        }
    }

    /// Still on its way to being, or being, at the venue.
    fn alive(self) -> bool {
        matches!(self, Seen::Pending | Seen::Resting | Seen::Partly)
    }

    /// Answered by the venue and in force, or already executed.
    fn in_force(self) -> bool {
        matches!(self, Seen::Resting | Seen::Partly | Seen::Filled)
    }
}

/// Which order goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Place the new order, cancel the old one once the new one is answered.
    NewFirst,
    /// Cancel the old order, place the new one once the old one is gone.
    CancelFirst,
}

impl Placement {
    /// The ordering for the resting order `req`: a reduce-only trigger order goes NEW-FIRST (never an
    /// unprotected moment); every other kind goes cancel-first.
    #[must_use]
    pub fn of(req: &OrderRequest) -> Placement {
        if req.reduce_only && req.order_type.is_trigger() {
            Placement::NewFirst
        } else {
            Placement::CancelFirst
        }
    }
}

/// What the caller sends this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Nothing.
    Nothing,
    /// Place the new order now (cancel-first, the old one is gone).
    SubmitNew,
    /// Cancel the old order (new-first, the new one is answered; or the new one ended and the old
    /// must follow).
    CancelOld,
    /// Cancel the new order (the old one ended; the new one is redundant, or it never answered).
    CancelNew,
}

/// How an amend ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The new order rests (or already executed) and the old one is gone.
    Replaced,
    /// The venue REFUSED the new order. `old_stands`: new-first (the old order was never touched).
    NewRefused { old_stands: bool },
    /// The venue did not answer the new order within [`PLACE_WAIT_MS`]. `old_stands` as above.
    NewSilent { old_stands: bool },
    /// New-first: the new order is answered but the old one did not confirm its cancel within
    /// [`CANCEL_WAIT_MS`]: BOTH rest.
    OldNotCancelled,
    /// Cancel-first: the cancel was unconfirmed for [`CANCEL_GIVE_UP_MS`]; the amend is dropped.
    CancelUnconfirmed,
    /// The old order FILLED during the amend; nothing is left to move (a new one, if placed, is
    /// cancelled).
    OldFilled,
    /// The old order was cancelled or expired by someone else during the amend (a new one, if
    /// placed, is cancelled).
    OldGone,
    /// The new order was cancelled or expired on its own (a bracket's sibling filled, say) before the
    /// old one was cancelled; the old one is cancelled too. Cancel-first: the order is gone.
    NewGone,
    /// New-first: the old order is cancelled and the new one is gone as well: nothing rests.
    BothGone,
}

impl End {
    /// `true` when the OLD order is untouched and still resting after this ending.
    #[must_use]
    pub fn old_stands(self) -> bool {
        matches!(
            self,
            End::NewRefused { old_stands: true }
                | End::NewSilent { old_stands: true }
                | End::OldNotCancelled
                | End::CancelUnconfirmed
        )
    }

    /// `true` when the amend did what was asked.
    #[must_use]
    pub fn succeeded(self) -> bool {
        self == End::Replaced
    }

    /// The operator's words for this ending. `old` and `new` are the two client order ids (`new` empty
    /// when none was placed), `reason` is the venue's own words for a refusal when it gave any.
    #[must_use]
    pub fn words(self, old: &str, new: &str, reason: Option<&str>) -> String {
        let why = reason.filter(|r| !r.is_empty()).map(|r| format!(" ({r})")).unwrap_or_default();
        match self {
            End::Replaced => format!("order {old} was replaced by {new}"),
            End::NewRefused { old_stands: true } => format!(
                "the venue REFUSED the replacement {new}{why}; the old order {old} STANDS unchanged"
            ),
            End::NewRefused { old_stands: false } => format!(
                "the old order {old} was cancelled but the venue REFUSED the replacement {new}{why}: \
                 the order is GONE, place it again"
            ),
            End::NewSilent { old_stands: true } => format!(
                "the venue did not answer the replacement {new} within {} s; it is being cancelled and \
                 the old order {old} STANDS unchanged",
                PLACE_WAIT_MS / 1000
            ),
            End::NewSilent { old_stands: false } => format!(
                "the old order {old} was cancelled and the venue has not answered the replacement {new} \
                 within {} s: it may still be placed, check the open orders before placing it again",
                PLACE_WAIT_MS / 1000
            ),
            End::OldNotCancelled => format!(
                "the replacement {new} is in force but the venue did not confirm the cancel of the old \
                 order {old} within {} s: BOTH rest, cancel {old} by hand",
                CANCEL_WAIT_MS / 1000
            ),
            End::CancelUnconfirmed => format!(
                "the venue did not confirm the cancel of {old} for {} s, so nothing was placed and the \
                 order is left as it is; if it is cancelled later it is GONE and must be placed again",
                CANCEL_GIVE_UP_MS / 1000
            ),
            End::OldFilled => format!(
                "the order {old} FILLED before it could be changed; nothing is left to move{}",
                if new.is_empty() { "" } else { " and the replacement is being cancelled" }
            ),
            End::OldGone => format!(
                "the order {old} was cancelled by someone else before it could be changed{}",
                if new.is_empty() { "" } else { "; the replacement is being cancelled" }
            ),
            End::NewGone => format!(
                "the replacement {new} ended on its own (cancelled or expired) while {old} was being \
                 moved; {old} is cancelled too, nothing of this order rests"
            ),
            End::BothGone => format!(
                "the old order {old} was cancelled and the replacement {new} is gone as well: NOTHING \
                 of this order rests, place it again"
            ),
        }
    }
}

/// The result of one pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// What to send.
    pub send: Act,
    /// `Some` when the amend is over (the caller drops the machine after carrying out `send`).
    pub end: Option<End>,
}

impl Step {
    const WAIT: Step = Step { send: Act::Nothing, end: None };

    fn send(send: Act) -> Step {
        Step { send, end: None }
    }

    fn end(send: Act, end: End) -> Step {
        Step { send, end: Some(end) }
    }
}

/// Where the amend stands. New-first runs `Placing` (the new order is out) then `Cancelling` (the
/// old order's cancel is out); cancel-first runs them the other way round: it BEGINS in `Cancelling`
/// (the cancel is out) and moves to `Placing` once the old order is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Placing,
    Cancelling,
}

/// One amend in flight: an ordering, a phase and the instant the phase began.
#[derive(Debug, Clone)]
pub struct Replace {
    placement: Placement,
    phase: Phase,
    since_ms: i64,
    warned_cancel: bool,
}

impl Replace {
    /// Begin an amend whose first half (new-first: the new order; cancel-first: the cancel) is being
    /// sent now, at `now_ms`.
    #[must_use]
    pub fn begin(placement: Placement, now_ms: i64) -> Replace {
        let phase = match placement {
            Placement::NewFirst => Phase::Placing,
            Placement::CancelFirst => Phase::Cancelling,
        };
        Replace { placement, phase, since_ms: now_ms, warned_cancel: false }
    }

    /// The ordering this amend runs.
    #[must_use]
    pub fn placement(&self) -> Placement {
        self.placement
    }

    /// `true` once a cancel-first amend has waited past [`CANCEL_WAIT_MS`] and said so (the caller
    /// asks it once, to print the advisory once).
    #[must_use]
    pub fn take_cancel_advisory(&mut self, now_ms: i64) -> bool {
        let due = self.placement == Placement::CancelFirst
            && self.phase == Phase::Cancelling
            && !self.warned_cancel
            && now_ms.saturating_sub(self.since_ms) > CANCEL_WAIT_MS;
        if due {
            self.warned_cancel = true;
        }
        due
    }

    /// One pass: the registry's view of the `old` and `new` orders at `now_ms`.
    #[must_use]
    pub fn step(&mut self, old: Seen, new: Seen, now_ms: i64) -> Step {
        let waited = now_ms.saturating_sub(self.since_ms);
        match (self.placement, self.phase) {
            // ---- new-first: the new order is out ------------------------------------------------
            (Placement::NewFirst, Phase::Placing) => {
                // The old order ended while the new one was being placed: the new one is redundant.
                if old == Seen::Filled {
                    return Step::end(cancel_new_if_alive(new), End::OldFilled);
                }
                if !old.alive() {
                    return Step::end(cancel_new_if_alive(new), End::OldGone);
                }
                match new {
                    Seen::Refused => Step::end(Act::Nothing, End::NewRefused { old_stands: true }),
                    Seen::Resting | Seen::Partly | Seen::Filled => {
                        self.phase = Phase::Cancelling;
                        self.since_ms = now_ms;
                        Step::send(Act::CancelOld)
                    }
                    Seen::Gone => Step::end(Act::CancelOld, End::NewGone),
                    Seen::Pending | Seen::Unknown if waited > PLACE_WAIT_MS => {
                        Step::end(cancel_new_if_alive(new), End::NewSilent { old_stands: true })
                    }
                    Seen::Pending | Seen::Unknown => Step::WAIT,
                }
            }
            // ---- new-first: the old order's cancel is out ---------------------------------------
            (Placement::NewFirst, Phase::Cancelling) => {
                if old == Seen::Filled {
                    return Step::end(cancel_new_if_alive(new), End::OldFilled);
                }
                if old == Seen::Gone || old == Seen::Unknown {
                    // We cancelled it. Whether the new order still stands decides the ending.
                    return if new.in_force() {
                        Step::end(Act::Nothing, End::Replaced)
                    } else {
                        Step::end(Act::Nothing, End::BothGone)
                    };
                }
                if !new.in_force() && new != Seen::Pending {
                    // The new order ended on its own while the old one still rests: the old one
                    // follows (the pair is one logical order).
                    return Step::end(Act::CancelOld, End::NewGone);
                }
                if waited > CANCEL_WAIT_MS {
                    return Step::end(Act::Nothing, End::OldNotCancelled);
                }
                Step::WAIT
            }
            // ---- cancel-first: the cancel is out ------------------------------------------------
            (Placement::CancelFirst, Phase::Cancelling) => match old {
                Seen::Gone | Seen::Unknown => {
                    self.phase = Phase::Placing;
                    self.since_ms = now_ms;
                    Step::send(Act::SubmitNew)
                }
                Seen::Filled => Step::end(Act::Nothing, End::OldFilled),
                Seen::Refused => Step::end(Act::Nothing, End::OldGone),
                Seen::Pending | Seen::Resting | Seen::Partly if waited > CANCEL_GIVE_UP_MS => {
                    Step::end(Act::Nothing, End::CancelUnconfirmed)
                }
                Seen::Pending | Seen::Resting | Seen::Partly => Step::WAIT,
            },
            // ---- cancel-first: the new order is out ---------------------------------------------
            (Placement::CancelFirst, Phase::Placing) => match new {
                Seen::Resting | Seen::Partly | Seen::Filled => {
                    Step::end(Act::Nothing, End::Replaced)
                }
                Seen::Refused => Step::end(Act::Nothing, End::NewRefused { old_stands: false }),
                Seen::Gone => Step::end(Act::Nothing, End::NewGone),
                Seen::Pending | Seen::Unknown if waited > PLACE_WAIT_MS => {
                    Step::end(Act::Nothing, End::NewSilent { old_stands: false })
                }
                Seen::Pending | Seen::Unknown => Step::WAIT,
            },
        }
    }
}

/// Cancel the new order only while it can still be at the venue.
fn cancel_new_if_alive(new: Seen) -> Act {
    if new.alive() { Act::CancelNew } else { Act::Nothing }
}

#[path = "amend_tests.rs"]
#[cfg(test)]
mod amend_tests;
