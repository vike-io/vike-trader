//! The OrderStatus FSM and the ManagedOrder aggregate. Exact port of `exec/order.py`.
//!
//! `ManagedOrder::apply(event)` is the ONLY mutator of order state: transition-table lookup,
//! `InvalidOrderTransition` on an illegal edge, fill accumulation (running VWAP).
//! `PARTIALLY_FILLED → CANCELED` and `ACCEPTED → CANCELED` are allowed (server-side OCO
//! sibling-cancel of a partially-filled leg). LIQUIDATED is live (perp force-close);
//! EMULATED/RELEASED are the client-side conditional states (wired by the R7 conditionals port).

use serde::{Deserialize, Serialize};
use vike_model::events::{Event, FillEvent};
use vike_model::OrderRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    Initialized,
    Submitted,
    Accepted,
    Triggered,
    PartiallyFilled,
    Filled,   // terminal
    Canceled, // terminal
    Rejected, // terminal (venue reject)
    Denied,   // terminal (RiskGate veto, pre-venue)
    Expired,  // terminal
    PendingCancel,
    Liquidated, // perp force-close (distinct from CANCELED/FILLED)
    Emulated,   // local conditional held client-side
    Released,   // emulated conditional released to the venue
}

impl OrderStatus {
    /// A resting/terminal classification: `true` once the order can receive no further lifecycle
    /// events. `Liquidated` is deliberately excluded (per this module's docs it is a live
    /// perp force-close state, not a closed terminal).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Canceled
                | OrderStatus::Rejected
                | OrderStatus::Denied
                | OrderStatus::Expired
        )
    }

    /// The modifiable set: the order rests at the venue, so a modify — or the venue's
    /// `OrderModifyRejected` advisory refusing one — is meaningful. The ONE definition of
    /// {ACCEPTED, TRIGGERED, PARTIALLY_FILLED}; the FSM's `OrderModified`/`OrderModifyRejected`
    /// arms and `ExecutionEngine::modify_order`'s pre-send gate all consult it.
    pub const MODIFIABLE: &'static [OrderStatus] =
        &[OrderStatus::Accepted, OrderStatus::Triggered, OrderStatus::PartiallyFilled];

    /// See [`OrderStatus::MODIFIABLE`].
    pub fn is_modifiable(&self) -> bool {
        Self::MODIFIABLE.contains(self)
    }

    /// The FSM's `OrderCanceled` allowed-from set — "may this order RECEIVE `OrderCanceled`?"
    /// (also the `OrderCancelRejected` advisory's allowed-from). The ONE definition of
    /// {ACCEPTED, TRIGGERED, PARTIALLY_FILLED, PENDING_CANCEL}.
    ///
    /// Deliberately a DIFFERENT (narrower) set than the cancel-send gate [`OrderStatus::is_live`],
    /// which answers "is a cancel worth SENDING?" — e.g. a SUBMITTED order is live (a cancel may be
    /// sent), yet a venue-honored cancel of it currently returns `OrderCanceled` →
    /// `InvalidOrderTransition` → dropped, masked in practice by in-order WS delivery (the accept
    /// lands first). Do not merge the two sets; the disagreement is pinned by tests below.
    pub const CAN_RECEIVE_CANCEL: &'static [OrderStatus] = &[
        OrderStatus::Accepted,
        OrderStatus::Triggered,
        OrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel,
    ];

    /// See [`OrderStatus::CAN_RECEIVE_CANCEL`].
    pub fn can_receive_cancel(&self) -> bool {
        Self::CAN_RECEIVE_CANCEL.contains(self)
    }

    /// The FSM's `OrderLiquidated` allowed-from set — "may this order RECEIVE `OrderLiquidated`?"
    /// The ONE definition of {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} for the liquidation lane.
    ///
    /// ⚠ It exists because the question was answered in TWO places that did not agree.
    /// [`transition`]'s `OrderLiquidated` arm spelled this set inline, while
    /// `ExecutionEngine::coid_for_position` — the picker that chooses WHICH order a liquidation
    /// force-closed — selected by NEGATION, skipping only {Liquidated, Filled, Canceled}. That
    /// admits SUBMITTED, PENDING_CANCEL, REJECTED, DENIED and EXPIRED, all of which the FSM refuses.
    ///
    /// The picker scans newest-first, so a still-SUBMITTED order shadows the ACCEPTED one that was
    /// actually force-closed: the pick lands on an order `apply` then rejects, and the real leg
    /// keeps its old status. The account still flattens by key, so positions and PnL stay correct —
    /// this is a stale ORDER STATUS, not money — but the two spellings were free to drift further,
    /// and a third would not have been noticed.
    ///
    /// ⚠ Deliberately its own constant rather than a reuse of [`OrderStatus::MODIFIABLE`], which is
    /// byte-identical TODAY. They answer different questions — "may I amend this?" and "may the
    /// venue force-close this?" — and the repo's own precedent is that a coincidence of membership
    /// is not a reason to share a name (see `CAN_RECEIVE_CANCEL`, kept apart from `is_live` for the
    /// same reason). `the_liquidation_set_matches_the_fsm` pins them equal so the coincidence is
    /// observed rather than assumed.
    pub const CAN_RECEIVE_LIQUIDATION: &'static [OrderStatus] =
        &[OrderStatus::Accepted, OrderStatus::Triggered, OrderStatus::PartiallyFilled];

    /// See [`OrderStatus::CAN_RECEIVE_LIQUIDATION`].
    pub fn can_receive_liquidation(&self) -> bool {
        Self::CAN_RECEIVE_LIQUIDATION.contains(self)
    }

    /// The cancel-SEND gate (the engine's "still worth acting on" liveness test): `true` while a
    /// cancel/confirm addressed to this order could still matter. The done-set it excludes is the
    /// five [`OrderStatus::is_terminal`] statuses PLUS `Liquidated`.
    ///
    /// NOT the same as `!is_terminal()`: they disagree on `Liquidated` BY DESIGN — `Liquidated` is
    /// a live perp force-close state (excluded from `is_terminal`, see its doc) yet never worth
    /// sending a cancel or modify to. And NOT the same question as
    /// [`OrderStatus::can_receive_cancel`] (what the FSM may APPLY) — see that doc for the
    /// intentional divergence. Both disagreements are pinned by tests below.
    pub fn is_live(&self) -> bool {
        !matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Canceled
                | OrderStatus::Rejected
                | OrderStatus::Denied
                | OrderStatus::Expired
                | OrderStatus::Liquidated
        )
    }

    /// Inverse of [`OrderStatus::as_str`] (fixture decoding).
    pub fn parse(s: &str) -> Option<OrderStatus> {
        Some(match s {
            "INITIALIZED" => OrderStatus::Initialized,
            "SUBMITTED" => OrderStatus::Submitted,
            "ACCEPTED" => OrderStatus::Accepted,
            "TRIGGERED" => OrderStatus::Triggered,
            "PARTIALLY_FILLED" => OrderStatus::PartiallyFilled,
            "FILLED" => OrderStatus::Filled,
            "CANCELED" => OrderStatus::Canceled,
            "REJECTED" => OrderStatus::Rejected,
            "DENIED" => OrderStatus::Denied,
            "EXPIRED" => OrderStatus::Expired,
            "PENDING_CANCEL" => OrderStatus::PendingCancel,
            "LIQUIDATED" => OrderStatus::Liquidated,
            "EMULATED" => OrderStatus::Emulated,
            "RELEASED" => OrderStatus::Released,
            _ => return None,
        })
    }

    /// The Python enum value string (fixtures + exec_db `status` column).
    pub fn as_str(&self) -> &'static str {
        match self {
            OrderStatus::Initialized => "INITIALIZED",
            OrderStatus::Submitted => "SUBMITTED",
            OrderStatus::Accepted => "ACCEPTED",
            OrderStatus::Triggered => "TRIGGERED",
            OrderStatus::PartiallyFilled => "PARTIALLY_FILLED",
            OrderStatus::Filled => "FILLED",
            OrderStatus::Canceled => "CANCELED",
            OrderStatus::Rejected => "REJECTED",
            OrderStatus::Denied => "DENIED",
            OrderStatus::Expired => "EXPIRED",
            OrderStatus::PendingCancel => "PENDING_CANCEL",
            OrderStatus::Liquidated => "LIQUIDATED",
            OrderStatus::Emulated => "EMULATED",
            OrderStatus::Released => "RELEASED",
        }
    }
}

