//! The DISPATCH thread: the one thread that owns an account's `ExecutionClient`.
//!
//! The trait's methods take `&mut self` and its contract says they run on ONE thread and return at once,
//! so every command a lease reads is queued here (`AccountSession::enqueue`) and executed in order.
//! The queue is the order of arrival: one TCP connection preserves the client's order, a reconnect is
//! fenced (the previous connection is read to its end before the next is answered), and this thread
//! drains FIFO, so a `cancel` sent after an `adopt_restored` sees what it taught.
//!
//! # What this thread does besides forwarding
//!
//! * **Dedup.** A `Submit` whose `(account, order client id)` the account has already seen is NOT sent again
//!   (`crate::trade::dedup`): the order client id is minted by the core, so a second sight is a retry. It was
//!   acknowledged when it was read; nothing more is owed.
//! * **The HALT gate**, at the submit and modify seam, on the gatewayhub's OWN halt path (the one file
//!   `vike_boot::halt` resolved for this process): while the sentinel exists a submit that opens risk is
//!   refused with the terminal `OrderRejected` the contract requires (`vike_execution::halt::HALT_REJECT_REASON`,
//!   byte for byte the bridge's), and a modify with the non-terminal `OrderModifyRejected`. A reducing
//!   submit is admitted per `vike_execution::halt::halt_admits_submit_under` and the account's `halt_admit`
//!   mode. **A cancel is never gated** and neither is `confirm`: halt must let an operator exit, never
//!   trap a position. The bridge's own gate (`ExecutionClientActor`) is behind this one; this is the second wall at
//!   the seam, not a replacement.
//! * **`Budget::spend`** once per order-class command (submit, cancel, amend). The budget is in shadow
//!   mode and refuses nothing; the verdict is still honoured here, so the day it enforces an order is
//!   refused loudly (a synthesized reject) and never vanishes.
//! * **`WireIds` and `ResolveOrders`**, the query-only request frames (`crate::trade::resolve`).
//!
//! # Teardown
//!
//! `ExecutionClientDispatchCommand::Stop` (or a closed channel) ends the loop AFTER every command queued before it ran, then
//! `begin_detach` and `detach` on the client, in that order, and drops it. Nothing here cancels a resting
//! order: that is the client's own `cancel_orders_on_shutdown` sweep, which the tradehub sends as
//! ordinary cancels before it detaches (owner ruling D6).

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Instant;

use vike_execution::ExecutionClient;
use vike_execution::halt::{
    HALT_REJECT_REASON, PositionEvidence, halt_admits_submit_under, sentinel_engaged,
};
use vike_gatewayhub_client::proto::TradeDown;
use vike_model::OrderCancelIntent;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};
use vike_model::{HaltAdmit, OrderRequest};

use super::dedup::DedupSet;
use super::lease::{ADAPTER_FAILED_REASON, TradeLeaseHold};
use super::resolve::{resolve_rows, wire_pairs};
use super::session::AccountSession;
use crate::budget::ceiling::{BudgetCeilingAxis, market_of};
use crate::budget::class::BudgetClass;
use crate::budget::ledger::Budget;
use crate::budget::mode::BudgetAdmission;

/// The reason on the terminal reject of an order the budget refused (unreachable while the budget is in
/// shadow mode: the text exists so the day it enforces, an order does not vanish).
pub const BUDGET_REFUSED_REASON: &str = "gatewayhub: the venue's order budget is exhausted";

/// One thing to do to the account's `ExecutionClient`, in the order it arrived.
pub enum ExecutionClientDispatchCommand {
    /// `submit`.
    Submit(Box<OrderRequest>),
    /// `submit_batch`.
    SubmitBatch(Vec<OrderRequest>),
    /// `cancel_with_intent`.
    Cancel {
        /// The order client id.
        order_client_id: String,
        /// Why.
        intent: OrderCancelIntent,
    },
    /// `cancel_batch_with_intent`.
    CancelBatch {
        /// The order client ids.
        order_client_ids: Vec<String>,
        /// Why.
        intent: OrderCancelIntent,
    },
    /// `modify`.
    Modify {
        /// The whole resting order.
        order: Box<OrderRequest>,
        /// The requested quantity.
        new_qty: Option<f64>,
        /// The requested price.
        new_price: Option<f64>,
        /// The requested trigger: `Some` goes to `modify_trigger`, `None` to `modify`.
        new_trigger_price: Option<f64>,
    },
    /// `confirm`.
    Confirm(String),
    /// `adopt_restored`.
    Adopt(Vec<vike_execution::RestoredOrder>),
    /// The boot wire-id prefetch; answered through `to`.
    WireIds {
        /// The request id.
        rid: u64,
        /// The order client ids.
        order_client_ids: Vec<String>,
        /// The lease that asked.
        to: Arc<TradeLeaseHold>,
    },
    /// The query-only resolution of unacknowledged submits; answered through `to`.
    Resolve {
        /// The request id.
        rid: u64,
        /// The order client ids.
        order_client_ids: Vec<String>,
        /// The lease that asked.
        to: Arc<TradeLeaseHold>,
    },
    /// End the loop after everything queued before this.
    Stop,
}

