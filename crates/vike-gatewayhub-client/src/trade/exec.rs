//! [`RemoteExecClient`]: the `ExecutionClient` the tradehub's core drives when the venue is the
//! gatewayhub's.
//!
//! It is the `ExecActor` shape (`crates/vike-bridge-core/src/exec_actor.rs`) with a socket where the
//! venue thread was: `submit`, `cancel`, `modify` and `confirm` are **non-blocking channel sends** to
//! the link thread, which numbers the frame, writes it and keeps the ledger (`crate::trade::ledger`).
//!
//! # The fold thread's whole cost
//!
//! `submit` does what `ExecActor::submit` does and nothing else: ONE `exists()` stat of the HALT
//! sentinel (`vike_exec::halt::sentinel_engaged`), a clone of the request into a frame, and a channel
//! send. No serialization, no socket write, no lock: the cost the core-hop latency gate
//! (`crates/vike-core/tests/runtime_latency.rs`) measures does not move. `tests/trade_text_gate.rs` pins
//! that `submit`/`cancel`/`modify` contain none of `serde_json`, `write` or `lock`.
//!
//! # No order may silently vanish
//!
//! A send that FAILS (the link thread has ended: a closed lease, a detached client) synthesizes the
//! same events `ExecActor` does for a dead channel - a terminal `OrderRejected` for a submit, a
//! NON-terminal `OrderCancelRejected` for a cancel - on the core's own lane, from the fold thread. A
//! frame that is accepted by the channel and then cannot be sent is the link thread's: the ledger's
//! NEVER-SENT rules.
//!
//! # The two traps of a `Box<dyn ExecutionClient + Send>`
//!
//! `wire_id_for` and `adopt_restored` are implemented here, not left to the trait's defaults:
//! `vike_exec`'s boxed impl DELEGATES them (`crates/vike-exec/CLAUDE.md`), so a default here would
//! silently restore nothing for hyperliquid.

use vike_exec::{CancelIntent, ExecutionClient};
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderRejected};

use super::ids::Lookup;
use super::link::TradeSession;
use super::shared::{Msg, Shared};
use crate::proto::{DetachPhase, TradeUp};

/// The gatewayhub-backed `ExecutionClient`. Built by [`TradeSession::exec_client`]; holds the session
/// (and so the link) alive for as long as the core holds the client.
pub struct RemoteExecClient {
    session: TradeSession,
}

impl RemoteExecClient {
    pub(crate) fn new(session: TradeSession) -> Self {
        Self { session }
    }

    fn shared(&self) -> &Shared {
        self.session.shared()
    }

    /// Whether the HALT sentinel exists right now: one `exists()`, evaluated once per call.
    fn halt_engaged(&self) -> bool {
        vike_exec::halt::sentinel_engaged(self.shared().halt_path.as_deref())
    }

    /// Hand a command frame to the link thread. `false` = the link thread has ended.
    fn enqueue(&self, frame: TradeUp) -> bool {
        self.shared().tx.send(Msg::Frame(Box::new(frame))).is_ok()
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
            client_order_id: request.client_order_id.clone(),
            reason: reason.to_string().into(),
            ts: request.ts,
        }));
    }

    fn cancel_rejected(&self, coid: &str, reason: &str) {
        self.emit(Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts: 0,
        }));
    }

    fn modify_rejected(&self, order: &OrderRequest, reason: &str) {
        self.emit(Event::OrderModifyRejected(OrderModifyRejected {
            client_order_id: order.client_order_id.clone(),
            reason: reason.to_string().into(),
            ts: order.ts,
        }));
    }

    /// The HALT gate of one submit: `true` = refused (the terminal reject is already emitted).
    fn halt_refuses(&self, request: &OrderRequest) -> bool {
        if !self.halt_engaged() {
            return false;
        }
        if !vike_exec::halt::halt_admits_submit(request) {
            self.reject(request, vike_exec::halt::HALT_REJECT_REASON);
            return true;
        }
        // NEVER SILENT: this boundary can only check the caller-asserted flag, so an order leaving
        // while the operator believes the switch is engaged must be findable in the log. The target
        // and words are the bridge's own (`ExecActor::submit`): an operator greps one line.
        tracing::warn!(
            target: "vike_bridge_core::halt",
            coid = %request.client_order_id,
            symbol = %request.symbol,
            qty = request.qty,
            "HALT engaged: admitting a reduce_only submit so the operator can still get out. \
             This boundary trusts the flag - it cannot verify the order against a position."
        );
        false
    }
}

impl ExecutionClient for RemoteExecClient {
    fn submit(&mut self, request: &OrderRequest) {
        if self.halt_refuses(request) {
            return;
        }
        if !self.enqueue(TradeUp::Submit { cseq: 0, order: request.clone() }) {
            // The link thread has ended: the order never left, so the terminal reject the contract
            // requires is made here. (If even this send fails the core is gone.)
            let reason = self.unavailable();
            self.reject(request, &reason);
        }
    }