/// Raised when `apply` receives an event illegal for the order's current status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidOrderTransition {
    pub client_order_id: String,
    pub status: OrderStatus,
    pub event_name: String,
}

impl std::fmt::Display for InvalidOrderTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: cannot apply {} in {}",
            self.client_order_id,
            self.event_name,
            self.status.as_str()
        )
    }
}

use OrderStatus as S;

/// event → (allowed-from states, resulting state) — the `_TRANSITIONS` table verbatim.
fn transition(event: &Event) -> Option<(&'static [OrderStatus], OrderStatus)> {
    match event {
        Event::OrderSubmitted(_) => Some((&[S::Initialized], S::Submitted)),
        Event::OrderAccepted(_) => Some((&[S::Submitted], S::Accepted)),
        Event::OrderRejected(_) => Some((&[S::Initialized, S::Submitted], S::Rejected)),
        Event::OrderDenied(_) => Some((&[S::Initialized], S::Denied)),
        Event::OrderTriggered(_) => Some((&[S::Accepted], S::Triggered)),
        Event::OrderPartiallyFilled(_) => {
            Some((&[S::Accepted, S::Triggered, S::PartiallyFilled], S::PartiallyFilled))
        }
        Event::OrderFilled(_) => {
            Some((&[S::Accepted, S::Triggered, S::PartiallyFilled], S::Filled))
        }
        Event::OrderCanceled(_) => Some((S::CAN_RECEIVE_CANCEL, S::Canceled)),
        Event::OrderExpired(_) => {
            Some((&[S::Accepted, S::Triggered, S::PartiallyFilled], S::Expired))
        }
        Event::OrderLiquidated(_) => Some((S::CAN_RECEIVE_LIQUIDATION, S::Liquidated)),
        _ => None, // not a lifecycle event — Python raises InvalidOrderTransition too
    }
}

