//! **The NEVER-SENT queue**: which command frames the lease link has lost, which it merely cannot
//! account for, and what each deserves.
//!
//! A command is in exactly one of four states (`docs/superpowers/plans/` wave 2, B.7; `wire_design.md` C.7):
//!
//! | state | meaning | on link death |
//! |---|---|---|
//! | **Queued** (in the client's channel, or being written: `write_all` not yet returned) | **NEVER-SENT** | the link thread classifies it at once: a submit gets a terminal `OrderRejected`, a modify and a sheddable cancel a NON-terminal advisory, a cancel that may not be shed is HELD and flushed at reconnect |
//! | **Written** (`write_all` returned, no `CommandAck`) | **UNKNOWN** | nothing is synthesized; resolved after the reconnect |
//! | **Received** (`CommandAck`) | the gatewayhub owns it | events and the stuck-order watchdog take over; UNKNOWN once nothing more can reach this lease and nothing answered it (a new epoch, a lease closed for good: `Shared::with_unanswered`); a cancel that may not be shed stays IN DOUBT until an event answers it and is sent again after an epoch change (`TradeLeaseShared::take_cancels_in_doubt`) |
//! | **Venue-answered** (an `Event` for the order client id) | the venue spoke | none |
//!
//! # Resolution of UNKNOWN
//!
//! `cseq` rises by one per frame, one TCP connection preserves order, and a reconnect is FENCED (the
//! gatewayhub reads the old connection to EOF before it answers the new `TradeOpen`), so
//! `TradeOpened.received_through_command_sequence` is **definitive inside one `gateway_epoch`**: a Written frame at or
//! below it was received (its events, or the watchdog, take over); one above it was NEVER received and
//! gets the NEVER-SENT disposition. Across an epoch change the watermark is gone: a Written submit
//! stays UNKNOWN (its order client id goes to `ResolveOrders`, then reconcile, then the watchdog's last resort);
//! a Written modify, and a Written cancel that MAY be shed (routine), gets the NON-terminal advisory that its
//! outcome is unknown (`OUTCOME_UNKNOWN_CANCEL_REASON`, `OUTCOME_UNKNOWN_MODIFY_REASON`: the old daemon may have
//! sent it, so it is not sent again, and the order, if live, rests as the venue holds it); a Written confirm is
//! dropped (a prod); a Written cancel that may NOT be shed (risk-off, unclassified) is held again with the
//! rest, because "never shed" is the dead-man's rule and a cancel is idempotent. So is one the old daemon
//! ACKNOWLEDGED and nothing answered (`TradeLeaseShared::take_cancels_in_doubt`, held through
//! [`TradeLeaseCommandLedger::hold_cancel`]): the daemon that owed it an answer is gone.
//! After an epoch change the held frames are released when the daemon answers an `AdapterStart`, not at
//! the reconnect: a restarted daemon has no adapter yet, and a cancel or a teaching sent to it is spent on
//! nothing (`TradeLeaseCommandLedger::resumed`, `TradeLeaseCommandLedger::adapter_answered`). The TEACHING goes first: the answer releases
//! the held teaching, then the held cancels behind it (a plain `start_adapter`), or, for a caller that
//! teaches the orders minted since boot itself (`TradeLease::restart_adapter`), the teaching ONLY, the
//! cancels following its `TradeLeaseCommandLedger::release_held`, so on a venue whose order-id map the restart emptied no
//! cancel reaches the adapter before the order it names has been taught (a cancel arriving while the
//! gate is closed queues behind them: `TradeLeaseCommandLedger::defer`).
//!
//! A lease CLOSED FOR GOOD settles its Written frames too (`TradeLeaseCommandLedger::closed`): after `Bye(Protocol)` the
//! daemon provably read none of what is still unacknowledged (it stops at the violating frame, after
//! acknowledging every earlier one), so those frames get the NEVER-SENT words; after any other end (a
//! takeover's drain, a lost link whose redial finds the lease held or refused, a stop) the daemon may have
//! executed them, so a submit stays UNKNOWN (named in an `error` line and in `unknown_orders`) and nothing
//! is synthesized. This ledger forgets a frame at its acknowledgement; what the daemon then owes it is an answer
//! through its ring, and the submit that answer never reached is `Shared::with_unanswered`'s.
//!
//! ⚠ **The client NEVER re-sends a Written ORDER.** The only frames sent twice are the idempotent
//! `AdoptRestored` teaching (it names state, not an order) and a cancel that may not be shed (held, or Written
//! or unanswered across an epoch change: it names an order and an absolute outcome, so it is idempotent).
//!
//! ⚠ **A double terminal is structurally impossible**: an order client id leaves this ledger with exactly one
//! disposition. The synthesized terminal `OrderRejected` is only ever made for a frame the gatewayhub
//! provably did not read; every UNKNOWN one is left to the venue's own event.
//!
//! This file is PURE (no socket, no thread, no clock): the link thread feeds it and acts on what it
//! returns, and its tests drive every row of the table above.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::OrderRefusalId;
use vike_model::events::refusal_id_of;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};
use vike_model::{OrderCancelIntent, OrderRequest};

use crate::proto::{TradeOpened, TradeUp};

/// The terminal reject of a submit that never left the client: the link was down (or the write failed)
/// when the frame was due.
pub const NEVER_SENT_SUBMIT_REASON: &str =
    "gatewayhub trade link down: the order was never sent to the gatewayhub";

/// The terminal reject of a submit the gatewayhub's watermark proves it never read.
pub const NEVER_RECEIVED_SUBMIT_REASON: &str =
    "gatewayhub trade link lost before the gatewayhub read the order: it was never executed";

/// The terminal reject of a submit whose frame alone exceeds the lease's up ceiling (it could never be
/// sent, split or not).
pub const TOO_LARGE_SUBMIT_REASON: &str = "gatewayhub trade frame too large: the order exceeds the lease's frame ceiling and was never sent";

/// The advisory of a cancel that was not sent (NON-terminal: the order, if live, is still resting).
pub const NEVER_SENT_CANCEL_REASON: &str =
    "gatewayhub trade link down: the cancel was never sent; the order, if live, is still resting";

/// The advisory of a cancel the gatewayhub's watermark proves it never read.
pub const NEVER_RECEIVED_CANCEL_REASON: &str = "gatewayhub trade link lost before the gatewayhub read \
     the cancel; the order, if live, is still resting";

/// The advisory of a modify that was not sent (NON-terminal: the order keeps its terms).
pub const NEVER_SENT_MODIFY_REASON: &str =
    "gatewayhub trade link down: the modify was never sent; the order keeps its terms";

/// The advisory of a modify the gatewayhub's watermark proves it never read.
pub const NEVER_RECEIVED_MODIFY_REASON: &str = "gatewayhub trade link lost before the gatewayhub read \
     the modify; the order keeps its terms";

/// The advisory of a routine cancel Written to a gatewayhub that restarted before acknowledging it
/// (NON-terminal: the old daemon may or may not have sent it on, and it is not sent again).
pub const OUTCOME_UNKNOWN_CANCEL_REASON: &str = "gatewayhub restarted before acknowledging the cancel: \
     its outcome is unknown and it was not sent again; the order, if live, is still resting";

/// The advisory of a modify Written to a gatewayhub that restarted before acknowledging it.
pub const OUTCOME_UNKNOWN_MODIFY_REASON: &str = "gatewayhub restarted before acknowledging the modify: \
     its outcome is unknown and it was not sent again; the order may keep its old terms";

/// The advisory of a HELD cancel the link gave up on (the lease is closed for good: a takeover, a
/// refusal, a stop).
pub const LEASE_CLOSED_CANCEL_REASON: &str = "gatewayhub trade lease closed: the cancel was \
     never sent; the order, if live, is still resting";

/// The terminal reject of a submit made on a lease that is closed for good (taken over, refused,
/// stopped); the tradehub log names the cause.
pub const LEASE_CLOSED_SUBMIT_REASON: &str =
    "gatewayhub trade lease closed (see the tradehub log): the order was never sent";

/// The advisory of a modify made on a lease that is closed for good.
pub const LEASE_CLOSED_MODIFY_REASON: &str = "gatewayhub trade lease closed (see the tradehub log): \
     the modify was never sent; the order keeps its terms";