/// What the dispatch thread owns.
pub struct ExecutionClientDispatchContext {
    /// The bound client.
    pub client: Box<dyn ExecutionClient + Send>,
    /// The session whose ring the synthesized events go to.
    pub session: Arc<AccountSession>,
    /// The venue (a `vike_model::VENUES` slug).
    pub venue: String,
    /// The engine symbol the adapter was started on: the market a cancel is counted against (a cancel
    /// names no symbol).
    pub symbol: String,
    /// The order-class budget.
    pub budget: Arc<Budget>,
    /// The HALT sentinel's path; `None` = no gate at this seam.
    pub halt_path: Option<PathBuf>,
    /// The halt-admit mode the start carried.
    pub halt_admit: HaltAdmit,
    /// Whether the venue can re-query an order by order client id.
    pub resolves_by_order_client_id: bool,
    /// The recent-submit set.
    pub dedup: DedupSet,
    /// Set by the dispatch thread when the bridge panicked: the session reads it to report the adapter
    /// dead and to build a fresh one at the next start (`run`'s doc).
    pub dead: Arc<AtomicBool>,
}

/// What the contract owes for a command the bridge was handling when it panicked: set just before the
/// bridge call, cleared just after, so a command the gate already answered (a duplicate, a HALT or budget
/// refusal) is never answered twice.
#[derive(Debug)]
enum Owed {
    /// A submit: a terminal `OrderRejected`.
    Reject { order_client_id: String, timestamp: i64 },
    /// A cancel: a non-terminal `OrderCancelRejected`.
    CancelAdvisory { order_client_id: String },
    /// A modify: a non-terminal `OrderModifyRejected`, stamped when it is made.
    ModifyAdvisory { order_client_id: String },
}

/// The thread body. Returns when the channel closes or `Stop` is read.
///
/// ⚠ **A panic in the bridge ends the ADAPTER, visibly, never the queue.** Each command runs under
/// `catch_unwind`; on a panic the adapter is marked dead (`ExecutionClientDispatchContext::dead`, which `start_adapter` and the
/// status report read), the command being handled gets the event the contract owes it, a reconcile is
/// asked for, and this thread keeps draining: every command read from then on is answered as a command to
/// an adapter that is gone, until the session tears the adapter down (`Stop`). It never exits on its own,
/// so no command acknowledged into the queue can be dropped with a closed channel.
pub fn run(rx: Receiver<ExecutionClientDispatchCommand>, mut ctx: ExecutionClientDispatchContext) {
    let mut owed: Vec<Owed> = Vec::new();
    let mut failed = false;
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, ExecutionClientDispatchCommand::Stop) {
            break;
        }
        // `ctx.dead` is also raised from OUTSIDE (the session's link supervisor takes the adapter down for a
        // lost venue link): what is still queued behind that moment is answered, never handed to a bridge
        // that is going away.
        if failed || ctx.dead.load(Ordering::SeqCst) {
            answer_dead(&ctx, cmd);
            continue;
        }
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| handle(&mut ctx, cmd, &mut owed))) {
            failed = true;
            adapter_failed(&ctx, &payload, std::mem::take(&mut owed));
        }
    }
    // Phase one, then phase two, on the same client: idempotent with each other by contract. A client that
    // panicked once may panic again; the teardown goes on.
    let _ = catch_unwind(AssertUnwindSafe(|| ctx.client.begin_detach()));
    let _ = catch_unwind(AssertUnwindSafe(|| ctx.client.detach()));
}