fn event_name(event: &Event) -> &'static str {
    match event {
        Event::Fill(_) => "FillEvent",
        Event::OrderSubmitted(_) => "OrderSubmitted",
        Event::OrderAccepted(_) => "OrderAccepted",
        Event::OrderRejected(_) => "OrderRejected",
        Event::OrderDenied(_) => "OrderDenied",
        Event::OrderTriggered(_) => "OrderTriggered",
        Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
        Event::OrderFilled(_) => "OrderFilled",
        Event::OrderCanceled(_) => "OrderCanceled",
        Event::OrderExpired(_) => "OrderExpired",
        Event::OrderLiquidated(_) => "OrderLiquidated",
        Event::OrderModified(_) => "OrderModified",
        Event::OrderCancelRejected(_) => "OrderCancelRejected",
        Event::OrderModifyRejected(_) => "OrderModifyRejected",
        Event::PositionOpened(_) => "PositionOpened",
        Event::PositionChanged(_) => "PositionChanged",
        Event::PositionClosed(_) => "PositionClosed",
        Event::AccountState(_) => "AccountState",
        Event::Funding(_) => "FundingEvent",
        Event::PositionLiquidated(_) => "PositionLiquidated",
    }
}

/// An order under management — the request plus its lifecycle state.
/// State changes ONLY via `apply`; `filled_qty`/`avg_fill_px` derive from the fill stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManagedOrder {
    pub request: OrderRequest,
    pub status: OrderStatus,
    pub venue_order_id: Option<String>,
    pub filled_qty: f64,
    pub avg_fill_px: f64,
    /// Wall-clock (injected `now_ms`) when this order was registered pre-ack, for the audit-C3
    /// stuck-order watchdog. `None` = reconcile-seeded (adopted from a snapshot, never swept).
    pub created_ms: Option<i64>,
}

impl ManagedOrder {
    pub fn new(request: OrderRequest) -> Self {
        ManagedOrder {
            request,
            status: OrderStatus::Initialized,
            venue_order_id: None,
            filled_qty: 0.0,
            avg_fill_px: 0.0,
            created_ms: None,
        }
    }

    pub fn client_order_id(&self) -> &str {
        &self.request.client_order_id
    }

