//! **The cpapi LIVE order hold** — stage A of
//! `docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`: a `(cpapi, live)` mount
//! binds the real account, serves its data and reconciles it, and its exec client REFUSES every
//! order, because the Client Portal stream has no observed live fill path
//! (`crates/bridges/vike-ibkr/src/transport/cpapi/decode.rs`'s `decode_ws_frame` decodes only
//! `sor`), so a real order's fill could go unseen and a position build up nobody sees.
//!
//! [`HeldExec`] wraps the connected client. Each submit gets ONE `error!` and a synthesized
//! terminal `OrderRejected` and never reaches the gateway (no order may silently vanish); a modify
//! gets a non-terminal `OrderModifyRejected`; every cancel door, `confirm` and the lifecycle are
//! delegated, so a hold never traps an operator in an order. `vike-tradehub-core` holds the same arm in
//! front of this one (`crates/vike-tradehub-core/src/orders_held.rs`'s `OrdersHeldClient`); this is the
//! bridge doing what its `resolve` said, for a caller that holds the client without that fold.
//!
//! **What lifts it** is code backed by evidence, never a setting: stage B, the change that adds the
//! cpapi fill decoder written from frames CAPTURED by the owner-attended live smoke, with its
//! offline fixture. Until then [`super::IbkrVenueAdapter`] answers
//! `HeldBelowLive::OrdersHeld` for every cpapi LIVE arm.

use vike_exec::{CancelIntent, EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};

use crate::recon::cpapi::VENUE;

/// The reason a held order is rejected with: what happened, why, and that nothing was sent.
pub(super) const CPAPI_LIVE_HELD_REASON: &str = "ibkr: cpapi LIVE orders are held — the Client \
     Portal stream has no observed live fill path, so a real order's fill could go unseen; \
     nothing was sent (decision 0140, stage A)";

/// A connected cpapi LIVE client that sends no order — see the module doc for each door.
pub(super) struct HeldExec {
    pub(super) inner: Box<dyn ExecutionClient + Send>,
    pub(super) events: EventSender,
    /// The starting account, for the log line (a label, never a credential).
    pub(super) account: String,
}

impl HeldExec {
    /// Deliver one synthesized event; a closed lane means the core has exited.
    fn emit(&self, event: Event) {
        let _ = self.events.blocking_send(event);
    }
}

impl ExecutionClient for HeldExec {
    fn submit(&mut self, request: &OrderRequest) {
        tracing::error!(
            venue = VENUE,
            account = %self.account,
            coid = %request.client_order_id,
            symbol = %request.symbol,
            "ibkr: REFUSING submit — cpapi LIVE orders are HELD (no observed live fill path); the \
             order is rejected and nothing reaches the live account"
        );
        self.emit(Event::OrderRejected(OrderRejected {
            client_order_id: request.client_order_id.clone(),
            reason: CPAPI_LIVE_HELD_REASON.into(),
            ts: request.ts,
        }));
    }
    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        for request in requests {
            self.submit(request);
        }
    }
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        let _ = (new_qty, new_price);
        tracing::error!(
            venue = VENUE,
            account = %self.account,
            coid = %order.client_order_id,
            "ibkr: REFUSING modify — cpapi LIVE orders are HELD; the resting order, if any, is \
             UNCHANGED"
        );
        self.emit(Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: order.client_order_id.clone(),
            reason: CPAPI_LIVE_HELD_REASON.into(),
            ts: order.ts,
        }));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.inner.cancel(client_order_id);
    }
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        self.inner.cancel_with_intent(client_order_id, intent);
    }
    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        self.inner.cancel_batch(client_order_ids);
    }
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        self.inner.cancel_batch_with_intent(client_order_ids, intent);
    }
    fn confirm(&mut self, client_order_id: &str) {
        self.inner.confirm(client_order_id);
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
    fn wire_id_for(&self, coid: &str) -> Option<String> {
        self.inner.wire_id_for(coid)
    }
    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        self.inner.adopt_restored(orders);
    }
}