/// The terminal reject of a submit carrying a number that cannot cross the wire (NaN or infinity).
pub const NON_FINITE_SUBMIT_REASON: &str =
    "gatewayhub trade: the order carries a non-finite number (NaN or infinity) and was never sent";

/// The advisory of a modify carrying a number that cannot cross the wire.
pub const NON_FINITE_MODIFY_REASON: &str = "gatewayhub trade: the modify carries a non-finite number \
     (NaN or infinity) and was never sent; the order keeps its terms";

/// Why a frame is being given its NEVER-SENT disposition: the texts differ so an operator can tell a
/// frame that never left from one the watermark proved unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TradeLeaseCommandLedgerWhy {
    /// It never left the client (queued while the link was down, or its write failed).
    NeverSent,
    /// It was Written, and the gatewayhub's `received_through_command_sequence` proves it never read it.
    NeverReceived,
    /// The frame alone exceeds the up ceiling.
    TooLarge,
    /// The lease is closed for good.
    Closed,
}

/// What became of the connection a lease is closing over: it decides what a Written-unacknowledged frame
/// is (see [`TradeLeaseCommandLedger::closed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TradeLeaseCommandLedgerFate {
    /// The daemon ended the lease at a frame it refused (`Bye(Protocol)`), acknowledging every earlier
    /// frame first and reading nothing after: what is still unacknowledged was not read.
    Unprocessed,
    /// The connection ended some other way (a takeover's drain, a lost link, a stop): the daemon may have
    /// executed any frame whose acknowledgement did not arrive.
    Unknown,
}

/// What closing a lease owes the core.
#[derive(Debug, Default)]
pub(crate) struct TradeLeaseCommandLedgerClose {
    /// Synthesized events (held cancels, and under [`TradeLeaseCommandLedgerFate::Unprocessed`] the Written frames).
    pub events: Vec<Event>,
    /// Order client ids of Written submits that stay UNKNOWN (under [`TradeLeaseCommandLedgerFate::Unknown`]).
    pub unknown_submits: Vec<String>,
    /// Order client ids of Written cancels the daemon may or may not have executed (under [`TradeLeaseCommandLedgerFate::Unknown`]).
    pub unknown_cancels: Vec<String>,
}

/// What a reconnect resolved.
#[derive(Debug, Default)]
pub(crate) struct TradeLeaseCommandLedgerResume {
    /// Synthesized events for the frames the watermark proved unread (to be delivered to the core).
    pub events: Vec<Event>,
    /// Order client ids of Written submits that stay UNKNOWN across an epoch change (ask `ResolveOrders`).
    pub resolve: Vec<String>,
    /// Frames to send now, in order, renumbered by the writer: HELD cancels and teaching.
    pub flush: Vec<TradeUp>,
}

/// The command ledger of one lease. See the module doc.
#[derive(Debug, Default)]
pub(crate) struct TradeLeaseCommandLedger {
    /// The `cseq` of the last frame whose write completed (`0` = none yet). Never burned by a failed
    /// write: the next frame reuses the number, which is safe because a frame whose `write_all` failed
    /// was never a complete frame at the daemon.
    last: u64,
    /// Written, not yet acknowledged, by `cseq`.
    written: BTreeMap<u64, TradeUp>,
    /// Frames that must not be shed, waiting for a link: risk-off (and unclassified) cancels and the
    /// idempotent teaching. Sent at the next reconnect, in this order, before anything newer.
    held: Vec<TradeUp>,
    /// Order client ids with a held cancel, so a repeated sweep during an outage queues each once.
    held_cancels: BTreeSet<String>,
    /// An epoch change happened and the daemon has not yet answered an `AdapterStart`: the held frames
    /// wait for it (see [`TradeLeaseCommandLedger::resumed`]).
    awaiting_adapter: bool,
    /// The daemon answered the `AdapterStart` and the TEACHING went out, but the held cancels wait for the
    /// caller's [`TradeLeaseCommandLedger::release_held`]: the caller teaches the orders minted since boot first, and a
    /// cancel sent ahead of that teaching fails on a venue whose map the restart emptied.
    awaiting_teaching: bool,
}

/// The order-command variants of `TradeUp` (each carries a `cseq`) and every other variant, spelled ONCE
/// for the read and the stamp below: `command_command_sequence!(frame, cseq => <command arm>, <other arm>)` expands to
/// the exhaustive `match`, with `cseq` bound to the command's field (by reference, as `frame` is).
macro_rules! command_command_sequence {
    ($frame:expr, $cseq:ident => $command:expr, $other:expr) => {
        match $frame {
            TradeUp::Submit { $cseq, .. }
            | TradeUp::SubmitBatch { $cseq, .. }
            | TradeUp::Cancel { $cseq, .. }
            | TradeUp::CancelBatch { $cseq, .. }
            | TradeUp::Modify { $cseq, .. }
            | TradeUp::Confirm { $cseq, .. }
            | TradeUp::AdoptRestored { $cseq, .. } => $command,
            TradeUp::AdapterStart(_)
            | TradeUp::WireIds { .. }
            | TradeUp::ResolveOrders { .. }
            | TradeUp::IdentityRecord { .. }
            | TradeUp::Reconcile { .. }
            | TradeUp::Ping
            | TradeUp::Detach { .. }
            | TradeUp::TradeStatus { .. }
            | TradeUp::SymbolProperties { .. }
            | TradeUp::VenueRead { .. } => $other,
        }
    };
}

/// The `cseq` a command frame carries; `None` for a frame that is not a command.
pub(crate) fn command_sequence_of(frame: &TradeUp) -> Option<u64> {
    command_command_sequence!(frame, command_sequence => Some(*command_sequence), None)
}

/// Stamp `n` on a command frame; a frame that is not a command is left alone (`false`).
pub(crate) fn set_command_sequence(frame: &mut TradeUp, n: u64) -> bool {
    command_command_sequence!(
        frame,
        command_sequence => {
            *command_sequence = n;
            true
        },
        false
    )
}

/// Whether every number on the order can cross the wire. `serde_json` writes a non-finite `f64` as
/// `null`, which the daemon cannot decode: such a frame would be a malformed request on a lease, and a
/// lease that is told it sent garbage ends. So it never leaves the client.
fn order_is_finite(o: &OrderRequest) -> bool {
    let opt = |v: Option<f64>| v.is_none_or(f64::is_finite);
    o.qty.is_finite()
        && o.weight.is_finite()
        && opt(o.price)
        && opt(o.trigger_price)
        && opt(o.stop)
        && opt(o.trail)
        && opt(o.extreme)
}

/// Screen a command frame before it is written: the frame to send (`None` = nothing left to send) and the
/// events for what was refused. A refused submit is a terminal reject, a refused modify an advisory; a
/// batch loses only its offenders.
pub(crate) fn screen_finite(frame: TradeUp) -> (Option<TradeUp>, Vec<Event>) {
    match frame {
        TradeUp::Submit { order, .. } if !order_is_finite(&order) => {
            (None, vec![rejected(&order, None, NON_FINITE_SUBMIT_REASON)])
        }
        TradeUp::SubmitBatch { command_sequence: cseq, orders } => {
            let (good, bad): (Vec<OrderRequest>, Vec<OrderRequest>) =
                orders.into_iter().partition(order_is_finite);
            let events = bad.iter().map(|o| rejected(o, None, NON_FINITE_SUBMIT_REASON)).collect();
            if good.is_empty() {
                (None, events)
            } else {
                (Some(TradeUp::SubmitBatch { command_sequence: cseq, orders: good }), events)
            }
        }
        TradeUp::Modify { order, new_qty, new_price, new_trigger_price, .. }
            if !order_is_finite(&order)
                || !new_qty.is_none_or(f64::is_finite)
                || !new_price.is_none_or(f64::is_finite)
                || !new_trigger_price.is_none_or(f64::is_finite) =>
        {
            (None, vec![modify_rejected(&order, None, NON_FINITE_MODIFY_REASON)])
        }
        other => (Some(other), Vec::new()),
    }
}

fn rejected(order: &OrderRequest, refusal: Option<OrderRefusalId>, reason: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order.order_client_id.clone(),
        reason: reason.to_string().into(),
        timestamp: order.timestamp,
        refusal_id: refusal.and_then(|refusal| refusal_id_of(refusal.word())),
    })
}