    pub fn apply(&mut self, event: &Event) -> Result<(), InvalidOrderTransition> {
        // Modify (RUST-NATIVE, no Python twin) is a NON-terminal self-transition: it keeps the
        // current status and rewrites the resting qty/price. Handled here — not via the transition
        // table, which maps to a single fixed target state — and legal only while the order is live
        // and not yet terminal.
        if let Event::OrderModified(am) = event {
            if !self.status.is_modifiable() {
                return Err(InvalidOrderTransition {
                    client_order_id: self.request.client_order_id.clone(),
                    status: self.status,
                    event_name: "OrderModified".to_string(),
                });
            }
            if let Some(q) = am.new_qty {
                self.request.qty = q;
            }
            if let Some(p) = am.new_price {
                // stop trigger vs limit price — modify the field the order actually rests on
                if modified_price_is_trigger(&self.request.order_type) {
                    self.request.trigger_price = Some(p);
                } else {
                    self.request.price = Some(p);
                }
            }
            if let Some(void) = &am.venue_order_id {
                self.venue_order_id = Some(void.to_string());
            }
            return Ok(()); // status unchanged
        }
        // Cancel/modify-reject (RUST-NATIVE, no Python twin) are NON-terminal advisories: the order
        // stays live and keeps its terms. Legal only while the request they refuse could have been
        // in flight (cancelable/modifiable states) — a reject in a terminal/pre-accept state is an
        // out-of-order artifact and errors (dropped as idempotent at the on_event fold).
        let advisory_allowed: Option<&[OrderStatus]> = match event {
            Event::OrderCancelRejected(_) => Some(S::CAN_RECEIVE_CANCEL),
            Event::OrderModifyRejected(_) => Some(S::MODIFIABLE),
            _ => None,
        };
        if let Some(allowed) = advisory_allowed {
            if !allowed.contains(&self.status) {
                return Err(InvalidOrderTransition {
                    client_order_id: self.request.client_order_id.clone(),
                    status: self.status,
                    event_name: event_name(event).to_string(),
                });
            }
            return Ok(()); // status + terms unchanged
        }
        let Some((allowed, to_status)) = transition(event) else {
            return Err(InvalidOrderTransition {
                client_order_id: self.request.client_order_id.clone(),
                status: self.status,
                event_name: event_name(event).to_string(),
            });
        };
        if !allowed.contains(&self.status) {
            return Err(InvalidOrderTransition {
                client_order_id: self.request.client_order_id.clone(),
                status: self.status,
                event_name: event_name(event).to_string(),
            });
        }
        if let Event::OrderAccepted(a) = event {
            if let Some(void) = &a.venue_order_id {
                self.venue_order_id = Some(void.to_string());
            }
        }
        match event {
            Event::OrderPartiallyFilled(w) => self.accumulate_fill(&w.fill),
            Event::OrderFilled(w) => self.accumulate_fill(&w.fill),
            _ => {}
        }
        self.status = to_status;
        Ok(())
    }

    fn accumulate_fill(&mut self, fill: &FillEvent) {
        let prev = self.filled_qty;
        let new = prev + fill.last_qty;
        if new > 0.0 {
            self.avg_fill_px = (self.avg_fill_px * prev + fill.last_px * fill.last_qty) / new;
        }
        self.filled_qty = new;
    }

    /// Cancel-vs-fill race guard restore site (LEAN `CancelPendingOrders` semantics, reimplemented).
    ///
    /// A locally-issued cancel never mutates status (`ExecutionEngine::cancel_order` is
    /// fire-and-forget), so for the live path the pre-cancel status IS the snapshot — nothing to
    /// restore. But an order can sit at `PENDING_CANCEL` via venue-status seeding
    /// (`ExecutionEngine::reregister_orders` / a recon report parsing the venue's own
    /// `PENDING_CANCEL` string), and the transition table above — fixture-pinned by r5 `fsm.json`,
    /// so it must NOT be widened — rejects fills from that state and leaves a cancel-reject inert.
    ///
    /// This method is the ONE restore: when (and only when) the order is `PENDING_CANCEL`, recompute
    /// the pre-cancel live status from the folded fill stream — `PARTIALLY_FILLED` if any qty has
    /// filled, else `ACCEPTED` (the venue could only be pending-cancel on an order it accepted) —
    /// and return `true`. Any other status is a no-op returning `false`. A pure function of
    /// `(status, filled_qty)`: no new state, so journal replay and `EngineSnapshot`/state-hash
    /// determinism are untouched. Called ONLY by the engine fold's cancel-race guard
    /// (`ExecutionEngine::on_event`) immediately before `apply`, for the three events that prove the
    /// cancel lost or died: a fill wrap (`OrderPartiallyFilled`/`OrderFilled`) or the venue's
    /// `OrderCancelRejected`. `OrderCanceled` never needs it — `PENDING_CANCEL → CANCELED` is
    /// already a legal edge (the ack path).
    pub fn resolve_pending_cancel(&mut self) -> bool {
        if self.status != OrderStatus::PendingCancel {
            return false;
        }
        self.status = if self.filled_qty > 0.0 {
            OrderStatus::PartiallyFilled
        } else {
            OrderStatus::Accepted
        };
        true
    }
}

/// Whether an [`Event::OrderModified`]'s `new_price` targets the order's TRIGGER price (`"stop"`
/// orders rest on their trigger) rather than the limit price. The single source of this routing:
/// the live FSM ([`ManagedOrder::apply`]) and the off-fold journal materializer both consult it, so
/// the durable order log records a stop-modify in the same column the FSM rests it on.
pub fn modified_price_is_trigger(order_type: &str) -> bool {
    order_type == "stop"
}

