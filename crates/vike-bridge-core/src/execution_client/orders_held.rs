//! **The order hold**: a bound execution client that sends no order. It stands in front of the client a
//! start bound, so an arm whose order path is not proven at its tier serves its data and reconcile while
//! every order is refused here, before it can reach the venue.
//!
//! ONE type for the two holds that were twins (the owner's ruling: one `ExecutionClientOrdersHeld`): the
//! gatewayhub's fold of an arm answered `ArmingHeldBelowLive::OrdersHeld` (its former `OrdersHeldClient`) and vike-ibkr's
//! cpapi LIVE hold (its `HeldExec`, decision 0140 stage A). What told them apart is a parameter: the venue,
//! the account label and the reason every refusal carries, the sentence a strategy, the GUI and the journal
//! read. The log line is one wording for both, with the reason as a field.
//!
//! What it does, door by door:
//!
//! * **`submit`, `submit_batch`**: refused. One `error!` and a synthesized terminal `OrderRejected` per order,
//!   and the held client never sees it: no order may silently vanish, and none may leave. `submit_batch` is
//!   the trait's default, which fans out onto this `submit` and never reaches the held client.
//! * **`modify`, `modify_trigger`**: refused with a NON-terminal `OrderModifyRejected` in the same words, the
//!   HALT switch's shape (`crate::exec_actor::ExecutionClientActor`): an amend can add size, and the resting order, if
//!   any, keeps its terms. `modify_trigger` answers `Ok` after that refusal, not the trait's `Err`, whose words
//!   would say the venue cannot move a trigger (it can; the hold is why nothing moves).
//! * **every cancel door, and `confirm`**: delegated. A hold must never trap an operator in an order, and a
//!   confirm is a status read.
//! * **the lifecycle and the pure seams** (`begin_detach`, `detach`, `poll_events`, `on_bar`,
//!   `amend_semantics`, `wire_id_for`, `adopt_restored`): delegated, each explicitly, because a trait default
//!   here would shadow the held client's override.

use vike_execution::{EventSender, ExecutionClient, RestoredOrder};
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};
use vike_model::{OrderCancelIntent, OrderRequest};

/// A bound execution client that sends no order: see the module doc for each door.
pub struct ExecutionClientOrdersHeld {
    inner: Box<dyn ExecutionClient + Send>,
    events: EventSender,
    venue: String,
    /// The account the client was bound for, for the log line (a label, never a credential).
    account: String,
    /// What every refused order and amend says: what happened, why, and that nothing was sent.
    reason: &'static str,
}

impl ExecutionClientOrdersHeld {
    /// Hold every order of `inner`, the client a start bound for `account` on `venue`; `events` is the lane
    /// the refusals are said on, and `reason` their words.
    pub fn new(
        inner: Box<dyn ExecutionClient + Send>,
        events: EventSender,
        venue: impl Into<String>,
        account: impl Into<String>,
        reason: &'static str,
    ) -> Self {
        Self { inner, events, venue: venue.into(), account: account.into(), reason }
    }

    /// Deliver one synthesized event. A closed lane means the core has exited (the process is shutting
    /// down), and there is nothing to deliver to.
    fn emit(&self, event: Event) {
        let _ = self.events.blocking_send(event);
    }
}

impl ExecutionClient for ExecutionClientOrdersHeld {
    fn submit(&mut self, request: &OrderRequest) {
        tracing::error!(
            venue = %self.venue,
            account = %self.account,
            order_client_id = %request.order_client_id,
            symbol = %request.symbol,
            reason = self.reason,
            "{}: REFUSING submit — this account is mounted with every ORDER HELD, so the order is \
             rejected here and nothing reaches the venue",
            self.venue
        );
        self.emit(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: self.reason.into(),
            timestamp: request.timestamp,
        }));
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        let _ = (new_qty, new_price);
        tracing::error!(
            venue = %self.venue,
            account = %self.account,
            order_client_id = %order.order_client_id,
            reason = self.reason,
            "{}: REFUSING modify — this account is mounted with every ORDER HELD; the resting \
             order, if any, is UNCHANGED",
            self.venue
        );
        self.emit(Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order.order_client_id.clone(),
            reason: self.reason.into(),
            timestamp: crate::rest::amend_answer_time(None),
        }));
    }
    /// Held exactly as [`Self::modify`]: the same `error!` and the same non-terminal refusal, never
    /// forwarded.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        _new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        self.modify(order, new_qty, new_price);
        Ok(())
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.inner.cancel(order_client_id);
    }
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        self.inner.cancel_with_intent(order_client_id, intent);
    }
    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        self.inner.cancel_batch(order_client_ids);
    }
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        self.inner.cancel_batch_with_intent(order_client_ids, intent);
    }
    fn confirm(&mut self, order_client_id: &str) {
        self.inner.confirm(order_client_id);
    }
    fn begin_detach(&mut self) {
        self.inner.begin_detach();
    }
    fn detach(&mut self) {
        self.inner.detach();
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.inner.poll_events()
    }
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) {
        self.inner.on_bar(bar);
    }
    fn amend_semantics(&self) -> Option<vike_venues::AmendSemantics> {
        self.inner.amend_semantics()
    }
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        self.inner.wire_id_for(order_client_id)
    }
    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        self.inner.adopt_restored(orders);
    }
}

#[cfg(test)]
#[path = "orders_held_tests.rs"]
mod orders_held_tests;
