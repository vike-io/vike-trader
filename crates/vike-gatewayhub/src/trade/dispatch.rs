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
//! * **Dedup.** A `Submit` whose `(account, coid)` the account has already seen is NOT sent again
//!   (`crate::trade::dedup`): the coid is minted by the core, so a second sight is a retry. It was
//!   acknowledged when it was read; nothing more is owed.
//! * **The HALT gate**, at the submit and modify seam, on the gatewayhub's OWN halt path (the one file
//!   `vike_boot::halt` resolved for this process): while the sentinel exists a submit that opens risk is
//!   refused with the terminal `OrderRejected` the contract requires (`vike_exec::halt::HALT_REJECT_REASON`,
//!   byte for byte the bridge's), and a modify with the non-terminal `OrderModifyRejected`. A reducing
//!   submit is admitted per `vike_exec::halt::halt_admits_submit_under` and the account's `halt_admit`
//!   mode. **A cancel is never gated** and neither is `confirm`: halt must let an operator exit, never
//!   trap a position. The bridge's own gate (`ExecActor`) is behind this one; this is the second wall at
//!   the seam, not a replacement.
//! * **`Budget::spend`** once per order-class command (submit, cancel, amend). The budget is in shadow
//!   mode and refuses nothing; the verdict is still honoured here, so the day it enforces an order is
//!   refused loudly (a synthesized reject) and never vanishes.
//! * **`WireIds` and `ResolveOrders`**, the query-only request frames (`crate::trade::resolve`).
//!
//! # Teardown
//!
//! `ExecCmd::Stop` (or a closed channel) ends the loop AFTER every command queued before it ran, then
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

use vike_exec::halt::{
    HALT_REJECT_REASON, PositionEvidence, halt_admits_submit_under, sentinel_engaged,
};
use vike_exec::{CancelIntent, ExecutionClient};
use vike_gatewayhub_client::proto::TradeDown;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};
use vike_model::{HaltAdmit, OrderRequest};

use super::dedup::DedupSet;
use super::lease::{ADAPTER_FAILED_REASON, Lease};
use super::resolve::{resolve_rows, wire_pairs};
use super::session::AccountSession;
use crate::budget::ceiling::{Axis, market_of};
use crate::budget::class::Class;
use crate::budget::ledger::Budget;
use crate::budget::mode::Admission;

/// The reason on the terminal reject of an order the budget refused (unreachable while the budget is in
/// shadow mode: the text exists so the day it enforces, an order does not vanish).
pub const BUDGET_REFUSED_REASON: &str = "gatewayhub: the venue's order budget is exhausted";

/// One thing to do to the account's `ExecutionClient`, in the order it arrived.
pub enum ExecCmd {
    /// `submit`.
    Submit(Box<OrderRequest>),
    /// `submit_batch`.
    SubmitBatch(Vec<OrderRequest>),
    /// `cancel_with_intent`.
    Cancel {
        /// The client order id.
        coid: String,
        /// Why.
        intent: CancelIntent,
    },
    /// `cancel_batch_with_intent`.
    CancelBatch {
        /// The client order ids.
        coids: Vec<String>,
        /// Why.
        intent: CancelIntent,
    },
    /// `modify`.
    Modify {
        /// The whole resting order.
        order: Box<OrderRequest>,
        /// The requested quantity.
        new_qty: Option<f64>,
        /// The requested price.
        new_price: Option<f64>,
    },
    /// `confirm`.
    Confirm(String),
    /// `adopt_restored`.
    Adopt(Vec<(String, String)>),
    /// The boot wire-id prefetch; answered through `to`.
    WireIds {
        /// The request id.
        rid: u64,
        /// The client order ids.
        coids: Vec<String>,
        /// The lease that asked.
        to: Arc<Lease>,
    },
    /// The query-only resolution of unacknowledged submits; answered through `to`.
    Resolve {
        /// The request id.
        rid: u64,
        /// The client order ids.
        coids: Vec<String>,
        /// The lease that asked.
        to: Arc<Lease>,
    },
    /// End the loop after everything queued before this.
    Stop,
}