#[cfg(test)]
mod modify_tests {
    use super::*;
    use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

    fn limit_order() -> ManagedOrder {
        ManagedOrder::new(OrderRequest {
            client_order_id: "c".into(),
            venue: "v".into(),
            symbol: "s".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })
    }

    fn submitted(coid: &str) -> Event {
        Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.into(), ts: 0 })
    }
    fn accepted(coid: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: None,
            ts: 0,
        })
    }
    fn modify(coid: &str, q: Option<f64>, p: Option<f64>) -> Event {
        Event::OrderModified(OrderModified {
            client_order_id: coid.into(),
            venue_order_id: None,
            new_qty: q,
            new_price: p,
            ts: 0,
        })
    }

    fn cancel_rejected(coid: &str) -> Event {
        Event::OrderCancelRejected(vike_model::events::OrderCancelRejected {
            client_order_id: coid.into(),
            reason: "network error".into(),
            ts: 0,
        })
    }
    fn modify_rejected(coid: &str) -> Event {
        Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
            client_order_id: coid.into(),
            reason: "venue error".into(),
            ts: 0,
        })
    }

    #[test]
    fn cancel_rejected_is_advisory_and_keeps_status() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&accepted("c")).unwrap();
        o.apply(&cancel_rejected("c")).unwrap();
        assert_eq!(
            o.status,
            OrderStatus::Accepted,
            "cancel-reject is non-terminal — the order stays live"
        );
    }

    #[test]
    fn cancel_rejected_before_accept_is_error() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap(); // not cancelable at the venue yet
        assert!(o.apply(&cancel_rejected("c")).is_err());
    }

    #[test]
    fn cancel_rejected_after_terminal_is_error() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&Event::OrderRejected(vike_model::events::OrderRejected {
            client_order_id: "c".into(),
            reason: "x".into(),
            ts: 0,
        }))
        .unwrap();
        assert!(o.apply(&cancel_rejected("c")).is_err(), "no reject after terminal");
    }

    #[test]
    fn modify_rejected_is_advisory_and_keeps_terms() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&accepted("c")).unwrap();
        o.apply(&modify_rejected("c")).unwrap();
        assert_eq!(o.status, OrderStatus::Accepted, "modify-reject is non-terminal");
        assert_eq!(o.request.qty, 1.0, "terms untouched");
        assert_eq!(o.request.price, Some(100.0));
    }

    #[test]
    fn modify_rewrites_resting_terms_and_keeps_status() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&accepted("c")).unwrap();
        o.apply(&modify("c", Some(2.0), Some(101.0))).unwrap();
        assert_eq!(o.status, OrderStatus::Accepted, "modify is a self-transition");
        assert_eq!(o.request.qty, 2.0);
        assert_eq!(o.request.price, Some(101.0));
    }

    #[test]
    fn modify_partial_none_fields_leave_terms_unchanged() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&accepted("c")).unwrap();
        o.apply(&modify("c", None, Some(105.0))).unwrap(); // price-only re-quote
        assert_eq!(o.request.qty, 1.0, "qty untouched when new_qty is None");
        assert_eq!(o.request.price, Some(105.0));
    }

    #[test]
    fn modify_rejected_before_accept() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap(); // SUBMITTED — not yet resting at the venue
        assert!(o.apply(&modify("c", Some(2.0), None)).is_err());
        assert_eq!(o.request.qty, 1.0, "rejected modify must not mutate terms");
    }

    #[test]
    fn modify_rejected_when_terminal() {
        let mut o = limit_order();
        o.apply(&submitted("c")).unwrap();
        o.apply(&Event::OrderRejected(vike_model::events::OrderRejected {
            client_order_id: "c".into(),
            reason: "x".into(),
            ts: 0,
        }))
        .unwrap();
        assert_eq!(o.status, OrderStatus::Rejected);
        assert!(o.apply(&modify("c", Some(2.0), Some(9.0))).is_err());
    }

    #[test]
    fn resolve_pending_cancel_restores_accepted_when_nothing_filled() {
        let mut o = limit_order();
        o.status = OrderStatus::PendingCancel; // venue-status seeded (reregister/recon)
        assert!(o.resolve_pending_cancel(), "PENDING_CANCEL is restored");
        assert_eq!(o.status, OrderStatus::Accepted, "no fills folded → ACCEPTED");
    }

    #[test]
    fn resolve_pending_cancel_restores_partially_filled_when_qty_folded() {
        let mut o = limit_order();
        o.status = OrderStatus::PendingCancel;
        o.filled_qty = 0.5; // fills arrived while the cancel was pending
        o.avg_fill_px = 100.0;
        assert!(o.resolve_pending_cancel());
        assert_eq!(
            o.status,
            OrderStatus::PartiallyFilled,
            "recomputed from the folded fill stream"
        );
        assert_eq!(o.filled_qty, 0.5, "fill accumulation untouched");
        assert_eq!(o.avg_fill_px, 100.0);
    }

    #[test]
    fn resolve_pending_cancel_is_a_noop_on_any_other_status() {
        for status in [
            OrderStatus::Initialized,
            OrderStatus::Submitted,
            OrderStatus::Accepted,
            OrderStatus::Triggered,
            OrderStatus::PartiallyFilled,
            OrderStatus::Filled,
            OrderStatus::Canceled,
            OrderStatus::Rejected,
        ] {
            let mut o = limit_order();
            o.status = status;
            assert!(!o.resolve_pending_cancel(), "no restore from {}", status.as_str());
            assert_eq!(o.status, status, "status untouched for {}", status.as_str());
        }
    }

    #[test]
    fn modify_of_stop_moves_the_trigger_not_the_limit() {
        let mut o = ManagedOrder::new(OrderRequest {
            client_order_id: "c".into(),
            venue: "v".into(),
            symbol: "s".into(),
            side: 1,
            qty: 1.0,
            order_type: "stop".into(),
            trigger_price: Some(100.0),
            ..Default::default()
        });
        o.apply(&submitted("c")).unwrap();
        o.apply(&accepted("c")).unwrap();
        o.apply(&modify("c", None, Some(110.0))).unwrap();
        assert_eq!(o.request.trigger_price, Some(110.0));
        assert_eq!(o.request.price, None, "limit price stays unset for a stop");
    }
}