fn cancel_rejected(order_client_id: &str, refusal: Option<OrderRefusalId>, reason: &str) -> Event {
    Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: order_client_id.to_string(),
        reason: reason.to_string().into(),
        timestamp: 0,
        refusal_id: refusal.and_then(|refusal| refusal_id_of(refusal.word())),
    })
}

fn modify_rejected(order: &OrderRequest, refusal: Option<OrderRefusalId>, reason: &str) -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: order.order_client_id.clone(),
        reason: reason.to_string().into(),
        // stamped when made, `vike_bridge_core::rest::amend_answer_time`'s rule (a layer this crate may not link)
        timestamp: vike_model::now_ms(),
        refusal_id: refusal.and_then(|refusal| refusal_id_of(refusal.word())),
    })
}

impl TradeLeaseCommandLedger {
    /// The `cseq` the next frame will carry.
    pub fn next_command_sequence(&self) -> u64 {
        self.last + 1
    }

    /// A command frame's write COMPLETED: it is Written (UNKNOWN until acknowledged) and its `cseq` is
    /// spent.
    pub fn wrote(&mut self, frame: TradeUp) {
        if let Some(cseq) = command_sequence_of(&frame) {
            self.last = self.last.max(cseq);
            self.written.insert(cseq, frame);
        }
    }

    /// The gatewayhub acknowledged everything through `through`: those frames are Received. (Acks
    /// arrive in order on one connection, so a `CommandAck` for `n` retires every earlier frame too.)
    pub fn acked(&mut self, through: u64) {
        self.written = self.written.split_off(&through.saturating_add(1));
    }

    /// How many frames are Written and unacknowledged.
    pub fn in_flight(&self) -> usize {
        self.written.len()
    }

    /// How many frames are held for the next link.
    #[cfg(test)]
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// The Written-unacknowledged frames, oldest first (for the status report and tests).
    #[cfg(test)]
    pub fn written_cseqs(&self) -> Vec<u64> {
        self.written.keys().copied().collect()
    }

    /// **The NEVER-SENT disposition of one command frame**: the events to synthesize, if any. A frame
    /// that must not be shed is HELD instead; a request or control frame is not an order and yields
    /// nothing (its waiter is failed by the link thread).
    pub fn unsent(&mut self, frame: TradeUp, why: TradeLeaseCommandLedgerWhy) -> Vec<Event> {
        match frame {
            TradeUp::Submit { order, .. } => {
                vec![rejected(&order, Some(OrderRefusalId::NotSent), submit_reason(why))]
            }
            TradeUp::SubmitBatch { orders, .. } => orders
                .iter()
                .map(|o| rejected(o, Some(OrderRefusalId::NotSent), submit_reason(why)))
                .collect(),
            TradeUp::Cancel { order_client_id, intent, .. } => {
                if intent.may_be_shed() || why == TradeLeaseCommandLedgerWhy::Closed {
                    vec![cancel_rejected(
                        &order_client_id,
                        Some(OrderRefusalId::NotSent),
                        cancel_reason(why),
                    )]
                } else {
                    self.hold(TradeUp::Cancel { command_sequence: 0, order_client_id, intent });
                    Vec::new()
                }
            }
            TradeUp::CancelBatch { order_client_ids, intent, .. } => {
                if intent.may_be_shed() || why == TradeLeaseCommandLedgerWhy::Closed {
                    order_client_ids
                        .iter()
                        .map(|c| {
                            cancel_rejected(c, Some(OrderRefusalId::NotSent), cancel_reason(why))
                        })
                        .collect()
                } else {
                    self.hold(TradeUp::CancelBatch {
                        command_sequence: 0,
                        order_client_ids,
                        intent,
                    });
                    Vec::new()
                }
            }
            TradeUp::Modify { order, .. } => {
                vec![modify_rejected(&order, Some(OrderRefusalId::NotSent), modify_reason(why))]
            }
            // A confirm is a prod: the venue's own terminal (or the watchdog's last resort) is the
            // backstop, and a prod sent minutes late proves nothing.
            TradeUp::Confirm { .. } => Vec::new(),
            TradeUp::AdoptRestored { pairs, venue_ids, .. } => {
                if why != TradeLeaseCommandLedgerWhy::Closed {
                    self.hold(TradeUp::AdoptRestored { command_sequence: 0, pairs, venue_ids });
                }
                Vec::new()
            }
            TradeUp::AdapterStart(_)
            | TradeUp::WireIds { .. }
            | TradeUp::ResolveOrders { .. }
            | TradeUp::IdentityRecord { .. }
            | TradeUp::Reconcile { .. }
            | TradeUp::Ping
            | TradeUp::Detach { .. }
            | TradeUp::TradeStatus { .. }
            | TradeUp::SymbolProperties { .. }
            | TradeUp::VenueRead { .. } => Vec::new(),
        }
    }

    /// Hold a cancel that may not be shed for the next link, as a NEVER-SENT one is: the cancel an epoch change
    /// left IN DOUBT (`TradeLeaseShared::take_cancels_in_doubt`). One already held is not held twice.
    pub fn hold_cancel(&mut self, order_client_id: String, intent: OrderCancelIntent) {
        self.hold(TradeUp::Cancel { command_sequence: 0, order_client_id, intent });
    }

    /// Hold a frame that must not be shed. Repeats are folded: a cancel already held for an order client id is not
    /// queued twice (a risk-off sweep repeats every pass of an outage), and teaching merges into one
    /// frame.
    fn hold(&mut self, frame: TradeUp) {
        match frame {
            TradeUp::Cancel { order_client_id, intent, .. } => {
                if self.held_cancels.insert(order_client_id.clone()) {
                    self.held.push(TradeUp::Cancel {
                        command_sequence: 0,
                        order_client_id,
                        intent,
                    });
                }
            }
            TradeUp::CancelBatch { order_client_ids, intent, .. } => {
                let fresh: Vec<String> = order_client_ids
                    .into_iter()
                    .filter(|c| self.held_cancels.insert(c.clone()))
                    .collect();
                if !fresh.is_empty() {
                    self.held.push(TradeUp::CancelBatch {
                        command_sequence: 0,
                        order_client_ids: fresh,
                        intent,
                    });
                }
            }
            TradeUp::AdoptRestored { pairs, venue_ids, .. } => {
                let existing = self.held.iter_mut().find_map(|f| match f {
                    TradeUp::AdoptRestored { pairs, venue_ids, .. } => Some((pairs, venue_ids)),
                    _ => None,
                });
                match existing {
                    Some((held, held_ids)) => {
                        for p in pairs {
                            if !held.contains(&p) {
                                held.push(p);
                            }
                        }
                        // One id per order client id, the fresher teaching's.
                        for v in venue_ids {
                            held_ids.retain(|(order_client_id, _)| *order_client_id != v.0);
                            held_ids.push(v);
                        }
                    }
                    None => self.held.push(TradeUp::AdoptRestored {
                        command_sequence: 0,
                        pairs,
                        venue_ids,
                    }),
                }
            }
            // Only the three kinds above are ever held.
            other => debug_assert!(false, "a {other:?} frame is never held"),
        }
    }