    fn cancel(&mut self, client_order_id: &str) {
        // ONE cancel door, so the two can never disagree (the `cancel_with_intent` contract).
        self.cancel_with_intent(client_order_id, CancelIntent::Unspecified);
    }

    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        // Deliberately NOT halt-gated: a kill switch must let an operator reduce/exit.
        let frame =
            TradeUp::Cancel { cseq: 0, coid: client_order_id.to_string(), intent: intent.into() };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            self.cancel_rejected(client_order_id, &reason);
        }
    }

    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        self.cancel_batch_with_intent(client_order_ids, CancelIntent::Unspecified);
    }

    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        if client_order_ids.is_empty() {
            return;
        }
        // Deliberately NOT halt-gated, like the single door. ONE frame: the daemon fans it out per id
        // exactly as `ExecActor` does for a venue with no bulk lane.
        let frame = TradeUp::CancelBatch {
            cseq: 0,
            coids: client_order_ids.to_vec(),
            intent: intent.into(),
        };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            for coid in client_order_ids {
                self.cancel_rejected(coid, &reason);
            }
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        // HALT (conservative, as `ExecActor::modify`): a modify can add size or chase price, so a halt
        // blocks it, and the block is ANNOUNCED with the non-terminal advisory.
        if self.halt_engaged() {
            self.modify_rejected(order, vike_exec::halt::HALT_REJECT_REASON);
            return;
        }
        let frame = TradeUp::Modify { cseq: 0, order: order.clone(), new_qty, new_price };
        if !self.enqueue(frame) {
            let reason = self.unavailable();
            self.modify_rejected(order, &reason);
        }
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        // The HALT gate is per ORDER, exactly as the default fan-out through `submit` would have it;
        // what survives travels as ONE frame (split by the link thread if it is over the ceiling).
        let admitted: Vec<OrderRequest> =
            requests.iter().filter(|r| !self.halt_refuses(r)).cloned().collect();
        if admitted.is_empty() {
            return;
        }
        if !self.enqueue(TradeUp::SubmitBatch { cseq: 0, orders: admitted.clone() }) {
            let reason = self.unavailable();
            for request in &admitted {
                self.reject(request, &reason);
            }
        }
    }

    fn confirm(&mut self, client_order_id: &str) {
        // NOT gated on HALT: a status confirm places no order. The re-query runs at the gatewayhub, off
        // this thread; the venue's authoritative terminal returns as an ordinary event.
        let frame = TradeUp::Confirm { cseq: 0, coid: client_order_id.to_string() };
        if !self.enqueue(frame) {
            tracing::debug!(
                coid = client_order_id,
                "gatewayhub trade: confirm not sent (the lease has ended)"
            );
        }
    }

    /// Phase one: raise the stop and return at once, join nothing. Idempotent with `detach`.
    fn begin_detach(&mut self) {
        self.shared().mark_stopping();
        let _ = self.shared().tx.send(Msg::Detach(DetachPhase::Begin));
    }

    /// Phase two: flush what is queued (the shutdown policy's cancel sweep ran BEFORE phase one and
    /// is already ahead of this in the channel), say goodbye and end the link thread.
    fn detach(&mut self) {
        self.session.detach();
    }

    fn wire_id_for(&self, coid: &str) -> Option<String> {
        let ids = self.shared().wire_ids.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        match ids.lookup(coid) {
            Lookup::Wire(wire) => Some(wire),
            Lookup::Echo => None,
            Lookup::Miss => {
                // An invariant violation: the composition root must prefetch every restored coid
                // before the core is spawned. Loud, and not once per call: the restore asks per
                // restored entry. Once per ACCOUNT's lease (a process-wide flag hid every account's
                // miss after the first).
                if ids.first_miss() {
                    let account = &self.shared().account;
                    tracing::error!(
                        coid, venue = %account.venue, account = ?account.label,
                        "gatewayhub trade: wire_id_for asked about a coid nobody prefetched - the \
                         restore would plan with an incomplete translation map (logged once per \
                         account)"
                    );
                }
                None
            }
        }
    }

    fn adopt_restored(&mut self, orders: &[(String, String)]) {
        if orders.is_empty() {
            return;
        }
        // Fire and forget, queued like `submit` and FIFO with every later command, so a `cancel` sent
        // after this one sees the state. Idempotent: a re-send after a reconnect is harmless.
        let frame = TradeUp::AdoptRestored { cseq: 0, pairs: orders.to_vec() };
        if !self.enqueue(frame) {
            tracing::warn!(
                count = orders.len(),
                "gatewayhub trade: restored orders could not be announced (the lease has ended)"
            );
        }
    }
}