/// Pins the INTENTIONAL disagreements between the named state-sets, so a future "cleanup" that
/// merges them (e.g. `is_live` → `!is_terminal`, or `is_live` ↔ `can_receive_cancel`) fails loudly
/// instead of silently re-opening the Liquidated cancel/modify path or the Submitted cancel gap.
#[cfg(test)]
mod state_set_tests {
    use super::*;

    const ALL: &[OrderStatus] = &[
        S::Initialized,
        S::Submitted,
        S::Accepted,
        S::Triggered,
        S::PartiallyFilled,
        S::Filled,
        S::Canceled,
        S::Rejected,
        S::Denied,
        S::Expired,
        S::PendingCancel,
        S::Liquidated,
        S::Emulated,
        S::Released,
    ];

    /// PIN: `is_live()` is NOT `!is_terminal()` — they disagree on exactly `Liquidated`, BY DESIGN
    /// (`Liquidated` is a live perp force-close excluded from `is_terminal`, yet never worth a
    /// cancel/modify). Do not "simplify" one into the other.
    #[test]
    fn is_live_is_not_the_negation_of_is_terminal_they_disagree_on_liquidated() {
        assert!(
            !OrderStatus::Liquidated.is_terminal(),
            "Liquidated is non-terminal BY DESIGN (perp force-close is a live state)"
        );
        assert!(
            !OrderStatus::Liquidated.is_live(),
            "Liquidated is NOT live for the cancel-send gate — never worth canceling/modifying"
        );
        for s in ALL {
            let expected = !s.is_terminal() && *s != OrderStatus::Liquidated;
            assert_eq!(
                s.is_live(),
                expected,
                "{}: is_live must equal !is_terminal for every status EXCEPT Liquidated",
                s.as_str()
            );
        }
    }

    /// PIN: the cancel-SEND gate (`is_live`) and the FSM's `OrderCanceled` allowed-from set
    /// (`can_receive_cancel`) answer DIFFERENT questions and are intentionally different sets:
    /// INITIALIZED/SUBMITTED/EMULATED/RELEASED are live (a cancel may be worth sending) yet may NOT
    /// receive `OrderCanceled` — the documented gap being a venue-honored cancel of a SUBMITTED
    /// order (OrderCanceled → InvalidOrderTransition → dropped, masked by in-order WS delivery).
    /// This test fails if the two sets are ever merged.
    #[test]
    fn cancel_send_gate_and_can_receive_cancel_are_distinct_sets() {
        // The FSM allowed-from set, exactly.
        assert_eq!(
            OrderStatus::CAN_RECEIVE_CANCEL,
            &[S::Accepted, S::Triggered, S::PartiallyFilled, S::PendingCancel],
            "OrderCanceled allowed-from set must not change in a naming-only refactor"
        );
        // Strictly narrower than the send gate: everything cancelable is live…
        for s in OrderStatus::CAN_RECEIVE_CANCEL {
            // The method's POSITIVE direction: every member of its own set must answer true, so a
            // body mutated to `false` cannot pass by satisfying only the negative assertions below.
            assert!(
                s.can_receive_cancel(),
                "{}: can_receive_cancel must return true for every status in CAN_RECEIVE_CANCEL",
                s.as_str()
            );
            assert!(s.is_live(), "{}: can_receive_cancel ⊆ is_live", s.as_str());
        }
        // …but NOT vice versa — the designed divergence, per status.
        for s in [S::Initialized, S::Submitted, S::Emulated, S::Released] {
            assert!(s.is_live(), "{}: live (cancel worth sending)", s.as_str());
            assert!(
                !s.can_receive_cancel(),
                "{}: yet may NOT receive OrderCanceled (the sets must stay distinct)",
                s.as_str()
            );
        }
    }

