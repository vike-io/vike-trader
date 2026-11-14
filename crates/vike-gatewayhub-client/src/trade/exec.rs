//! [`ExecutionClientGateway`]: the `ExecutionClient` the tradehub's core drives when the venue is the
//! gatewayhub's.
//!
//! It is the `ExecutionClientActor` shape (`crates/vike-bridge-core/src/exec_actor.rs`) with a socket where the
//! venue thread was: `submit`, `cancel`, `modify` and `confirm` are **non-blocking channel sends** to
//! the link thread, which numbers the frame, writes it and keeps the ledger (`crate::trade::ledger`).
//!
//! # The fold thread's whole cost
//!
//! `submit` does what `ExecutionClientActor::submit` does and nothing else: ONE `exists()` stat of the HALT
//! sentinel (`vike_execution::halt::sentinel_engaged`), a clone of the request into a frame, and a channel
//! send. No serialization, no socket write, no lock: the cost the core-hop latency gate
//! (`crates/vike-core/tests/runtime_latency.rs`) measures does not move. `tests/trade/trade_text_gate.rs` pins
//! that `submit`/`cancel`/`modify` contain none of `serde_json`, `write` or `lock`.
//!
//! # No order may silently vanish
//!
//! A send that FAILS (the link thread has ended: a closed lease, a detached client) synthesizes the
//! same events `ExecutionClientActor` does for a dead channel - a terminal `OrderRejected` for a submit, a
//! NON-terminal `OrderCancelRejected` for a cancel - on the core's own lane, from the fold thread. A
//! frame that is accepted by the channel and then cannot be sent is the link thread's: the ledger's
//! NEVER-SENT rules.
//!
//! # The two traps of a `Box<dyn ExecutionClient + Send>`
//!
//! `wire_id_for` and `adopt_restored` are implemented here, not left to the trait's defaults:
//! `vike_execution`'s boxed impl DELEGATES them (`crates/vike-execution/CLAUDE.md`), so a default here would
//! silently restore nothing for hyperliquid.

use vike_execution::{ExecutionClient, RestoredOrder};
use vike_model::OrderCancelIntent;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};

use super::ids::Lookup;
use super::link::TradeLease;
use super::shared::{TradeLeaseMessage, TradeLeaseShared};
use crate::proto::{DetachPhase, TradeUp};

/// Why a trigger amend was not sent: the daemon of this lease did not advertise
/// [`crate::proto::FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER`].
const MODIFY_TRIGGER_UNADVERTISED: &str = "the gatewayhub of this account predates the trigger \
     amend (`gateway_trade_modify_trigger` not advertised), so nothing was sent and the order keeps its terms";

/// Why an order carrying an attributes-wave term was not sent: the daemon of this lease did not advertise
/// [`crate::proto::FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES`].
const ORDER_ATTRIBUTES_UNADVERTISED: &str = "the gatewayhub of this account predates the attributes \
     wave's order terms (`gateway_trade_order_attributes` not advertised): it would drop them and send a plain \
     order, so nothing was sent";

/// Whether `request` carries a term an older daemon would DROP and still send the order without (decision 0267):
/// an attribute, a percent trail, an adjustment, a hidden display (0), or a release the venue is to hold itself.
/// Every other order never reads the daemon's words, so a plain one takes no lock.
fn carries_attributes_terms(request: &OrderRequest) -> bool {
    request.attributes.is_some()
        || request.trail_percent.is_some()
        || request.adjust.is_some()
        || request.display_quantity == Some(0.0)
        || request.release.is_some()
}

/// The gatewayhub-backed `ExecutionClient`. Built by [`TradeLease::execution_client`]; holds the session
/// (and so the link) alive for as long as the core holds the client.
pub struct ExecutionClientGateway {
    session: TradeLease,
}

impl ExecutionClientGateway {
    pub(crate) fn new(session: TradeLease) -> Self {
        Self { session }
    }

    fn shared(&self) -> &TradeLeaseShared {
        self.session.shared()
    }

    /// Whether the HALT sentinel exists right now: one `exists()`, evaluated once per call.
    fn halt_engaged(&self) -> bool {
        vike_execution::halt::sentinel_engaged(self.shared().halt_path.as_deref())
    }

    /// Hand a command frame to the link thread. `false` = the link thread has ended.
    fn enqueue(&self, frame: TradeUp) -> bool {
        self.shared().tx.send(TradeLeaseMessage::Frame(Box::new(frame))).is_ok()
    }