/// The bridge panicked: mark the adapter dead, say what happened to the command in hand and ask the
/// attached client for a reconcile pass (the order it was submitting may be at the venue).
fn adapter_failed(
    ctx: &ExecutionClientDispatchContext,
    payload: &(dyn Any + Send),
    owed: Vec<Owed>,
) {
    ctx.dead.store(true, Ordering::SeqCst);
    let why = panic_text(payload);
    tracing::error!(
        venue = %ctx.venue,
        panic = %why,
        in_flight = owed.len(),
        "TRADE-LINK: the venue adapter panicked and is dead; its queued commands are answered as \
         failed and a start of the adapter builds a fresh one"
    );
    for item in owed {
        let event = match item {
            Owed::Reject { order_client_id, timestamp: ts } => {
                Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id,
                    reason: ADAPTER_FAILED_REASON.into(),
                    timestamp: ts,
                })
            }
            Owed::CancelAdvisory { order_client_id } => {
                Event::OrderCancelRejected(OrderCancelRejected {
                    order_client_id,
                    reason: ADAPTER_FAILED_REASON.into(),
                    timestamp: 0,
                })
            }
            Owed::ModifyAdvisory { order_client_id } => {
                Event::OrderModifyRejected(OrderModifyRejected {
                    order_client_id,
                    reason: ADAPTER_FAILED_REASON.into(),
                    // stamped when made: `vike_bridge_core::rest::amend_answer_time`'s rule (sessions name no bridge crate)
                    timestamp: vike_model::now_ms(),
                })
            }
        };
        ctx.session.emit_event(event);
    }
    ctx.session.reconcile_now("the venue adapter failed");
}

/// A command read after the adapter died: the event the contract owes, or an empty answer for a query.
fn answer_dead(ctx: &ExecutionClientDispatchContext, cmd: ExecutionClientDispatchCommand) {
    match cmd {
        ExecutionClientDispatchCommand::WireIds { rid, to, .. } => {
            to.send(TradeDown::WireIdsReply { rid, pairs: Vec::new() });
        }
        ExecutionClientDispatchCommand::Resolve { rid, order_client_ids, to } => {
            let results = resolve_rows(&order_client_ids, |_| false, false, |_| {});
            to.send(TradeDown::OrdersResolved { rid, results });
        }
        other => ctx.session.synthesize(other),
    }
}

/// The text of a panic payload, bounded.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-text panic payload".to_string());
    text.chars().take(200).collect()
}

fn handle(
    ctx: &mut ExecutionClientDispatchContext,
    cmd: ExecutionClientDispatchCommand,
    owed: &mut Vec<Owed>,
) {
    match cmd {
        ExecutionClientDispatchCommand::Submit(order) => {
            let halted = halt_engaged(ctx);
            if let Some(order) = vet_submit(ctx, *order, halted, owed) {
                ctx.client.submit(&order);
                owed.clear();
            }
        }
        ExecutionClientDispatchCommand::SubmitBatch(orders) => {
            let halted = halt_engaged(ctx);
            let mut admitted = Vec::with_capacity(orders.len());
            for order in orders {
                if let Some(order) = vet_submit(ctx, order, halted, owed) {
                    admitted.push(order);
                }
            }
            if !admitted.is_empty() {
                ctx.client.submit_batch(&admitted);
            }
            owed.clear();
        }
        ExecutionClientDispatchCommand::Cancel { order_client_id, intent } => {
            spend(ctx, &ctx.symbol.clone(), 1);
            owed.push(Owed::CancelAdvisory { order_client_id: order_client_id.clone() });
            ctx.client.cancel_with_intent(&order_client_id, intent);
            owed.clear();
        }
        ExecutionClientDispatchCommand::CancelBatch { order_client_ids, intent } => {
            spend(
                ctx,
                &ctx.symbol.clone(),
                u64::try_from(order_client_ids.len()).unwrap_or(u64::MAX),
            );
            owed.extend(
                order_client_ids
                    .iter()
                    .map(|c| Owed::CancelAdvisory { order_client_id: c.clone() }),
            );
            ctx.client.cancel_batch_with_intent(&order_client_ids, intent);
            owed.clear();
        }
        ExecutionClientDispatchCommand::Modify { order, new_qty, new_price, new_trigger_price } => {
            if halt_engaged(ctx) {
                // Conservative, as the bridge is: a modify can add size or chase price, and reduce-only
                // cannot be inferred here, so under HALT the exit path is cancel. Announced, never
                // silent: the advisory is non-terminal and the resting order keeps its terms.
                ctx.session.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                    order_client_id: order.order_client_id.clone(),
                    reason: HALT_REJECT_REASON.into(),
                    // stamped when made: `vike_bridge_core::rest::amend_answer_time`'s rule
                    timestamp: vike_model::now_ms(),
                }));
                return;
            }
            spend(ctx, &order.symbol, 1);
            owed.push(Owed::ModifyAdvisory { order_client_id: order.order_client_id.clone() });
            match new_trigger_price {
                None => ctx.client.modify(&order, new_qty, new_price),
                // A bridge that cannot move a trigger refuses in words (the trait's default): told
                // upward as the non-terminal advisory, never the limit moved alone.
                Some(t) => {
                    if let Err(why) = ctx.client.modify_trigger(&order, new_qty, new_price, t) {
                        ctx.session.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                            order_client_id: order.order_client_id.clone(),
                            reason: format!("{}: {why}", order.venue).into(),
                            // stamped when made: `vike_bridge_core::rest::amend_answer_time`'s rule
                            timestamp: vike_model::now_ms(),
                        }));
                    }
                }
            }
            owed.clear();
        }
        ExecutionClientDispatchCommand::Confirm(order_client_id) => {
            ctx.client.confirm(&order_client_id)
        }
        ExecutionClientDispatchCommand::Adopt(pairs) => ctx.client.adopt_restored(&pairs),
        ExecutionClientDispatchCommand::WireIds { rid, order_client_ids, to } => {
            let pairs = wire_pairs(&order_client_ids, |c| ctx.client.wire_id_for(c));
            to.send(TradeDown::WireIdsReply { rid, pairs });
        }
        ExecutionClientDispatchCommand::Resolve { rid, order_client_ids, to } => {
            let results = {
                let dedup = &ctx.dedup;
                let client = &mut ctx.client;
                resolve_rows(
                    &order_client_ids,
                    |c| dedup.contains(c),
                    ctx.resolves_by_order_client_id,
                    |c| {
                        client.confirm(c);
                    },
                )
            };
            to.send(TradeDown::OrdersResolved { rid, results });
        }
        ExecutionClientDispatchCommand::Stop => {}
    }
}