/// What the dispatch thread owns.
pub struct ExecCtx {
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
    /// Whether the venue can re-query an order by client order id.
    pub resolves_by_coid: bool,
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
    Reject { coid: String, ts: i64 },
    /// A cancel: a non-terminal `OrderCancelRejected`.
    CancelAdvisory { coid: String },
    /// A modify: a non-terminal `OrderModifyRejected`.
    ModifyAdvisory { coid: String, ts: i64 },
}

/// The thread body. Returns when the channel closes or `Stop` is read.
///
/// ⚠ **A panic in the bridge ends the ADAPTER, visibly, never the queue.** Each command runs under
/// `catch_unwind`; on a panic the adapter is marked dead (`ExecCtx::dead`, which `start_adapter` and the
/// status report read), the command being handled gets the event the contract owes it, a reconcile is
/// asked for, and this thread keeps draining: every command read from then on is answered as a command to
/// an adapter that is gone, until the session tears the adapter down (`Stop`). It never exits on its own,
/// so no command acknowledged into the queue can be dropped with a closed channel.
pub fn run(rx: Receiver<ExecCmd>, mut ctx: ExecCtx) {
    let mut owed: Vec<Owed> = Vec::new();
    let mut failed = false;
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, ExecCmd::Stop) {
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
fn adapter_failed(ctx: &ExecCtx, payload: &(dyn Any + Send), owed: Vec<Owed>) {
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
            Owed::Reject { coid, ts } => Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: ADAPTER_FAILED_REASON.into(),
                ts,
            }),
            Owed::CancelAdvisory { coid } => Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: coid,
                reason: ADAPTER_FAILED_REASON.into(),
                ts: 0,
            }),
            Owed::ModifyAdvisory { coid, ts } => Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid,
                reason: ADAPTER_FAILED_REASON.into(),
                ts,
            }),
        };
        ctx.session.emit_event(event);
    }
    ctx.session.reconcile_now("the venue adapter failed");
}

