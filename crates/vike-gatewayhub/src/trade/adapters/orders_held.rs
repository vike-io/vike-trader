//! **THE ORDER HOLD** — what the start does with an arm whose probe answered
//! `vike_bridge_core::venue_adapter::HeldBelowLive::OrdersHeld`: bind the tier, send no order
//! (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A).
//!
//! `crate::contract`'s `resolution_to_arming` folds that answer to a row AT the bridge's tier with
//! `vike_config::ArmingBlock::OrdersHeld`, and `crate::contract`'s `contract_parts` hands every
//! outcome of such a row to [`hold_orders`], which wraps a `Live` outcome's client in
//! [`OrdersHeldClient`]. The bridge's own client refuses orders too; this one stands IN FRONT of it,
//! so a bridge whose `start` forgot its hold still sends nothing. It is the tier interlock's rule
//! applied to orders: the probe and the arm are one answer, and the fold holds the arm to it.
//!
//! What the wrapper does, door by door:
//!
//! * **`submit`, `submit_batch`**: refused. One `error!` and a synthesized terminal `OrderRejected`
//!   per order, and the inner client never sees it: no order may silently vanish, and none may
//!   leave.
//! * **`modify`**: refused with a NON-terminal `OrderModifyRejected`, the HALT switch's shape
//!   (`crates/vike-bridge-core/src/exec_actor.rs`'s `ExecActor`): an amend can add size, and the
//!   resting order, if any, keeps its terms.
//! * **every cancel door, and `confirm`**: delegated. A hold must never trap an operator in an
//!   order, and a confirm is a status read.
//! * **the lifecycle and the pure seams** (`begin_detach`, `detach`, `poll_events`, `on_bar`,
//!   `amend_semantics`, `wire_id_for`, `adopt_restored`): delegated, each explicitly, because a
//!   trait default here would shadow the inner client's override.

use vike_bridge_core::venue_adapter::{AdapterOutcome, ExecOutcome};
use vike_exec::{CancelIntent, EventSender, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::{Event, OrderModifyRejected, OrderRejected};

/// The reason every held order is rejected with — the sentence a strategy, the GUI and the
/// journal read, so it says what happened and that it is deliberate.
pub(crate) const ORDERS_HELD_REASON: &str = "orders held: this venue's arm is mounted with every \
     order refused (its order path at this tier is not proven); nothing was sent";

/// A held arm's outcome: a `Live` outcome's client wrapped in [`OrdersHeldClient`], said once at
/// `warn!`; a `Paper` outcome unchanged (it has no client to hold).
pub(crate) fn hold_orders(
    venue: &str,
    account: &AccountLabel,
    events: &EventSender,
    outcome: AdapterOutcome,
) -> AdapterOutcome {
    let AdapterOutcome { exec, recon, identity } = outcome;
    let exec = match exec {
        ExecOutcome::Live(mut live) => {
            tracing::warn!(
                venue,
                account = %account,
                bound_tier = live.bound_tier.as_str(),
                "{venue}: account `{account}` is mounted at the {} tier with every ORDER HELD — \
                 its data and reconcile run, and every order is rejected before it reaches the \
                 venue",
                live.bound_tier.as_str().to_uppercase()
            );
            live.client = Box::new(OrdersHeldClient {
                inner: live.client,
                events: events.clone(),
                venue: venue.to_string(),
                account: account.to_string(),
            });
            ExecOutcome::Live(live)
        }
        ExecOutcome::Paper => ExecOutcome::Paper,
    };
    AdapterOutcome { exec, recon, identity }
}

/// A bound exec client that sends no order: see the module doc for each door.
pub(crate) struct OrdersHeldClient {
    pub(crate) inner: Box<dyn ExecutionClient + Send>,
    pub(crate) events: EventSender,
    pub(crate) venue: String,
    pub(crate) account: String,
}

impl OrdersHeldClient {
    /// Deliver one synthesized event. A closed lane means the core has exited (the process is
    /// shutting down), and there is nothing to deliver to.
    fn emit(&self, event: Event) {
        let _ = self.events.blocking_send(event);
    }
}

impl ExecutionClient for OrdersHeldClient {
    fn submit(&mut self, request: &OrderRequest) {
        tracing::error!(
            venue = %self.venue,
            account = %self.account,
            coid = %request.client_order_id,
            symbol = %request.symbol,
            "{}: REFUSING submit — this account is mounted with every ORDER HELD, so the order is \
             rejected here and nothing reaches the venue",
            self.venue
        );
        self.emit(Event::OrderRejected(OrderRejected {
            cause: None,
            client_order_id: request.client_order_id.clone(),
            reason: ORDERS_HELD_REASON.into(),
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
            venue = %self.venue,
            account = %self.account,
            coid = %order.client_order_id,
            "{}: REFUSING modify — this account is mounted with every ORDER HELD; the resting \
             order, if any, is UNCHANGED",
            self.venue
        );
        self.emit(Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: order.client_order_id.clone(),
            reason: ORDERS_HELD_REASON.into(),
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