/// Whether the HALT sentinel exists NOW (a filesystem `exists()`, evaluated once per command).
fn halt_engaged(ctx: &ExecutionClientDispatchContext) -> bool {
    sentinel_engaged(ctx.halt_path.as_deref())
}

/// Dedup, the HALT gate and the budget for ONE submit. `Some` = send it; `None` = it was a duplicate or
/// was refused here (with its terminal event already in the ring).
fn vet_submit(
    ctx: &mut ExecutionClientDispatchContext,
    order: OrderRequest,
    halted: bool,
    owed: &mut Vec<Owed>,
) -> Option<OrderRequest> {
    // Only LOOK here; the order client id is remembered once the order is admitted. An order this gate refuses
    // (HALT, budget) was never sent, so the core's retry of the same order client id is a new decision, not a
    // duplicate to swallow.
    if ctx.dedup.contains(&order.order_client_id) {
        tracing::debug!(
            order_client_id = %order.order_client_id,
            "TRADE-LINK: a duplicate submit (same order client id) was acknowledged and not re-sent"
        );
        return None;
    }
    if halted && !halt_admits_submit_under(&order, ctx.halt_admit, PositionEvidence::NO_BOOK) {
        reject(ctx, &order, HALT_REJECT_REASON);
        return None;
    }
    if spend(ctx, &order.symbol, 1) == BudgetAdmission::Refuse {
        reject(ctx, &order, BUDGET_REFUSED_REASON);
        return None;
    }
    // This dedup set is per ADAPTER (a panic rebuild or a changed start empties it), so an order client id admitted here may
    // have lived before in this session: from now on it is a new order to the ring's first-wins memory too.
    ctx.dedup.first_sight(&order.order_client_id);
    ctx.session.forget_order_facts(&order.order_client_id);
    owed.push(Owed::Reject {
        order_client_id: order.order_client_id.clone(),
        timestamp: order.timestamp,
    });
    Some(order)
}

/// The terminal `OrderRejected` of an order that never reached the adapter.
fn reject(ctx: &ExecutionClientDispatchContext, order: &OrderRequest, reason: &str) {
    ctx.session.emit_event(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order.order_client_id.clone(),
        reason: reason.into(),
        timestamp: order.timestamp,
    }));
}

/// Count `cost` order-class units against the venue's order meter.
fn spend(ctx: &ExecutionClientDispatchContext, symbol: &str, cost: u64) -> BudgetAdmission {
    let market = market_of(&ctx.venue, symbol);
    ctx.budget.spend(
        &ctx.venue,
        market,
        BudgetCeilingAxis::Orders,
        BudgetClass::Order,
        cost,
        Instant::now(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_refusal_reason_is_a_sentence_an_operator_can_grep() {
        assert!(BUDGET_REFUSED_REASON.starts_with("gatewayhub:"));
    }
}