    /// The modifiable set, exactly — {ACCEPTED, TRIGGERED, PARTIALLY_FILLED}; a naming-only
    /// refactor must not change membership.
    #[test]
    fn modifiable_set_membership_is_pinned() {
        assert_eq!(OrderStatus::MODIFIABLE, &[S::Accepted, S::Triggered, S::PartiallyFilled]);
        for s in ALL {
            assert_eq!(s.is_modifiable(), OrderStatus::MODIFIABLE.contains(s), "{}", s.as_str());
        }
    }
}

#[cfg(test)]
mod liquidation_predicate_tests {
    use super::*;
    use crate::order::OrderStatus as S;

    /// ⚠ **The picker and the applier must be ONE predicate.**
    ///
    /// "Which registry order is still alive enough to be the leg a liquidation force-closed?" used
    /// to be answered in two places that disagreed: `transition`'s `OrderLiquidated` arm spelled
    /// {ACCEPTED, TRIGGERED, PARTIALLY_FILLED} inline, while
    /// `ExecutionEngine::coid_for_position` selected by NEGATION, skipping only
    /// {Liquidated, Filled, Canceled}.
    ///
    /// This asserts the five statuses that gap admitted — each one an order the picker WOULD have
    /// returned and the FSM then refused. Because the scan is newest-first, any of them shadows the
    /// ACCEPTED order that was actually force-closed, which then keeps its old status.
    #[test]
    fn the_picker_admits_nothing_the_fsm_refuses() {
        for s in [S::Submitted, S::PendingCancel, S::Rejected, S::Denied, S::Expired] {
            assert!(
                !s.can_receive_liquidation(),
                "{}: the OLD negated skip set admitted this, and the FSM refuses it — selecting on \
                 it shadows the real liquidated leg",
                s.as_str()
            );
        }
        // ...and the three that ARE legal still are, so this is not a blanket refusal.
        for s in [S::Accepted, S::Triggered, S::PartiallyFilled] {
            assert!(s.can_receive_liquidation(), "{}: must stay selectable", s.as_str());
        }
    }

    /// The constant IS the FSM's allowed-from row, verbatim — a naming-only refactor cannot move it.
    #[test]
    fn the_liquidation_set_matches_the_fsm() {
        assert_eq!(
            OrderStatus::CAN_RECEIVE_LIQUIDATION,
            &[S::Accepted, S::Triggered, S::PartiallyFilled],
            "OrderLiquidated allowed-from set must not change in a naming-only refactor"
        );
    }

    /// ⚠ `CAN_RECEIVE_LIQUIDATION` and `MODIFIABLE` are byte-identical TODAY and are deliberately
    /// SEPARATE constants — they answer different questions ("may the venue force-close this?" vs
    /// "may I amend this?"). This pins the coincidence so it is OBSERVED rather than assumed: if
    /// one set ever moves, this test fails and the author decides whether the other should follow,
    /// instead of a shared name silently deciding for them.
    ///
    /// The same reasoning keeps `CAN_RECEIVE_CANCEL` apart from `is_live`, pinned above.
    #[test]
    fn the_liquidation_and_modifiable_sets_coincide_today_and_that_is_pinned_not_shared() {
        assert_eq!(
            OrderStatus::CAN_RECEIVE_LIQUIDATION,
            OrderStatus::MODIFIABLE,
            "these coincide today; if one moves, decide about the other rather than merging them"
        );
    }
}