    /// Deliver one synthesized event on the core's lane. A closed lane is `CoreGone`: the process is
    /// shutting down and there is nothing to deliver to.
    fn emit(&self, event: Event) {
        let _ = self.shared().events.blocking_send(event);
    }

    /// Why a command could not be enqueued, in the words the tradehub log and the GUI show.
    fn unavailable(&self) -> String {
        self.shared().unavailable_text()
    }

    fn reject(&self, request: &OrderRequest, reason: &str) {
        self.emit(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: reason.to_string().into(),
            timestamp: request.timestamp,
        }));
    }

    fn cancel_rejected(&self, order_client_id: &str, reason: &str) {
        self.emit(Event::OrderCancelRejected(OrderCancelRejected {
            order_client_id: order_client_id.to_string(),
            reason: reason.to_string().into(),
            timestamp: 0,
        }));
    }

    fn modify_rejected(&self, order: &OrderRequest, reason: &str) {
        self.emit(Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: order.order_client_id.clone(),
            reason: reason.to_string().into(),
            // stamped when made, `vike_bridge_core::rest::amend_answer_time`'s rule (a layer this crate may not link)
            timestamp: vike_model::now_ms(),
        }));
    }

    /// The HALT gate of one submit: `true` = refused (the terminal reject is already emitted).
    fn halt_refuses(&self, request: &OrderRequest) -> bool {
        if !self.halt_engaged() {
            return false;
        }
        if !vike_execution::halt::halt_admits_submit(request) {
            self.reject(request, vike_execution::halt::HALT_REJECT_REASON);
            return true;
        }
        // NEVER SILENT: this boundary can only check the caller-asserted flag, so an order leaving
        // while the operator believes the switch is engaged must be findable in the log. The target
        // and words are the bridge's own (`ExecutionClientActor::submit`): an operator greps one line.
        tracing::warn!(
            target: "vike_bridge_core::halt",
            order_client_id = %request.order_client_id,
            symbol = %request.symbol,
            qty = request.qty,
            "HALT engaged: admitting a reduce_only submit so the operator can still get out. \
             This boundary trusts the flag - it cannot verify the order against a position."
        );
        false
    }

    /// The gatewayhub-word gate of one submit: `true` = refused (the terminal reject is already emitted). An order
    /// carrying an attributes-wave term goes only to a daemon that advertised it reads them; a plain order never
    /// asks (no lock on the fold thread for it).
    fn attributes_refused(&self, request: &OrderRequest) -> bool {
        if !carries_attributes_terms(request)
            || self.shared().has_feature(crate::proto::FEATURE_GATEWAY_TRADE_ORDER_ATTRIBUTES)
        {
            return false;
        }
        self.reject(request, ORDER_ATTRIBUTES_UNADVERTISED);
        true
    }
}

impl ExecutionClient for ExecutionClientGateway {
    fn submit(&mut self, request: &OrderRequest) {
        if self.halt_refuses(request) || self.attributes_refused(request) {
            return;
        }
        if !self.enqueue(TradeUp::Submit { command_sequence: 0, order: request.clone() }) {
            // The link thread has ended: the order never left, so the terminal reject the contract
            // requires is made here. (If even this send fails the core is gone.)
            let reason = self.unavailable();
            self.reject(request, &reason);
        }
    }

