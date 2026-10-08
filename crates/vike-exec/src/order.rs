//! The OrderStatus FSM and the ManagedOrder aggregate.
//!
//! `ManagedOrder::apply(event)` is the ONLY mutator of order state: transition-table lookup,
//! `InvalidOrderTransition` on an illegal edge, fill accumulation (running VWAP).
//! `PARTIALLY_FILLED → CANCELED` and `ACCEPTED → CANCELED` are allowed (server-side OCO
//! sibling-cancel of a partially-filled leg). LIQUIDATED is live (perp force-close);
//! EMULATED/RELEASED are the client-side conditional states.

use serde::{Deserialize, Serialize};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

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
    /// ⚠ It exists because the question was answered in TWO places that did not agree:
    /// `transition`'s arm, and `ExecutionEngine::coid_for_position` (the picker choosing WHICH
    /// order a liquidation force-closed), which selected by NEGATION and so admitted SUBMITTED,
    /// PENDING_CANCEL, REJECTED, DENIED and EXPIRED — all refused by the FSM. Scanning newest-first,
    /// a still-SUBMITTED order shadowed the ACCEPTED one actually force-closed, leaving it a stale
    /// ORDER STATUS (positions and PnL stay correct: the account flattens by key).
    ///
    /// ⚠ Deliberately its own constant rather than a reuse of [`OrderStatus::MODIFIABLE`], which is
    /// byte-identical TODAY: they answer different questions ("may I amend this?" vs "may the venue
    /// force-close this?"), as `CAN_RECEIVE_CANCEL` is kept apart from `is_live`.
    /// `the_liquidation_and_modifiable_sets_coincide_today_and_that_is_pinned_not_shared` pins them
    /// equal so the coincidence is observed rather than assumed.
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
        if let Event::OrderAccepted(a) = event
            && let Some(void) = &a.venue_order_id
        {
            self.venue_order_id = Some(void.to_string());
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
    /// fire-and-forget), but an order can sit at `PENDING_CANCEL` via venue-status seeding
    /// (`ExecutionEngine::reregister_orders` / a recon report carrying the venue's own
    /// `PENDING_CANCEL`), and the transition table — fixture-pinned by r5 `fsm.json`, so it must
    /// NOT be widened — rejects fills from that state and leaves a cancel-reject inert.
    ///
    /// This is the ONE restore: only from `PENDING_CANCEL`, recompute the pre-cancel live status
    /// from the folded fills — `PARTIALLY_FILLED` if any qty filled, else `ACCEPTED` (the venue
    /// could only be pending-cancel on an order it accepted) — and return `true`; any other status
    /// is a no-op returning `false`. A pure function of `(status, filled_qty)`, so journal replay
    /// and the state hash are untouched. Called ONLY by `ExecutionEngine::on_event`'s cancel-race
    /// guard right before `apply`, for the events proving the cancel lost or died (a fill wrap or
    /// `OrderCancelRejected`); `PENDING_CANCEL → CANCELED` is already a legal edge (the ack path).
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
mod modify_tests;

/// Pins the INTENTIONAL disagreements between the named state-sets, so a future "cleanup" that
/// merges them (e.g. `is_live` → `!is_terminal`, or `is_live` ↔ `can_receive_cancel`) fails loudly
/// instead of silently re-opening the Liquidated cancel/modify path or the Submitted cancel gap.
#[cfg(test)]
mod state_set_tests;

#[cfg(test)]
mod liquidation_predicate_tests;

/// `accumulate_fill`'s EXACT running-VWAP arithmetic — the companion to `order_fsm_props.rs`'s
/// `filled_qty_is_monotone_and_vwap_stays_bounded`, which only bounds `avg_fill_px` inside
/// `[min, max]` of the fill prices and cannot tell the weighted mean from any other value there.
#[cfg(test)]
mod fill_accumulation_tests;