/// Targeted unit coverage for `accumulate_fill`'s running VWAP -- the mutation-lane companion to
/// `order_fsm_props.rs`'s property-based `filled_qty_is_monotone_and_vwap_stays_bounded`, which
/// only bounds `avg_fill_px` inside `[min, max]` of folded fill prices and so cannot distinguish
/// the correct weighted mean from every other value the interval admits. These pin the EXACT
/// arithmetic instead.
#[cfg(test)]
mod fill_accumulation_tests {
    use super::*;
    use vike_model::events::{OrderAccepted, OrderFilled, OrderPartiallyFilled, OrderSubmitted};

    fn order_with_qty(qty: f64) -> ManagedOrder {
        ManagedOrder::new(OrderRequest {
            client_order_id: "c".into(),
            venue: "v".into(),
            symbol: "s".into(),
            side: 1,
            qty,
            order_type: "limit".into(),
            price: Some(100.0),
            ..Default::default()
        })
    }

    fn submitted() -> Event {
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "c".into(), ts: 0 })
    }

    fn accepted() -> Event {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: "c".into(),
            venue_order_id: None,
            ts: 0,
        })
    }

    fn fill(trade_id: &'static str, qty: f64, px: f64) -> FillEvent {
        FillEvent {
            trade_id: trade_id.into(),
            client_order_id: "c".into(),
            venue: "v".into(),
            symbol: "s".into(),
            side: 1,
            last_qty: qty,
            last_px: px,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            ts: 0,
            mark_price: None,
            position_side: "BOTH".into(),
        }
    }

    fn partially_filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
        Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: "c".into(),
            fill: fill(trade_id, qty, px),
            ts: 0,
        })
    }

    fn filled(trade_id: &'static str, qty: f64, px: f64) -> Event {
        Event::OrderFilled(OrderFilled {
            client_order_id: "c".into(),
            fill: fill(trade_id, qty, px),
            ts: 0,
        })
    }

    /// The documented law itself: after N partial fills, `avg_fill_px` is the QUANTITY-WEIGHTED
    /// mean of every fill price folded so far, and `filled_qty` is their sum. Three fills with
    /// DIFFERENT prices AND different quantities -- a single fill, equal quantities, or equal
    /// prices cannot distinguish this from `(sum of prices) / count` or any other wrong fold --
    /// exercising exactly the running update `accumulate_fill` performs:
    /// `(avg_fill_px * prev + last_px * last_qty) / new`.
    #[test]
    fn running_vwap_is_the_quantity_weighted_mean_of_all_fills() {
        let mut o = order_with_qty(10.0);
        o.apply(&submitted()).unwrap();
        o.apply(&accepted()).unwrap();

        o.apply(&partially_filled("t1", 2.0, 100.0)).unwrap();
        assert_eq!(o.filled_qty, 2.0);
        assert_eq!(o.avg_fill_px, 100.0, "first fill: VWAP is just its own price");

        o.apply(&partially_filled("t2", 6.0, 110.0)).unwrap();
        assert_eq!(o.filled_qty, 8.0);
        // (100*2 + 110*6) / 8 = 860 / 8 = 107.5
        assert_eq!(o.avg_fill_px, 107.5, "VWAP must weight by quantity, not average prices flat");

        o.apply(&filled("t3", 2.0, 90.0)).unwrap();
        assert_eq!(o.filled_qty, 10.0, "filled_qty is the running SUM of fill quantities");
        // (107.5*8 + 90*2) / 10 = (860 + 180) / 10 = 1040 / 10 = 104.0
        assert_eq!(o.avg_fill_px, 104.0);
        assert_eq!(o.status, OrderStatus::Filled);
    }

    /// The `if new > 0.0` guard's own boundary: a fill that leaves `filled_qty` at exactly zero
    /// -- `prev == 0.0` and `fill.last_qty == 0.0` so `new == 0.0` -- must SKIP the VWAP update
    /// rather than divide zero by zero. `>` correctly skips it (`0.0 > 0.0` is false); a mutant
    /// `>=` would execute the division, compute `0.0 / 0.0 == NaN`, and poison `avg_fill_px`
    /// forever after (NaN contaminates every later `avg_fill_px * prev` term).
    #[test]
    fn zero_quantity_fill_at_zero_prior_qty_does_not_divide_by_zero() {
        let mut o = order_with_qty(10.0);
        o.apply(&submitted()).unwrap();
        o.apply(&accepted()).unwrap();

        o.apply(&partially_filled("t1", 0.0, 555.0)).unwrap();

        assert_eq!(o.filled_qty, 0.0, "a zero-qty fill folds no quantity");
        assert_eq!(
            o.avg_fill_px, 0.0,
            "the guard must skip the update, not divide 0.0/0.0 into NaN"
        );
        assert!(!o.avg_fill_px.is_nan(), "avg_fill_px must never become NaN");
    }
}