/// A command read after the adapter died: the event the contract owes, or an empty answer for a query.
fn answer_dead(ctx: &ExecCtx, cmd: ExecCmd) {
    match cmd {
        ExecCmd::WireIds { rid, to, .. } => {
            to.send(TradeDown::WireIdsReply { rid, pairs: Vec::new() });
        }
        ExecCmd::Resolve { rid, coids, to } => {
            let results = resolve_rows(&coids, |_| false, false, |_| {});
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

fn handle(ctx: &mut ExecCtx, cmd: ExecCmd, owed: &mut Vec<Owed>) {
    match cmd {
        ExecCmd::Submit(order) => {
            let halted = halt_engaged(ctx);
            if let Some(order) = vet_submit(ctx, *order, halted, owed) {
                ctx.client.submit(&order);
                owed.clear();
            }
        }
        ExecCmd::SubmitBatch(orders) => {
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
        ExecCmd::Cancel { coid, intent } => {
            spend(ctx, &ctx.symbol.clone(), 1);
            owed.push(Owed::CancelAdvisory { coid: coid.clone() });
            ctx.client.cancel_with_intent(&coid, intent);
            owed.clear();
        }
        ExecCmd::CancelBatch { coids, intent } => {
            spend(ctx, &ctx.symbol.clone(), u64::try_from(coids.len()).unwrap_or(u64::MAX));
            owed.extend(coids.iter().map(|c| Owed::CancelAdvisory { coid: c.clone() }));
            ctx.client.cancel_batch_with_intent(&coids, intent);
            owed.clear();
        }
        ExecCmd::Modify { order, new_qty, new_price } => {
            if halt_engaged(ctx) {
                // Conservative, as the bridge is: a modify can add size or chase price, and reduce-only
                // cannot be inferred here, so under HALT the exit path is cancel. Announced, never
                // silent: the advisory is non-terminal and the resting order keeps its terms.
                ctx.session.emit_event(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: HALT_REJECT_REASON.into(),
                    ts: order.ts,
                }));
                return;
            }
            spend(ctx, &order.symbol, 1);
            owed.push(Owed::ModifyAdvisory { coid: order.client_order_id.clone(), ts: order.ts });
            ctx.client.modify(&order, new_qty, new_price);
            owed.clear();
        }
        ExecCmd::Confirm(coid) => ctx.client.confirm(&coid),
        ExecCmd::Adopt(pairs) => ctx.client.adopt_restored(&pairs),
        ExecCmd::WireIds { rid, coids, to } => {
            let pairs = wire_pairs(&coids, |c| ctx.client.wire_id_for(c));
            to.send(TradeDown::WireIdsReply { rid, pairs });
        }
        ExecCmd::Resolve { rid, coids, to } => {
            let results = {
                let dedup = &ctx.dedup;
                let client = &mut ctx.client;
                resolve_rows(
                    &coids,
                    |c| dedup.contains(c),
                    ctx.resolves_by_coid,
                    |c| {
                        client.confirm(c);
                    },
                )
            };
            to.send(TradeDown::OrdersResolved { rid, results });
        }
        ExecCmd::Stop => {}
    }
}

/// Whether the HALT sentinel exists NOW (a filesystem `exists()`, evaluated once per command).
fn halt_engaged(ctx: &ExecCtx) -> bool {
    sentinel_engaged(ctx.halt_path.as_deref())
}

/// Dedup, the HALT gate and the budget for ONE submit. `Some` = send it; `None` = it was a duplicate or
/// was refused here (with its terminal event already in the ring).
fn vet_submit(
    ctx: &mut ExecCtx,
    order: OrderRequest,
    halted: bool,
    owed: &mut Vec<Owed>,
) -> Option<OrderRequest> {
    // Only LOOK here; the coid is remembered once the order is admitted. An order this gate refuses
    // (HALT, budget) was never sent, so the core's retry of the same coid is a new decision, not a
    // duplicate to swallow.
    if ctx.dedup.contains(&order.client_order_id) {
        tracing::debug!(
            coid = %order.client_order_id,
            "TRADE-LINK: a duplicate submit (same client order id) was acknowledged and not re-sent"
        );
        return None;
    }
    if halted && !halt_admits_submit_under(&order, ctx.halt_admit, PositionEvidence::NO_BOOK) {
        reject(ctx, &order, HALT_REJECT_REASON);
        return None;
    }
    if spend(ctx, &order.symbol, 1) == Admission::Refuse {
        reject(ctx, &order, BUDGET_REFUSED_REASON);
        return None;
    }
    // This dedup set is per ADAPTER (a panic rebuild or a changed start empties it), so a coid admitted here may
    // have lived before in this session: from now on it is a new order to the ring's first-wins memory too.
    ctx.dedup.first_sight(&order.client_order_id);
    ctx.session.forget_order_facts(&order.client_order_id);
    owed.push(Owed::Reject { coid: order.client_order_id.clone(), ts: order.ts });
    Some(order)
}

/// The terminal `OrderRejected` of an order that never reached the adapter.
fn reject(ctx: &ExecCtx, order: &OrderRequest, reason: &str) {
    ctx.session.emit_event(Event::OrderRejected(OrderRejected {
        client_order_id: order.client_order_id.clone(),
        reason: reason.into(),
        ts: order.ts,
    }));
}

/// Count `cost` order-class units against the venue's order meter.
fn spend(ctx: &ExecCtx, symbol: &str, cost: u64) -> Admission {
    let market = market_of(&ctx.venue, symbol);
    ctx.budget.spend(&ctx.venue, market, Axis::Orders, Class::Order, cost, Instant::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_refusal_reason_is_a_sentence_an_operator_can_grep() {
        assert!(BUDGET_REFUSED_REASON.starts_with("gatewayhub:"));
    }
}