    /// **A link came up**: resolve every Written frame against the daemon's answer and release the held
    /// ones. See the module doc for the two cases (`previous_epoch` is the epoch the previous connection
    /// reported, `None` on the first).
    ///
    /// After an EPOCH CHANGE the daemon is a new process with no adapter: a held frame flushed now would
    /// land on a session with none, be answered with an advisory (or, for the teaching, nothing) and be
    /// counted delivered. So they are kept (`flush` stays empty) until [`TradeLeaseCommandLedger::adapter_answered`], which
    /// the link calls when the daemon answers an `AdapterStart`. A Written cancel that may not be shed
    /// (risk-off, unclassified) is held again for the same flush: cancels are idempotent, and "never shed"
    /// is the rule of a cancel the dead-man sent.
    pub fn resumed(
        &mut self,
        previous_epoch: Option<u64>,
        opened: &TradeOpened,
    ) -> TradeLeaseCommandLedgerResume {
        let same_epoch = previous_epoch == Some(opened.gateway_epoch);
        let epoch_changed = previous_epoch.is_some_and(|e| e != opened.gateway_epoch);
        let written = std::mem::take(&mut self.written);
        let mut out = TradeLeaseCommandLedgerResume::default();
        for (cseq, frame) in written {
            if same_epoch {
                if cseq <= opened.received_through_command_sequence {
                    continue; // RECEIVED: events and the watchdog take over.
                }
                out.events.extend(self.unsent(frame, TradeLeaseCommandLedgerWhy::NeverReceived));
                continue;
            }
            // A new epoch: the watermark is gone. UNKNOWN stays unknown.
            match frame {
                TradeUp::Submit { order, .. } => out.resolve.push(order.order_client_id),
                TradeUp::SubmitBatch { orders, .. } => {
                    out.resolve.extend(orders.into_iter().map(|o| o.order_client_id));
                }
                // The teaching is idempotent and names state, not an order: re-teach it.
                adopt @ TradeUp::AdoptRestored { .. } => {
                    self.hold(adopt);
                }
                // A cancel that may not be shed is idempotent by content: send it again once the adapter
                // is back. A routine one is not sent again, and says its outcome is unknown.
                TradeUp::Cancel { order_client_id, intent, .. } => {
                    if intent.may_be_shed() {
                        out.events.push(cancel_rejected(
                            &order_client_id,
                            Some(OrderRefusalId::OutcomeUnknown),
                            OUTCOME_UNKNOWN_CANCEL_REASON,
                        ));
                    } else {
                        self.hold(TradeUp::Cancel { command_sequence: 0, order_client_id, intent });
                    }
                }
                TradeUp::CancelBatch { order_client_ids, intent, .. } => {
                    if intent.may_be_shed() {
                        out.events.extend(order_client_ids.iter().map(|c| {
                            cancel_rejected(
                                c,
                                Some(OrderRefusalId::OutcomeUnknown),
                                OUTCOME_UNKNOWN_CANCEL_REASON,
                            )
                        }));
                    } else {
                        self.hold(TradeUp::CancelBatch {
                            command_sequence: 0,
                            order_client_ids,
                            intent,
                        });
                    }
                }
                TradeUp::Modify { order, .. } => {
                    out.events.push(modify_rejected(
                        &order,
                        Some(OrderRefusalId::OutcomeUnknown),
                        OUTCOME_UNKNOWN_MODIFY_REASON,
                    ));
                }
                // A Written confirm is a prod: nothing to synthesize, nothing to resolve.
                TradeUp::Confirm { .. }
                | TradeUp::AdapterStart(_)
                | TradeUp::WireIds { .. }
                | TradeUp::ResolveOrders { .. }
                | TradeUp::IdentityRecord { .. }
                | TradeUp::Reconcile { .. }
                | TradeUp::Ping
                | TradeUp::Detach { .. }
                | TradeUp::TradeStatus { .. }
                | TradeUp::SymbolProperties { .. }
                | TradeUp::VenueRead { .. } => {}
            }
        }
        if epoch_changed {
            self.awaiting_adapter = true;
        }
        if !self.gated() {
            out.flush = self.take_held();
        }
        out
    }

    /// Whether the held frames are still waiting for the adapter or for its teaching.
    pub fn gated(&self) -> bool {
        self.awaiting_adapter || self.awaiting_teaching
    }

    /// Everything held, TEACHING FIRST and each kind in the order it was held: a cancel released ahead of
    /// the teaching it needs fails with "no venue order id" on a venue whose map a restart emptied.
    fn take_held(&mut self) -> Vec<TradeUp> {
        self.held_cancels.clear();
        let (mut out, cancels): (Vec<TradeUp>, Vec<TradeUp>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|f| matches!(f, TradeUp::AdoptRestored { .. }));
        out.extend(cancels);
        out
    }

    /// A cancel that arrives while the held frames are gated joins them (behind the teaching): written now
    /// it would reach a daemon with no adapter, or one not yet taught the order, and be spent on an
    /// advisory. Anything else (and any cancel that may be shed) is handed back untouched.
    pub fn defer(&mut self, frame: TradeUp) -> Option<TradeUp> {
        if !self.gated() {
            return Some(frame);
        }
        match frame {
            TradeUp::Cancel { order_client_id, intent, .. } if !intent.may_be_shed() => {
                self.hold(TradeUp::Cancel { command_sequence: 0, order_client_id, intent });
                None
            }
            TradeUp::CancelBatch { order_client_ids, intent, .. } if !intent.may_be_shed() => {
                self.hold(TradeUp::CancelBatch { command_sequence: 0, order_client_ids, intent });
                None
            }
            other => Some(other),
        }
    }

    /// The daemon answered an `AdapterStart` (the adapter is up, or it said why not): release what an
    /// epoch change held back for it, the TEACHING first and the cancels behind it. Empty when nothing was
    /// waiting.
    ///
    /// `hold` is the caller's word that it will teach the new adapter the orders minted since boot right
    /// after the answer (`TradeLease::restart_adapter`): then only the held teaching is released and the
    /// held cancels stay held until [`TradeLeaseCommandLedger::release_held`], so those frames reach the daemon before any
    /// cancel. A plain `start_adapter` (`hold` false) teaches nothing of its own, so waiting would only
    /// strand the cancels: they follow the held teaching at once.
    pub fn adapter_answered(&mut self, hold: bool) -> Vec<TradeUp> {
        if !self.awaiting_adapter {
            return Vec::new();
        }
        self.awaiting_adapter = false;
        if !hold {
            self.awaiting_teaching = false;
            return self.take_held();
        }
        let (teaching, cancels): (Vec<TradeUp>, Vec<TradeUp>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|f| matches!(f, TradeUp::AdoptRestored { .. }));
        self.held = cancels;
        self.awaiting_teaching = !self.held.is_empty();
        teaching
    }

    /// The caller has taught what it means to teach: release the held cancels (and any teaching held
    /// since, first). Nothing while a newer epoch change still waits for its adapter.
    pub fn release_held(&mut self) -> Vec<TradeUp> {
        if self.awaiting_adapter {
            return Vec::new();
        }
        self.awaiting_teaching = false;
        self.take_held()
    }

    /// The caller GIVES UP on the adapter (a refusal asking again cannot change, or it has not started in
    /// the time the cancels may wait): release everything, the adapter's answer or not. The cancels reach
    /// a daemon that may have no adapter and are answered with an advisory each, which is the point: a
    /// held risk-off cancel is answered, never left waiting for ever.
    pub fn release_held_anyway(&mut self) -> Vec<TradeUp> {
        self.awaiting_adapter = false;
        self.release_held()
    }

    /// The lease is closed for good: every held frame and every Written-unacknowledged frame gets its last
    /// word, by what became of the connection (`fate`).
    ///
    /// * A held cancel gets the non-terminal advisory (the order, if live, is still resting); held teaching
    ///   is dropped.
    /// * A frame that was Written and never acknowledged: under [`TradeLeaseCommandLedgerFate::Unprocessed`] the daemon provably
    ///   never read it (it ended the lease at an earlier frame, after acknowledging everything before), so
    ///   it gets the NEVER-SENT disposition (a submit a terminal reject, a cancel or a modify an advisory).
    ///   Under [`TradeLeaseCommandLedgerFate::Unknown`] the daemon may have executed it: nothing is synthesized (a false terminal
    ///   would be worse), its order client ids are RETURNED so the link names them and marks them UNKNOWN.
    pub fn closed(&mut self, fate: TradeLeaseCommandLedgerFate) -> TradeLeaseCommandLedgerClose {
        self.held_cancels.clear();
        let held = std::mem::take(&mut self.held);
        let mut out = TradeLeaseCommandLedgerClose::default();
        for frame in held {
            out.events.extend(self.unsent(frame, TradeLeaseCommandLedgerWhy::Closed));
        }
        for (_, frame) in std::mem::take(&mut self.written) {
            match fate {
                TradeLeaseCommandLedgerFate::Unprocessed => {
                    out.events.extend(self.unsent(frame, TradeLeaseCommandLedgerWhy::Closed))
                }
                TradeLeaseCommandLedgerFate::Unknown => match frame {
                    TradeUp::Submit { order, .. } => {
                        out.unknown_submits.push(order.order_client_id)
                    }
                    TradeUp::SubmitBatch { orders, .. } => {
                        out.unknown_submits.extend(orders.into_iter().map(|o| o.order_client_id));
                    }
                    TradeUp::Cancel { order_client_id, .. } => {
                        out.unknown_cancels.push(order_client_id)
                    }
                    TradeUp::CancelBatch { order_client_ids, .. } => {
                        out.unknown_cancels.extend(order_client_ids)
                    }
                    // A modify, a confirm or teaching names no order's fate: reconcile finds the state.
                    TradeUp::Modify { .. }
                    | TradeUp::Confirm { .. }
                    | TradeUp::AdoptRestored { .. }
                    | TradeUp::AdapterStart(_)
                    | TradeUp::WireIds { .. }
                    | TradeUp::ResolveOrders { .. }
                    | TradeUp::IdentityRecord { .. }
                    | TradeUp::Reconcile { .. }
                    | TradeUp::Ping
                    | TradeUp::Detach { .. }
                    | TradeUp::TradeStatus { .. }
                    | TradeUp::SymbolProperties { .. }
                    | TradeUp::VenueRead { .. } => {}
                },
            }
        }
        out
    }
}