    fn cancel(&mut self, order_client_id: &str) {
        // ONE cancel door, so the two can never disagree (the `cancel_with_intent` contract).
        self.cancel_with_intent(order_client_id, OrderCancelIntent::Unspecified);
    }

    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        // Deliberately NOT halt-gated: a kill switch must let an operator reduce/exit.
        let frame = TradeUp::Cancel {
            command_sequence: 0,
            order_client_id: order_client_id.to_string(),
            intent,
        };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            self.cancel_rejected(order_client_id, &reason);
        }
    }

    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        self.cancel_batch_with_intent(order_client_ids, OrderCancelIntent::Unspecified);
    }

    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        if order_client_ids.is_empty() {
            return;
        }
        // Deliberately NOT halt-gated, like the single door. ONE frame: the daemon fans it out per id
        // exactly as `ExecutionClientActor` does for a venue with no bulk lane.
        let frame = TradeUp::CancelBatch {
            command_sequence: 0,
            order_client_ids: order_client_ids.to_vec(),
            intent,
        };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            for order_client_id in order_client_ids {
                self.cancel_rejected(order_client_id, &reason);
            }
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        // HALT (conservative, as `ExecutionClientActor::modify`): a modify can add size or chase price, so a halt
        // blocks it, and the block is ANNOUNCED with the non-terminal advisory.
        if self.halt_engaged() {
            self.modify_rejected(order, vike_execution::halt::HALT_REJECT_REASON);
            return;
        }
        let frame = TradeUp::Modify {
            command_sequence: 0,
            order: order.clone(),
            new_qty,
            new_price,
            new_trigger_price: None,
        };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            self.modify_rejected(order, &reason);
        }
    }

    /// The trigger rides the same frame, to a daemon that ADVERTISED it reads the key
    /// ([`crate::proto::FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER`]): an older one would drop it and move the limit
    /// alone, so the amend is refused here in words instead. HALT-gated as `modify`; the gatewayhub's
    /// bridge answers (or refuses) the rest.
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        if self.halt_engaged() {
            self.modify_rejected(order, vike_execution::halt::HALT_REJECT_REASON);
            return Ok(());
        }
        if !self.shared().has_feature(crate::proto::FEATURE_GATEWAY_TRADE_MODIFY_TRIGGER) {
            return Err(MODIFY_TRIGGER_UNADVERTISED);
        }
        let frame = TradeUp::Modify {
            command_sequence: 0,
            order: order.clone(),
            new_qty,
            new_price,
            new_trigger_price: Some(new_trigger_price),
        };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            self.modify_rejected(order, &reason);
        }
        Ok(())
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        // The HALT gate is per ORDER, exactly as the default fan-out through `submit` would have it;
        // what survives travels as ONE frame (split by the link thread if it is over the ceiling).
        let admitted: Vec<OrderRequest> = requests
            .iter()
            .filter(|r| !self.halt_refuses(r) && !self.attributes_refused(r))
            .cloned()
            .collect();
        if admitted.is_empty() {
            return;
        }
        if !self.enqueue(TradeUp::SubmitBatch { command_sequence: 0, orders: admitted.clone() }) {
            let reason = self.unavailable();
            for request in &admitted {
                self.reject(request, &reason);
            }
        }
    }

    fn confirm(&mut self, order_client_id: &str) {
        // NOT gated on HALT: a status confirm places no order. The re-query runs at the gatewayhub, off
        // this thread; the venue's authoritative terminal returns as an ordinary event.
        let frame =
            TradeUp::Confirm { command_sequence: 0, order_client_id: order_client_id.to_string() };
        if !self.enqueue(frame) {
            tracing::debug!(
                order_client_id = order_client_id,
                "gatewayhub trade: confirm not sent (the lease has ended)"
            );
        }
    }

    /// Phase one: raise the stop and return at once, join nothing. Idempotent with `detach`.
    fn begin_detach(&mut self) {
        self.shared().mark_stopping();
        let _ = self.shared().tx.send(TradeLeaseMessage::Detach(DetachPhase::Begin));
    }

    /// Phase two: flush what is queued (the shutdown policy's cancel sweep ran BEFORE phase one and
    /// is already ahead of this in the channel), say goodbye and end the link thread.
    fn detach(&mut self) {
        self.session.detach();
    }

    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        let ids = self.shared().wire_ids.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        match ids.lookup(order_client_id) {
            Lookup::Wire(wire) => Some(wire),
            Lookup::Echo => None,
            Lookup::Miss => {
                // An invariant violation: the composition root must prefetch every restored order client id
                // before the core is spawned. Loud, and not once per call: the restore asks per
                // restored entry. Once per ACCOUNT's lease (a process-wide flag hid every account's
                // miss after the first).
                if ids.first_miss() {
                    let account = &self.shared().account;
                    tracing::error!(
                        order_client_id, venue = %account.venue, account = ?account.label,
                        "gatewayhub trade: wire_id_for asked about an order client id nobody prefetched - the \
                         restore would plan with an incomplete translation map (logged once per \
                         account)"
                    );
                }
                None
            }
        }
    }

    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        if orders.is_empty() {
            return;
        }
        // Fire and forget, queued like `submit` and FIFO with every later command, so a `cancel` sent
        // after this one sees the state. Idempotent: a re-send after a reconnect is harmless.
        let frame = super::mirror::adopt_restored_frame(orders);
        if !self.enqueue(frame) {
            tracing::warn!(
                count = orders.len(),
                "gatewayhub trade: restored orders could not be announced (the lease has ended)"
            );
        }
    }
}