fn submit_reason(why: TradeLeaseCommandLedgerWhy) -> &'static str {
    match why {
        TradeLeaseCommandLedgerWhy::NeverSent => NEVER_SENT_SUBMIT_REASON,
        TradeLeaseCommandLedgerWhy::Closed => LEASE_CLOSED_SUBMIT_REASON,
        TradeLeaseCommandLedgerWhy::NeverReceived => NEVER_RECEIVED_SUBMIT_REASON,
        TradeLeaseCommandLedgerWhy::TooLarge => TOO_LARGE_SUBMIT_REASON,
    }
}

fn cancel_reason(why: TradeLeaseCommandLedgerWhy) -> &'static str {
    match why {
        TradeLeaseCommandLedgerWhy::NeverSent | TradeLeaseCommandLedgerWhy::TooLarge => {
            NEVER_SENT_CANCEL_REASON
        }
        TradeLeaseCommandLedgerWhy::NeverReceived => NEVER_RECEIVED_CANCEL_REASON,
        TradeLeaseCommandLedgerWhy::Closed => LEASE_CLOSED_CANCEL_REASON,
    }
}

fn modify_reason(why: TradeLeaseCommandLedgerWhy) -> &'static str {
    match why {
        TradeLeaseCommandLedgerWhy::NeverSent | TradeLeaseCommandLedgerWhy::TooLarge => {
            NEVER_SENT_MODIFY_REASON
        }
        TradeLeaseCommandLedgerWhy::Closed => LEASE_CLOSED_MODIFY_REASON,
        TradeLeaseCommandLedgerWhy::NeverReceived => NEVER_RECEIVED_MODIFY_REASON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::OrderCancelIntent;

    fn order(order_client_id: &str) -> OrderRequest {
        OrderRequest {
            order_client_id: order_client_id.to_string(),
            timestamp: 7,
            ..Default::default()
        }
    }

    fn submit(order_client_id: &str) -> TradeUp {
        TradeUp::Submit { command_sequence: 0, order: order(order_client_id) }
    }

    fn cancel(order_client_id: &str, intent: OrderCancelIntent) -> TradeUp {
        TradeUp::Cancel {
            command_sequence: 0,
            order_client_id: order_client_id.to_string(),
            intent,
        }
    }

    fn opened(epoch: u64, through: u64) -> TradeOpened {
        TradeOpened {
            gateway_epoch: epoch,
            lease_epoch: 1,
            received_through_command_sequence: through,
            project_token: String::new(),
            heartbeat_ms: 2_000,
            event_ring_floor: Some(0),
        }
    }

    fn terminal_order_client_ids(events: &[Event]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::OrderRejected(r) => Some(r.order_client_id.clone()),
                _ => None,
            })
            .collect()
    }

    /// **Every `TradeUp` variant, read and stamped**: the seven order commands carry a `cseq` that
    /// `command_sequence_of` reads and `set_command_sequence` overwrites (answering `true`); every other frame reads `None`, is
    /// left byte-for-byte alone and answers `false`. One row per variant, so a variant moved to the wrong
    /// arm of `command_command_sequence!` turns one row red.
    #[test]
    fn every_variant_is_read_and_stamped_on_its_own_side_of_the_command_line() {
        use crate::proto::{AdapterStart, DetachPhase, ReconcileKind};
        let start = AdapterStart {
            rid: 1,
            symbol: "BTCUSDT".into(),
            declared_legs: Vec::new(),
            reconcile_enabled: false,
            takes_trigger: false,
            risk: None,
            market_slippage: None,
            halt_admit: Default::default(),
            withhold: None,
            account_leverage: None,
            read_account_currency: false,
            route_key: "binance".into(),
        };
        let commands = [
            TradeUp::Submit { command_sequence: 41, order: order("a") },
            TradeUp::SubmitBatch { command_sequence: 41, orders: vec![order("a"), order("b")] },
            TradeUp::Cancel {
                command_sequence: 41,
                order_client_id: "a".into(),
                intent: OrderCancelIntent::default(),
            },
            TradeUp::CancelBatch {
                command_sequence: 41,
                order_client_ids: vec!["a".into()],
                intent: OrderCancelIntent::default(),
            },
            TradeUp::Modify {
                command_sequence: 41,
                order: order("a"),
                new_qty: Some(2.0),
                new_price: None,
                new_trigger_price: None,
            },
            TradeUp::Confirm { command_sequence: 41, order_client_id: "a".into() },
            TradeUp::AdoptRestored {
                command_sequence: 41,
                pairs: vec![("a".into(), "BTCUSDT".into())],
                venue_ids: Vec::new(),
            },
        ];
        for frame in commands {
            assert_eq!(command_sequence_of(&frame), Some(41), "{frame:?}");
            let mut stamped = frame.clone();
            assert!(set_command_sequence(&mut stamped, 42), "{frame:?}");
            assert_eq!(command_sequence_of(&stamped), Some(42), "{frame:?}");
        }
        let others = [
            TradeUp::AdapterStart(start),
            TradeUp::WireIds { rid: 1, order_client_ids: vec!["a".into()] },
            TradeUp::ResolveOrders { rid: 1, order_client_ids: vec!["a".into()] },
            TradeUp::IdentityRecord { rid: 1 },
            TradeUp::Reconcile { rid: 1, kind: ReconcileKind::Orders { since: 0 } },
            TradeUp::Ping,
            TradeUp::Detach { phase: DetachPhase::Begin },
            TradeUp::TradeStatus { rid: 1 },
            TradeUp::SymbolProperties {
                rid: 1,
                symbol: "BTCUSDT".into(),
                probe: true,
                quoted: false,
                forget: false,
                modes: true,
            },
            TradeUp::VenueRead { rid: 1, kind: vike_model::VenueReadKind::Balance },
        ];
        for frame in others {
            assert_eq!(command_sequence_of(&frame), None, "{frame:?}");
            let mut untouched = frame.clone();
            assert!(!set_command_sequence(&mut untouched, 42), "{frame:?}");
            assert_eq!(untouched, frame, "a frame that is not a command is left alone");
        }
    }

    /// Write `frame` through the ledger the way the link thread does.
    fn write(ledger: &mut TradeLeaseCommandLedger, mut frame: TradeUp) -> u64 {
        let n = ledger.next_command_sequence();
        assert!(set_command_sequence(&mut frame, n));
        ledger.wrote(frame);
        n
    }

    #[test]
    fn a_queued_submit_is_a_terminal_reject_and_never_a_silence() {
        let mut ledger = TradeLeaseCommandLedger::default();
        let events = ledger.unsent(submit("c1"), TradeLeaseCommandLedgerWhy::NeverSent);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, "c1");
                assert_eq!(r.reason.as_str(), NEVER_SENT_SUBMIT_REASON);
                assert_eq!(r.timestamp, 7, "the reject carries the request's own stamp");
            }
            other => panic!("a never-sent submit must be a terminal reject, got {other:?}"),
        }
    }

    #[test]
    fn a_never_sent_batch_rejects_every_order_in_it() {
        let mut ledger = TradeLeaseCommandLedger::default();
        let batch = TradeUp::SubmitBatch {
            command_sequence: 0,
            orders: vec![order("a"), order("b"), order("c")],
        };
        assert_eq!(
            terminal_order_client_ids(&ledger.unsent(batch, TradeLeaseCommandLedgerWhy::NeverSent)),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn a_modify_and_a_routine_cancel_get_non_terminal_advisories() {
        let mut ledger = TradeLeaseCommandLedger::default();
        let modify = TradeUp::Modify {
            command_sequence: 0,
            order: order("m"),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: None,
        };
        let before = vike_model::now_ms();
        match ledger.unsent(modify, TradeLeaseCommandLedgerWhy::NeverSent).as_slice() {
            [Event::OrderModifyRejected(r)] => {
                assert_eq!(r.order_client_id, "m");
                assert_eq!(r.reason.as_str(), NEVER_SENT_MODIFY_REASON);
                // the owner's 2026-11-12 ruling: stamped when the refusal is made, never at the
                // order's own creation (its 7) and never 0
                assert!(r.timestamp >= before, "{}", r.timestamp);
            }
            other => panic!("a modify that never left is a non-terminal advisory: {other:?}"),
        }
        match ledger
            .unsent(cancel("k", OrderCancelIntent::Routine), TradeLeaseCommandLedgerWhy::NeverSent)
            .as_slice()
        {
            [Event::OrderCancelRejected(r)] => assert_eq!(r.order_client_id, "k"),
            other => panic!("a routine cancel may be shed, with an advisory: {other:?}"),
        }
        assert_eq!(ledger.held_len(), 0);
    }

    #[test]
    fn a_risk_off_or_unclassified_cancel_is_never_shed_it_is_held_and_flushed_at_reconnect() {
        for intent in [OrderCancelIntent::RiskOff, OrderCancelIntent::Unspecified] {
            let mut ledger = TradeLeaseCommandLedger::default();
            assert!(
                ledger
                    .unsent(cancel("k", intent), TradeLeaseCommandLedgerWhy::NeverSent)
                    .is_empty()
            );
            // The sweep repeats during the outage: each order client id is held once.
            assert!(
                ledger
                    .unsent(cancel("k", intent), TradeLeaseCommandLedgerWhy::NeverSent)
                    .is_empty()
            );
            assert!(
                ledger
                    .unsent(cancel("j", intent), TradeLeaseCommandLedgerWhy::NeverSent)
                    .is_empty()
            );
            assert_eq!(ledger.held_len(), 2);
            let resumed = ledger.resumed(None, &opened(1, 0));
            assert!(resumed.events.is_empty());
            let order_client_ids: Vec<&str> = resumed
                .flush
                .iter()
                .map(|f| match f {
                    TradeUp::Cancel { order_client_id, .. } => order_client_id.as_str(),
                    other => panic!("only cancels were held: {other:?}"),
                })
                .collect();
            assert_eq!(order_client_ids, ["k", "j"], "flushed in the order they were held");
            assert_eq!(ledger.held_len(), 0);
        }
    }

    #[test]
    fn a_confirm_is_a_prod_and_is_dropped_not_held() {
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    TradeUp::Confirm { command_sequence: 0, order_client_id: "c".into() },
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert_eq!(ledger.held_len(), 0);
    }

    #[test]
    fn teaching_is_held_merged_and_survives_an_epoch_change() {
        let mut ledger = TradeLeaseCommandLedger::default();
        let adopt = |pairs: &[(&str, &str)]| TradeUp::AdoptRestored {
            command_sequence: 0,
            pairs: pairs.iter().map(|(a, b)| ((*a).to_string(), (*b).to_string())).collect(),
            venue_ids: Vec::new(),
        };
        let with_ids = |ids: &[(&str, &str)]| TradeUp::AdoptRestored {
            command_sequence: 0,
            pairs: vec![("a".into(), "BTC".into())],
            venue_ids: ids.iter().map(|(a, b)| ((*a).to_string(), (*b).to_string())).collect(),
        };
        assert!(
            ledger.unsent(adopt(&[("a", "BTC")]), TradeLeaseCommandLedgerWhy::NeverSent).is_empty()
        );
        assert!(
            ledger
                .unsent(adopt(&[("a", "BTC"), ("b", "ETH")]), TradeLeaseCommandLedgerWhy::NeverSent)
                .is_empty()
        );
        // The venue ids merge too, one per order client id, the fresher teaching's.
        assert!(
            ledger
                .unsent(with_ids(&[("a", "v-old")]), TradeLeaseCommandLedgerWhy::NeverSent)
                .is_empty()
        );
        assert!(
            ledger
                .unsent(with_ids(&[("a", "v-new")]), TradeLeaseCommandLedgerWhy::NeverSent)
                .is_empty()
        );
        assert_eq!(ledger.held_len(), 1, "teaching merges into one frame");
        let resumed = ledger.resumed(None, &opened(1, 0));
        match resumed.flush.as_slice() {
            [TradeUp::AdoptRestored { pairs, venue_ids, .. }] => {
                assert_eq!(pairs.len(), 2);
                assert_eq!(venue_ids, &vec![("a".to_string(), "v-new".to_string())]);
            }
            other => panic!("one merged teaching frame expected: {other:?}"),
        }
        // A Written teaching frame is re-taught after an epoch change, once the daemon has an adapter
        // again (a teaching sent to a daemon with none is dropped without a word).
        let n = write(&mut ledger, adopt(&[("z", "SOL")]));
        assert_eq!(n, 1);
        let resumed = ledger.resumed(Some(1), &opened(2, 0));
        assert!(
            resumed.flush.is_empty(),
            "held for the adapter, not spent on a daemon without one"
        );
        assert!(matches!(
            ledger.adapter_answered(false).as_slice(),
            [TradeUp::AdoptRestored { .. }]
        ));
        assert!(ledger.adapter_answered(false).is_empty(), "released once");
    }

    #[test]
    fn command_sequence_is_per_client_and_a_failed_write_burns_nothing() {
        let mut ledger = TradeLeaseCommandLedger::default();
        assert_eq!(ledger.next_command_sequence(), 1);
        // A write that FAILED is never `wrote`n: the next frame reuses the number.
        assert_eq!(ledger.next_command_sequence(), 1);
        assert_eq!(write(&mut ledger, submit("a")), 1);
        assert_eq!(write(&mut ledger, submit("b")), 2);
        assert_eq!(ledger.next_command_sequence(), 3);
        assert_eq!(ledger.written_cseqs(), [1, 2]);
        // It survives the connection: the counter is the client's, not the socket's.
        let _ = ledger.resumed(None, &opened(1, 2));
        assert_eq!(ledger.next_command_sequence(), 3);
    }

    #[test]
    fn a_command_ack_retires_everything_through_it() {
        let mut ledger = TradeLeaseCommandLedger::default();
        for c in ["a", "b", "c"] {
            write(&mut ledger, submit(c));
        }
        ledger.acked(2);
        assert_eq!(ledger.written_cseqs(), [3]);
        ledger.acked(0);
        assert_eq!(ledger.in_flight(), 1, "an old ack retires nothing new");
        ledger.acked(u64::MAX);
        assert_eq!(ledger.in_flight(), 0);
    }

    #[test]
    fn same_epoch_the_watermark_splits_received_from_never_received() {
        let mut ledger = TradeLeaseCommandLedger::default();
        for c in ["r1", "r2", "lost3", "lost4"] {
            write(&mut ledger, submit(c));
        }
        // The daemon read 1 and 2 and never saw 3 or 4.
        let resumed = ledger.resumed(Some(9), &opened(9, 2));
        assert_eq!(terminal_order_client_ids(&resumed.events), ["lost3", "lost4"]);
        for e in &resumed.events {
            let Event::OrderRejected(r) = e else { panic!("{e:?}") };
            assert_eq!(r.reason.as_str(), NEVER_RECEIVED_SUBMIT_REASON);
        }
        assert!(resumed.resolve.is_empty(), "inside one epoch nothing is UNKNOWN");
        assert_eq!(ledger.in_flight(), 0);
    }

    #[test]
    fn same_epoch_a_never_received_risk_off_cancel_is_held_not_shed() {
        let mut ledger = TradeLeaseCommandLedger::default();
        write(&mut ledger, cancel("keep", OrderCancelIntent::RiskOff));
        write(&mut ledger, cancel("shed", OrderCancelIntent::Routine));
        write(
            &mut ledger,
            TradeUp::Modify {
                command_sequence: 0,
                order: order("mod"),
                new_qty: None,
                new_price: Some(1.0),
                new_trigger_price: None,
            },
        );
        let resumed = ledger.resumed(Some(4), &opened(4, 0));
        assert!(matches!(
            resumed.flush.as_slice(),
            [TradeUp::Cancel { order_client_id, intent: OrderCancelIntent::RiskOff, .. }] if order_client_id == "keep"
        ));
        let kinds: Vec<&str> = resumed
            .events
            .iter()
            .map(|e| match e {
                Event::OrderCancelRejected(r) => {
                    assert_eq!(r.reason.as_str(), NEVER_RECEIVED_CANCEL_REASON);
                    "cancel"
                }
                Event::OrderModifyRejected(_) => "modify",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["cancel", "modify"]);
    }

    #[test]
    fn an_epoch_change_leaves_written_submits_unknown_and_synthesizes_nothing() {
        let mut ledger = TradeLeaseCommandLedger::default();
        write(&mut ledger, submit("u1"));
        write(
            &mut ledger,
            TradeUp::SubmitBatch { command_sequence: 0, orders: vec![order("u2"), order("u3")] },
        );
        write(&mut ledger, cancel("c", OrderCancelIntent::RiskOff));
        write(
            &mut ledger,
            TradeUp::Modify {
                command_sequence: 0,
                order: order("m"),
                new_qty: Some(1.0),
                new_price: None,
                new_trigger_price: None,
            },
        );
        // Even a watermark that WOULD cover them means nothing across an epoch change.
        let resumed = ledger.resumed(Some(1), &opened(2, 99));
        assert!(
            terminal_order_client_ids(&resumed.events).is_empty(),
            "UNKNOWN is never synthesized into a terminal"
        );
        assert!(
            matches!(
                resumed.events.as_slice(),
                [Event::OrderModifyRejected(r)]
                    if r.order_client_id == "m" && r.reason.as_str() == OUTCOME_UNKNOWN_MODIFY_REASON
            ),
            "the modify says its outcome is unknown: {:?}",
            resumed.events
        );
        assert_eq!(resumed.resolve, ["u1", "u2", "u3"]);
        assert!(resumed.flush.is_empty(), "a Written cancel is not re-sent");
        assert_eq!(ledger.in_flight(), 0);
    }

    #[test]
    fn the_first_connection_has_no_previous_epoch_and_nothing_to_resolve() {
        let mut ledger = TradeLeaseCommandLedger::default();
        let resumed = ledger.resumed(None, &opened(5, 0));
        assert!(
            resumed.events.is_empty() && resumed.resolve.is_empty() && resumed.flush.is_empty()
        );
    }

    #[test]
    fn a_closed_lease_gives_every_held_cancel_its_last_word() {
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        let batch = TradeUp::CancelBatch {
            command_sequence: 0,
            order_client_ids: vec!["h2".into(), "h3".into()],
            intent: OrderCancelIntent::Unspecified,
        };
        assert!(ledger.unsent(batch, TradeLeaseCommandLedgerWhy::NeverSent).is_empty());
        assert!(
            ledger
                .unsent(
                    TradeUp::AdoptRestored {
                        command_sequence: 0,
                        pairs: vec![("t".into(), "BTC".into())],
                        venue_ids: Vec::new(),
                    },
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        let events = ledger.closed(TradeLeaseCommandLedgerFate::Unknown).events;
        let ids: Vec<&str> = events
            .iter()
            .map(|e| match e {
                Event::OrderCancelRejected(r) => {
                    assert_eq!(r.reason.as_str(), LEASE_CLOSED_CANCEL_REASON);
                    r.order_client_id.as_str()
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, ["h1", "h2", "h3"], "teaching is dropped, no cancel vanishes");
        assert_eq!(ledger.held_len(), 0);
    }

    #[test]
    fn an_epoch_change_holds_every_held_frame_for_the_adapter_and_resends_a_written_risk_off_cancel()
     {
        let mut ledger = TradeLeaseCommandLedger::default();
        write(&mut ledger, cancel("w-risk", OrderCancelIntent::RiskOff));
        write(&mut ledger, cancel("w-routine", OrderCancelIntent::Routine));
        write(&mut ledger, cancel("w-plain", OrderCancelIntent::Unspecified));
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );

        let resumed = ledger.resumed(Some(1), &opened(2, 0));
        assert!(resumed.flush.is_empty(), "nothing is spent on a daemon without an adapter");
        assert!(
            matches!(
                resumed.events.as_slice(),
                [Event::OrderCancelRejected(r)]
                    if r.order_client_id == "w-routine" && r.reason.as_str() == OUTCOME_UNKNOWN_CANCEL_REASON
            ),
            "only the routine cancel, which is not sent again, says its outcome is unknown: {:?}",
            resumed.events
        );
        // The link's acknowledged-and-unanswered cancels join them; one already held is held once.
        ledger.hold_cancel("w-risk".into(), OrderCancelIntent::RiskOff);
        ledger.hold_cancel("acked".into(), OrderCancelIntent::Unspecified);
        let released = ledger.adapter_answered(false);
        assert!(!ledger.gated(), "a plain answer releases everything: nothing waits for a caller");
        let order_client_ids: Vec<&str> = released
            .iter()
            .filter_map(|f| match f {
                TradeUp::Cancel { order_client_id, .. } => Some(order_client_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            order_client_ids,
            ["h1", "w-risk", "w-plain", "acked"],
            "held first, then the Written ones that may not be shed, then the acknowledged ones"
        );

        // A second reconnect on the same (new) epoch, before any adapter answer, keeps waiting.
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert!(ledger.resumed(Some(1), &opened(2, 0)).flush.is_empty());
        assert!(ledger.resumed(Some(2), &opened(2, 0)).flush.is_empty(), "still no adapter");
        assert_eq!(ledger.adapter_answered(false).len(), 1);

        // No epoch change: the adapter is alive, the held frames go at once.
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    cancel("h2", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert_eq!(ledger.resumed(Some(2), &opened(2, 0)).flush.len(), 1);
        assert!(ledger.adapter_answered(false).is_empty());
    }

    #[test]
    fn after_an_epoch_change_the_teaching_goes_out_before_any_held_cancel() {
        let adopt = |order_client_id: &str| TradeUp::AdoptRestored {
            command_sequence: 0,
            pairs: vec![(order_client_id.to_string(), "BTC".to_string())],
            venue_ids: Vec::new(),
        };
        // The review's shape: cancels held while the link was down, the boot teaching Written earlier
        // (so `resumed` re-holds it AFTER the cancels).
        let mut ledger = TradeLeaseCommandLedger::default();
        write(&mut ledger, adopt("boot"));
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert!(
            ledger
                .unsent(
                    cancel("h2", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert!(ledger.resumed(Some(1), &opened(2, 0)).flush.is_empty());

        // The daemon's answer releases the teaching and ONLY the teaching.
        let first = ledger.adapter_answered(true);
        assert!(
            matches!(first.as_slice(), [TradeUp::AdoptRestored { pairs, .. }] if pairs[0].0 == "boot"),
            "the boot teaching leads: {first:?}"
        );
        // A cancel arriving now (the dead-man's second sweep) queues behind, it is not written.
        assert!(ledger.defer(cancel("h3", OrderCancelIntent::RiskOff)).is_none());
        // A routine cancel and an order are not deferred.
        assert!(ledger.defer(cancel("r", OrderCancelIntent::Routine)).is_some());
        assert!(ledger.defer(submit("s")).is_some());
        // The caller's own teaching is not a cancel: it is written at once.
        assert!(ledger.defer(adopt("minted")).is_some());

        let rest = ledger.release_held();
        let order_client_ids: Vec<&str> = rest
            .iter()
            .map(|f| match f {
                TradeUp::Cancel { order_client_id, .. } => order_client_id.as_str(),
                other => panic!("only cancels remain: {other:?}"),
            })
            .collect();
        assert_eq!(order_client_ids, ["h1", "h2", "h3"]);
        assert!(!ledger.gated());
        assert!(ledger.defer(cancel("late", OrderCancelIntent::RiskOff)).is_some(), "gate is open");
    }

    #[test]
    fn a_plain_answer_releases_the_teaching_and_then_every_cancel_at_once() {
        // A caller that did not say it would teach (a plain `start_adapter`) strands nothing: the held
        // frames all go at the answer, the teaching still ahead of the cancels it was held behind.
        let mut ledger = TradeLeaseCommandLedger::default();
        write(&mut ledger, cancel("w1", OrderCancelIntent::RiskOff));
        write(
            &mut ledger,
            TradeUp::AdoptRestored {
                command_sequence: 0,
                pairs: vec![("boot".into(), "BTC".into())],
                venue_ids: Vec::new(),
            },
        );
        assert!(ledger.resumed(Some(1), &opened(2, 0)).flush.is_empty());
        let all = ledger.adapter_answered(false);
        assert!(!ledger.gated(), "nothing waits for a caller that said nothing");
        assert!(matches!(
            all.as_slice(),
            [TradeUp::AdoptRestored { .. }, TradeUp::Cancel { order_client_id, .. }] if order_client_id == "w1"
        ));
    }

    #[test]
    fn a_release_waits_for_a_newer_epochs_adapter_and_teaching_held_since_goes_first() {
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert!(ledger.resumed(Some(1), &opened(2, 0)).flush.is_empty());
        assert!(ledger.adapter_answered(true).is_empty());
        // The gatewayhub restarted AGAIN before the caller released: the new adapter has not answered.
        assert!(ledger.resumed(Some(2), &opened(3, 0)).flush.is_empty());
        assert!(ledger.release_held().is_empty(), "no adapter yet: nothing is spent");
        assert!(ledger.gated());
        assert!(ledger.adapter_answered(true).is_empty());
        // Teaching held in the meantime still precedes the cancel.
        assert!(
            ledger
                .unsent(
                    TradeUp::AdoptRestored {
                        command_sequence: 0,
                        pairs: vec![("t".into(), "BTC".into())],
                        venue_ids: Vec::new(),
                    },
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        let rest = ledger.release_held();
        assert!(matches!(
            rest.as_slice(),
            [TradeUp::AdoptRestored { .. }, TradeUp::Cancel { order_client_id, .. }] if order_client_id == "h1"
        ));
    }

    #[test]
    fn giving_up_on_the_adapter_releases_the_held_cancels_though_it_never_answered() {
        let mut ledger = TradeLeaseCommandLedger::default();
        assert!(
            ledger
                .unsent(
                    cancel("h1", OrderCancelIntent::RiskOff),
                    TradeLeaseCommandLedgerWhy::NeverSent
                )
                .is_empty()
        );
        assert!(ledger.resumed(Some(1), &opened(2, 0)).flush.is_empty());
        assert!(ledger.release_held().is_empty(), "a plain release waits for the adapter's answer");
        assert!(ledger.gated());
        let rest = ledger.release_held_anyway();
        assert!(
            matches!(rest.as_slice(), [TradeUp::Cancel { order_client_id, .. }] if order_client_id == "h1")
        );
        assert!(!ledger.gated(), "the gate is open: nothing waits for an adapter that gave up");
        assert!(ledger.adapter_answered(true).is_empty(), "a late answer releases nothing twice");
    }

    #[test]
    fn a_closed_lease_names_the_unknown_and_rejects_the_unprocessed() {
        let wrote_all = |ledger: &mut TradeLeaseCommandLedger| {
            for (n, frame) in [
                submit("s1"),
                TradeUp::SubmitBatch {
                    command_sequence: 0,
                    orders: vec![order("s2"), order("s3")],
                },
                cancel("k1", OrderCancelIntent::RiskOff),
                TradeUp::Modify {
                    command_sequence: 0,
                    order: order("m1"),
                    new_qty: None,
                    new_price: Some(1.0),
                    new_trigger_price: None,
                },
            ]
            .into_iter()
            .enumerate()
            {
                let mut frame = frame;
                set_command_sequence(&mut frame, u64::try_from(n).unwrap_or(0) + 1);
                ledger.wrote(frame);
            }
        };

        // Another way out: the daemon may have executed any of them. Nothing is synthesized, the submits
        // and the cancel are returned for the link to name.
        let mut ledger = TradeLeaseCommandLedger::default();
        wrote_all(&mut ledger);
        let closed = ledger.closed(TradeLeaseCommandLedgerFate::Unknown);
        assert!(closed.events.is_empty(), "UNKNOWN is never a synthesized terminal");
        assert_eq!(closed.unknown_submits, ["s1", "s2", "s3"]);
        assert_eq!(closed.unknown_cancels, ["k1"]);
        assert_eq!(ledger.in_flight(), 0, "every Written frame got a disposition");

        // The daemon ended the lease at a frame it refused: whatever it never acknowledged was not read.
        let mut ledger = TradeLeaseCommandLedger::default();
        wrote_all(&mut ledger);
        let closed = ledger.closed(TradeLeaseCommandLedgerFate::Unprocessed);
        assert!(closed.unknown_submits.is_empty() && closed.unknown_cancels.is_empty());
        assert_eq!(terminal_order_client_ids(&closed.events), ["s1", "s2", "s3"]);
        assert!(closed.events.iter().any(|e| matches!(
            e,
            Event::OrderCancelRejected(r) if r.order_client_id == "k1"
                && r.reason.as_str() == LEASE_CLOSED_CANCEL_REASON
        )));
        assert!(closed.events.iter().any(|e| matches!(
            e,
            Event::OrderModifyRejected(r) if r.order_client_id == "m1"
        )));

        // An acknowledged frame is the daemon's: neither fate touches it.
        let mut ledger = TradeLeaseCommandLedger::default();
        wrote_all(&mut ledger);
        ledger.acked(4);
        let closed = ledger.closed(TradeLeaseCommandLedgerFate::Unknown);
        assert!(closed.unknown_submits.is_empty() && closed.events.is_empty());
    }
    #[test]
    fn a_request_frame_is_not_an_order_and_synthesizes_nothing() {
        let mut ledger = TradeLeaseCommandLedger::default();
        for frame in [
            TradeUp::WireIds { rid: 1, order_client_ids: vec!["a".into()] },
            TradeUp::IdentityRecord { rid: 2 },
            TradeUp::Ping,
            TradeUp::TradeStatus { rid: 3 },
        ] {
            assert!(ledger.unsent(frame, TradeLeaseCommandLedgerWhy::NeverSent).is_empty());
        }
        assert_eq!(ledger.held_len(), 0);
    }

    #[test]
    fn a_non_finite_number_never_crosses_and_a_batch_loses_only_its_offenders() {
        let mut nan = order("nan");
        nan.price = Some(f64::NAN);
        let (sent, events) =
            screen_finite(TradeUp::Submit { command_sequence: 0, order: nan.clone() });
        assert!(sent.is_none());
        assert_eq!(terminal_order_client_ids(&events), ["nan"]);
        let mut inf = order("inf");
        inf.qty = f64::INFINITY;
        let (sent, events) = screen_finite(TradeUp::SubmitBatch {
            command_sequence: 4,
            orders: vec![order("ok1"), nan, inf, order("ok2")],
        });
        let Some(TradeUp::SubmitBatch { command_sequence: cseq, orders }) = sent else {
            panic!("the good ones go")
        };
        assert_eq!(cseq, 4);
        let ids: Vec<&str> = orders.iter().map(|o| o.order_client_id.as_str()).collect();
        assert_eq!(ids, ["ok1", "ok2"]);
        assert_eq!(terminal_order_client_ids(&events), ["nan", "inf"]);
        let (sent, events) = screen_finite(TradeUp::Modify {
            command_sequence: 0,
            order: order("m"),
            new_qty: None,
            new_price: Some(f64::NEG_INFINITY),
            new_trigger_price: None,
        });
        assert!(sent.is_none());
        assert!(
            matches!(events.as_slice(), [Event::OrderModifyRejected(r)] if r.reason.as_str() == NON_FINITE_MODIFY_REASON)
        );
        let (sent, events) = screen_finite(TradeUp::Modify {
            command_sequence: 0,
            order: order("m"),
            new_qty: None,
            new_price: None,
            new_trigger_price: Some(f64::NAN),
        });
        assert!(sent.is_none(), "a non-finite trigger is screened like a non-finite price");
        assert!(
            matches!(events.as_slice(), [Event::OrderModifyRejected(r)] if r.reason.as_str() == NON_FINITE_MODIFY_REASON)
        );
        let fine = TradeUp::Submit { command_sequence: 0, order: order("fine") };
        let (sent, events) = screen_finite(fine.clone());
        assert_eq!(sent, Some(fine));
        assert!(events.is_empty());
    }

    #[test]
    fn the_too_large_disposition_names_itself() {
        let mut ledger = TradeLeaseCommandLedger::default();
        match ledger.unsent(submit("big"), TradeLeaseCommandLedgerWhy::TooLarge).as_slice() {
            [Event::OrderRejected(r)] => assert_eq!(r.reason.as_str(), TOO_LARGE_SUBMIT_REASON),
            other => panic!("{other:?}"),
        }
    }
}
