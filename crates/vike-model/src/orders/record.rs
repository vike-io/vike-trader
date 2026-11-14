//! The OrderStatus FSM and the OrderRecord aggregate.
//!
//! `OrderRecord::apply(event)` is the ONLY mutator of order state: transition-table lookup,
//! `OrderTransitionError` on an illegal edge, fill accumulation (running VWAP).
//! `PARTIALLY_FILLED → CANCELED` and `ACCEPTED → CANCELED` are allowed (server-side OCO
//! sibling-cancel of a partially-filled leg). LIQUIDATED is live (perp force-close);
//! EMULATED/RELEASED are the client-side conditional states.
//!
//! Order STATE is domain vocabulary, so it lives here beside the `OrderRequest` it wraps; the
//! engine that folds events into it is vike-execution's `Engine`.

use crate::events::{Event, FillEvent, OrderSubmitted};
use crate::{ContractValue, OrderRequest, OrderStatusReport, OrderType};
use serde::{Deserialize, Serialize};

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
    /// A WORKING order's status from its fill progress alone, for a venue row that names no status
    /// word: `PartiallyFilled` once any of it filled, else `Accepted`. A resting row that reports its
    /// whole size filled is still `PartiallyFilled`, because the venue still lists it (owner ruling
    /// B-16); a filled order is the venue's end word, or its fill event, to say. A `NaN` reads as
    /// nothing filled.
    #[must_use]
    pub fn from_fill_progress(filled_qty: f64) -> OrderStatus {
        if filled_qty > 0.0 { OrderStatus::PartiallyFilled } else { OrderStatus::Accepted }
    }

    /// A resting/terminal classification: `true` once the order can receive no further lifecycle
    /// events. `Liquidated` is deliberately excluded (per this module's docs it is a live
    /// perp force-close state, not a closed terminal).
    #[inline]
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
    /// arms and `Engine::modify_order`'s pre-send gate all consult it.
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
    /// `OrderTransitionError` → dropped, masked in practice by in-order WS delivery (the accept
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
    /// `transition`'s arm, and `Engine::order_client_id_for_position` (the picker choosing WHICH
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
    #[inline]
    pub fn is_live(&self) -> bool {
        !self.is_terminal() && !matches!(self, OrderStatus::Liquidated)
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
pub struct OrderTransitionError {
    pub order_client_id: String,
    pub status: OrderStatus,
    pub event_name: String,
}

impl std::fmt::Display for OrderTransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: cannot apply {} in {}",
            self.order_client_id,
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
        // not a lifecycle event — Python raises OrderTransitionError too
        Event::Fill(_)
        | Event::OrderModified(_)
        | Event::PositionOpened(_)
        | Event::PositionChanged(_)
        | Event::PositionClosed(_)
        | Event::AccountState(_)
        | Event::Funding(_)
        | Event::PositionLiquidated(_)
        | Event::OrderCancelRejected(_)
        | Event::OrderModifyRejected(_)
        | Event::FeeAdjustment(_) => None,
    }
}

/// An order under management — the request plus its lifecycle state.
/// State changes ONLY via `apply`; `filled_qty`/`average_fill_price` derive from the fill stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRecord {
    pub request: OrderRequest,
    pub status: OrderStatus,
    pub venue_order_id: Option<String>,
    /// In the unit of `request.qty`: for a combo, combo units (`OrderRecord::fold_combo_fill`).
    pub filled_qty: f64,
    /// For a combo, the net per combo unit (`OrderRecord::fold_combo_fill`).
    pub average_fill_price: f64,
    /// Wall-clock (injected `now_ms`) when this order was registered pre-ack, for the audit-C3
    /// stuck-order watchdog. `None` = reconcile-seeded (adopted from a snapshot, never swept).
    pub created_ms: Option<i64>,
    /// The venue's fills and terminals that found this order still `Initialized` (W19g), held for
    /// [`OrderRecord::replay_deferred`] and nothing else: see [`OrderRecord::defer_refused`]. Empty for
    /// every order that took the ordinary road, absent from its JSON then, and left OUT of the engine's
    /// `state_hash` (the engine snapshot's doc), so an older build's recorded hash still fences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_events: Vec<Event>,
    /// Set on every member of a chain of cancel-then-new amends (`vike_venues::venue_amend_mode`): the
    /// order client id the OPERATOR first named, which stays the name of the logical order across every
    /// replacement (the first order of the chain names itself). A cancel or modify naming that id reaches
    /// the live member of the chain (`vike-core`'s `runtime/apply/amend.rs`). `None` for every order that
    /// was never part of one, and then ABSENT from its JSON, so a snapshot and its `state_hash` are
    /// byte-for-byte what they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<String>,
    /// Nobody knows this order's lifetime: it was rebuilt from a venue report, which carries none
    /// ([`Self::from_status_report`]), so `request.time_in_force` is only the type's default and
    /// `request.gtd_expiry` is empty. The restore clears it when the ownership file or the journal
    /// remembers the lifetime (`vike-core`'s `runtime/restore.rs`); until then a modify that would send
    /// that default as the order's own is refused by name (`Engine::modify_order`). False for
    /// every order placed through the engine, and then ABSENT from its JSON.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lifetime_unknown: bool,
    /// A COMBO's running fill per leg, in `request.combo_legs` order: `[qty, qty x average price]`, what
    /// `filled_qty` (combo units) and `average_fill_price` (the net) are derived from. The second is `Σ qty x px`
    /// on a linear leg and the harmonic average times the qty on an inverse one
    /// ([`ContractValue::add_quantity_price`]), so `quantity_price / qty` is the leg's average either way. Empty for every
    /// other order, and then ABSENT from its JSON, so a snapshot and its `state_hash` are what they were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combo_leg_fills: Vec<[f64; 2]>,
}

/// How many refused events one `Initialized` order holds ([`OrderRecord::defer_refused`]); the rest are
/// dropped as they always were, so the buffer is bounded per order whatever the venue sends.
pub const MAX_DEFERRED_EVENTS: usize = 16;

impl OrderRecord {
    pub fn new(request: OrderRequest) -> Self {
        OrderRecord {
            request,
            status: OrderStatus::Initialized,
            venue_order_id: None,
            filled_qty: 0.0,
            average_fill_price: 0.0,
            created_ms: None,
            deferred_events: Vec::new(),
            replaces: None,
            lifetime_unknown: false,
            combo_leg_fills: Vec::new(),
        }
    }

    /// Rebuild an order from a venue's [`OrderStatusReport`] — the ONE construction behind
    /// `Engine::reregister_orders` (adopting an order the registry lost or a previous
    /// session left resting). `None` when the report carries no `order_client_id`. This restores the
    /// order's EXISTENCE, status (as the venue's parser mapped it), filled qty, average price and venue
    /// id, and the terms the venue's row STATED (limit price, trigger price, trailing distance in price
    /// or percent; a non-finite one is dropped); a term the row did not state stays `None`. The report
    /// has no lifetime ([`Self::lifetime_unknown`]).
    /// `created_ms: None`: adopted, never swept by the stuck-order watchdog.
    pub fn from_status_report(r: &OrderStatusReport) -> Option<Self> {
        let order_client_id = r.order_client_id.clone()?;
        let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
        let request = OrderRequest {
            order_client_id,
            venue: r.venue.clone(),
            symbol: r.symbol.clone(),
            side: r.side,
            qty: r.qty,
            // the venue parser's mapping; a kind with no twin among ours stays `Unrecognized`
            order_type: r.order_type.clone(),
            price: finite(r.price),
            trigger_price: finite(r.trigger_price),
            trail: finite(r.trail),
            trail_percent: finite(r.trail_percent),
            ..Default::default()
        };
        Some(OrderRecord {
            request,
            status: r.status,
            venue_order_id: (!r.venue_order_id.is_empty()).then(|| r.venue_order_id.to_string()),
            filled_qty: r.filled_qty,
            average_fill_price: r.average_price,
            created_ms: None,
            deferred_events: Vec::new(),
            replaces: None,
            lifetime_unknown: true,
            combo_leg_fills: Vec::new(),
        })
    }

    pub fn order_client_id(&self) -> &str {
        &self.request.order_client_id
    }

    /// [`Self::apply_valued`] on a contract whose unit is the default linear one: every caller that
    /// knows no contract (the bridges' own order books, the journal materializer, the tests).
    pub fn apply(&mut self, event: &Event) -> Result<(), OrderTransitionError> {
        self.apply_valued(event, ContractValue::default())
    }

    /// Fold one lifecycle event, a fill's price averaged into [`Self::average_fill_price`] by `value`'s
    /// [`ContractValue::average_price`]: arithmetic on a linear contract (the running VWAP, bit for bit
    /// at any unit), harmonic on an inverse one. The engine's fold passes the account's value of the
    /// fill's symbol; nothing else about the transition reads it.
    pub fn apply_valued(
        &mut self,
        event: &Event,
        value: ContractValue,
    ) -> Result<(), OrderTransitionError> {
        // Modify (RUST-NATIVE, no Python twin) is a NON-terminal self-transition: it keeps the
        // current status and rewrites the resting qty/price. Handled here — not via the transition
        // table, which maps to a single fixed target state — and legal only while the order is live
        // and not yet terminal.
        if let Event::OrderModified(am) = event {
            if !self.status.is_modifiable() {
                return Err(OrderTransitionError {
                    order_client_id: self.request.order_client_id.clone(),
                    status: self.status,
                    event_name: "OrderModified".to_string(),
                });
            }
            if let Some(q) = am.new_qty {
                self.request.qty = q;
            }
            // stop trigger vs limit price — modify the field the order actually rests on
            let (limit, trigger) =
                modified_prices(&self.request.order_type, am.new_price, am.new_trigger_price);
            if let Some(p) = limit {
                self.request.price = Some(p);
            }
            if let Some(t) = trigger {
                self.request.trigger_price = Some(t);
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
            // every other event: not this tap's business (audited 2026-10, I-21)
            _ => None,
        };
        if let Some(allowed) = advisory_allowed {
            if !allowed.contains(&self.status) {
                return Err(OrderTransitionError {
                    order_client_id: self.request.order_client_id.clone(),
                    status: self.status,
                    event_name: event.wire_tag().to_string(),
                });
            }
            return Ok(()); // status + terms unchanged
        }
        let Some((allowed, to_status)) = transition(event) else {
            return Err(OrderTransitionError {
                order_client_id: self.request.order_client_id.clone(),
                status: self.status,
                event_name: event.wire_tag().to_string(),
            });
        };
        if !allowed.contains(&self.status) {
            return Err(OrderTransitionError {
                order_client_id: self.request.order_client_id.clone(),
                status: self.status,
                event_name: event.wire_tag().to_string(),
            });
        }
        if let Event::OrderAccepted(a) = event
            && let Some(void) = &a.venue_order_id
        {
            self.venue_order_id = Some(void.to_string());
        }
        let fill = match event {
            Event::OrderPartiallyFilled(w) => Some(&w.fill),
            Event::OrderFilled(w) => Some(&w.fill),
            // every other event: not this tap's business (audited 2026-10, I-21)
            _ => None,
        };
        let to_status = match fill {
            Some(fill) if !self.request.combo_legs.is_empty() => {
                self.fold_combo_fill(fill, to_status == S::Filled, value)
            }
            Some(fill) => {
                self.accumulate_fill(fill, value);
                to_status
            }
            None => to_status,
        };
        self.status = to_status;
        Ok(())
    }

    /// Apply a venue's ANSWER to an order that is still `Initialized` because its `OrderSubmitted` was
    /// lost, by walking the one implied hop first (W19-D1).
    ///
    /// An order whose `OrderSubmitted` never reached the fold, lost with a gateway ring or across a
    /// restart, is `Initialized` while the venue holds it. The venue's answer to an UNKNOWN submit
    /// (`vike_bridge_core::resolve_ambiguous_submit`) is a bare `OrderAccepted`, and the table takes it only
    /// from `Submitted`; the order then stayed `Initialized` until the stuck-order watchdog's last resort
    /// REJECTED it while it rested. For exactly `OrderAccepted` THAT NAMES THE VENUE'S ORDER, and only from
    /// `Initialized`, the missing `OrderSubmitted` is applied first (with the answer's own timestamp), then
    /// the answer.
    ///
    /// ⚠ An accept with **no venue order id** is not that evidence (W19g): ig's `unresolved_confirm` answers one to a
    /// lookup that failed, and the shared bridge code says one ahead of a fill on an order with no answer
    /// (`vike_bridge_core::event_map::accept_implied_by_fill`, which follows its `OrderSubmitted` on the lane). It answers
    /// as `apply` does (refused), so an order that may never have reached the venue stays where the stuck-order
    /// watchdog still sweeps it.
    ///
    /// ⚠ **The transition table is untouched**: it is fixture-pinned (r5 `fsm.json`:
    /// `INITIALIZED + OrderAccepted` is `ok: false`, and the `out_of_order` scenario of `hub.json` has a
    /// fill arrive before `OrderSubmitted` and end `FILLED` with the venue id the later accept carried) and
    /// `apply` stays the one table lookup. This is a separate, explicit door for the fold's COLD error path;
    /// every other event, and every other status, answers exactly as `apply` does (this calls it). A fill, a
    /// cancel or an expiry that finds an `Initialized` order stays refused, but the fold hands it to
    /// [`OrderRecord::defer_refused`] and the answer's landing replays it ([`OrderRecord::replay_deferred`]).
    ///
    /// Err purity holds: a refused event leaves the status as it was found.
    pub fn apply_over_lost_prefix(&mut self, event: &Event) -> Result<(), OrderTransitionError> {
        let Event::OrderAccepted(answer) = event else { return self.apply(event) };
        if self.status != OrderStatus::Initialized || answer.venue_order_id.is_none() {
            return self.apply(event);
        }
        let prefix = Event::OrderSubmitted(OrderSubmitted {
            order_client_id: answer.order_client_id.clone(),
            timestamp: answer.timestamp,
        });
        self.apply(&prefix)?;
        let applied = self.apply(event);
        if applied.is_err() {
            self.status = OrderStatus::Initialized;
        }
        applied
    }

    /// Keep an event the table just refused because the order is still `Initialized`, if it is one the venue
    /// sends only for an order it holds past acceptance: a fill ([`Event::OrderPartiallyFilled`],
    /// [`Event::OrderFilled`]), a cancel or an expiry (W19g). Returns whether it was kept.
    ///
    /// Why: the venue's ANSWER to a lost `OrderSubmitted` (see [`OrderRecord::apply_over_lost_prefix`]) can
    /// arrive AFTER such an event, which the table (r5-pinned) refuses from `Initialized`. Landing the answer
    /// alone would leave the order `Accepted` for ever while the venue holds it filled or gone, a ghost no
    /// sweep ends. The fold keeps the refused event here and [`OrderRecord::replay_deferred`] applies it
    /// when the answer lands: the bare answer above, or an ordinary `OrderSubmitted` then `OrderAccepted`
    /// walk that finds the buffer non-empty (the fill came before `OrderSubmitted`). An order that never
    /// gets an accept never replays it, and the buffer is dropped with the order.
    ///
    /// Bounded ([`MAX_DEFERRED_EVENTS`]); an event already held is not held twice, and neither is a FILL
    /// whose trade id a held fill already carries (W13c: the bridge's reconnect-resync or a reconcile-
    /// synthesized fill for a trade the user stream also delivered has the same id and a different `ts`
    /// or fee, so event equality misses it, and the ordinary fold's `seen_fsm_trade_ids` keeps the FIRST).
    /// No effect unless the status is `Initialized`.
    pub fn defer_refused(&mut self, event: &Event) -> bool {
        if self.status != OrderStatus::Initialized
            || !matches!(
                event,
                Event::OrderPartiallyFilled(_)
                    | Event::OrderFilled(_)
                    | Event::OrderCanceled(_)
                    | Event::OrderExpired(_)
            )
            || self.deferred_events.len() >= MAX_DEFERRED_EVENTS
            || self.deferred_events.iter().any(|held| held == event)
            || fill_trade_id(event).is_some_and(|tid| {
                self.deferred_events.iter().any(|held| fill_trade_id(held) == Some(tid))
            })
        {
            return false;
        }
        self.deferred_events.push(event.clone());
        true
    }

    /// Apply, in the order they arrived, the events [`OrderRecord::defer_refused`] kept, through the
    /// ordinary [`OrderRecord::apply`] (the order is `Accepted` now, so fills and terminals are legal), and
    /// empty the buffer. Returns the events that APPLIED: the fold marks their trade ids and tells the
    /// strategy; one the table still refuses (a second terminal) is dropped, as it would be live.
    ///
    /// **A fill is deduplicated by its trade id exactly as the ordinary fold does** (W13c): `already_seen` is
    /// the engine's `seen_fsm_trade_ids`, and a fill whose id it holds, or that an earlier event of this
    /// same replay already applied, is dropped before it can double `filled_qty` (the ordinary arm checks
    /// the set before `apply` and inserts after a successful one, so the first arrival wins).
    ///
    /// `value_of` answers the contract value of a fill's own SYMBOL, which averages it as
    /// [`Self::apply_valued`] does: a combo leg's row is valued as that leg, never as the combo.
    pub fn replay_deferred(
        &mut self,
        value_of: impl Fn(&str) -> ContractValue,
        already_seen: impl Fn(&str) -> bool,
    ) -> Vec<Event> {
        let held = std::mem::take(&mut self.deferred_events);
        let mut applied: Vec<Event> = Vec::new();
        for event in held {
            if let Some(tid) = fill_trade_id(&event)
                && (already_seen(tid.as_str())
                    || applied.iter().any(|done| fill_trade_id(done) == Some(tid)))
            {
                continue;
            }
            let value = match &event {
                Event::OrderPartiallyFilled(w) => value_of(w.fill.symbol.as_str()),
                Event::OrderFilled(w) => value_of(w.fill.symbol.as_str()),
                // every other event: carries no fill to average
                _ => ContractValue::default(),
            };
            if self.apply_valued(&event, value).is_ok() {
                applied.push(event);
            }
        }
        applied
    }

    /// The running fill VWAP: [`ContractValue::average_price`], whose linear arm is the expression
    /// this always spelled (`(avg·previous + px·q) / new`), so a linear order's bits are unchanged.
    #[inline]
    fn accumulate_fill(&mut self, fill: &FillEvent, value: ContractValue) {
        let previous = self.filled_qty;
        let new = previous + fill.last_qty;
        if new > 0.0 {
            self.average_fill_price = value.average_price(
                previous,
                self.average_fill_price,
                fill.last_qty,
                fill.last_price,
            );
        }
        self.filled_qty = new;
    }

    /// One fill wrap of a COMBO, in the combo's own units (its `qty`); returns the status it leaves the order in.
    ///
    /// A venue reports one combo execution as a row per LEG (paper, deribit) and, on deribit, one more row under
    /// the combo instrument at the net price, every row wrapped under the one order client id. Each leg row adds to that leg's
    /// running qty and `qty × average` ([`Self::combo_leg_fills`]) by `value`, the value of the row's own symbol
    /// ([`ContractValue::add_quantity_price`]: harmonic on an inverse leg, such as a deribit coin-margined future); the
    /// combo-instrument row adds nothing. `filled_qty` is
    /// the units EVERY leg has executed (`min(leg qty / |ratio|)`, so no leg's row order matters) and `average_fill_price`,
    /// once every leg has a fill, the NET per combo unit (`Σ ratio x leg VWAP`, [`crate::combo_net`]'s law): what a
    /// venue's own order report states for a combo (deribit's `filled_amount` and `average_price`).
    ///
    /// A leg row's "filled" never closes the combo by itself: on deribit it is the state of the leg's own child
    /// order (its `order_id` is not the combo's, `combo_fill_fixtures.rs`). The combo closes when its units are
    /// complete on every leg, or on a combo-level `OrderFilled` (a row on no leg, deribit's combo-instrument row:
    /// the combo order's own state); `filled_qty` is then `qty`.
    fn fold_combo_fill(
        &mut self,
        fill: &FillEvent,
        filled_word: bool,
        value: ContractValue,
    ) -> OrderStatus {
        let n = self.request.combo_legs.len();
        let leg = self.request.combo_legs.iter().position(|l| l.symbol == fill.symbol.as_str());
        if let Some(i) = leg {
            if self.combo_leg_fills.len() != n {
                self.combo_leg_fills = vec![[0.0; 2]; n];
            }
            let [held, quantity_price] = self.combo_leg_fills[i];
            self.combo_leg_fills[i] = [
                held + fill.last_qty,
                value.add_quantity_price(held, quantity_price, fill.last_qty, fill.last_price),
            ];
            let legs = self.request.combo_legs.iter().zip(&self.combo_leg_fills);
            self.filled_qty = legs
                .clone()
                .map(|(l, [qty, _])| qty / f64::from(l.ratio.unsigned_abs()))
                .fold(f64::INFINITY, f64::min);
            if self.combo_leg_fills.iter().all(|[qty, _]| *qty > 0.0) {
                self.average_fill_price = legs
                    .map(|(l, [qty, quantity_price])| f64::from(l.ratio) * (quantity_price / qty))
                    .sum();
            }
        }
        // Relative: `|ratio| x qty / |ratio|` need not round back to `qty` exactly.
        let complete = self.filled_qty >= self.request.qty * (1.0 - 1e-9);
        if complete || (filled_word && leg.is_none()) {
            self.filled_qty = self.request.qty;
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        }
    }

    /// Cancel-vs-fill race guard restore site (LEAN `CancelPendingOrders` semantics, reimplemented).
    ///
    /// A locally-issued cancel never mutates status (`Engine::cancel_order` is
    /// fire-and-forget), but an order can sit at `PENDING_CANCEL` via venue-status seeding
    /// (`Engine::reregister_orders` / a recon report carrying the venue's own
    /// `PENDING_CANCEL`), and the transition table — fixture-pinned by r5 `fsm.json`, so it must
    /// NOT be widened — rejects fills from that state and leaves a cancel-reject inert.
    ///
    /// This is the ONE restore: only from `PENDING_CANCEL`, recompute the pre-cancel live status
    /// from the folded fills — `PARTIALLY_FILLED` if any qty filled, else `ACCEPTED` (the venue
    /// could only be pending-cancel on an order it accepted) — and return `true`; any other status
    /// is a no-op returning `false`. A pure function of `(status, filled_qty)`, so journal replay
    /// and the state hash are untouched. Called ONLY by `Engine::on_event`'s cancel-race
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

/// The trade id a fill-carrying lifecycle event names (`None`: the event carries no fill): the key of the
/// FSM-side fill dedup (`seen_fsm_trade_ids`) and of the deferred buffer's.
fn fill_trade_id(event: &Event) -> Option<&crate::events::TradeId> {
    match event {
        Event::OrderPartiallyFilled(w) => Some(&w.fill.trade_id),
        Event::OrderFilled(w) => Some(&w.fill.trade_id),
        // every other event: carries no fill
        _ => None,
    }
}

/// Whether an [`Event::OrderModified`]'s `new_price` targets the order's TRIGGER price
/// ([`OrderType::Stop`] orders rest on their trigger; every other kind, `Unrecognized` included,
/// moves its limit price) rather than the limit price. The single source of this routing:
/// the live FSM ([`OrderRecord::apply`]) and the off-fold journal materializer both consult it, so
/// the durable order log records a stop-modify in the same column the FSM rests it on.
pub fn modified_price_is_trigger(order_type: &OrderType) -> bool {
    *order_type == OrderType::Stop
}

/// Where a modify's two prices land on an order of `order_type`: `(new limit price, new trigger
/// price)`. `new_price` goes where [`modified_price_is_trigger`] says, and an explicit
/// `new_trigger_price` WINS: on a stop, whose one price IS its trigger, the explicit trigger is the
/// new trigger and `new_price` is not read (a stop that gained a limit price would be sent as a
/// stop-limit). A trigger price is the business of a trigger kind ([`OrderType::is_trigger`]) only:
/// on any other kind it lands nowhere, and the doors refuse it before a modify is built.
///
/// A TRAILING kind ([`OrderType::is_trailing`]) is modified in its SIZE only: it states no price, and its
/// trigger price is its activation, which no modify moves, so neither lands on it (the node's door refuses
/// both first, as it refuses a new trail or offset: "cancel it and place a new one").
#[must_use]
pub fn modified_prices(
    order_type: &OrderType,
    new_price: Option<f64>,
    new_trigger_price: Option<f64>,
) -> (Option<f64>, Option<f64>) {
    if modified_price_is_trigger(order_type) {
        (None, new_trigger_price.or(new_price))
    } else if order_type.is_trailing() {
        (None, None)
    } else if order_type.is_trigger() {
        (new_price, new_trigger_price)
    } else {
        (new_price, None)
    }
}

/// The resting `order` as a native amend RE-SENDS it: a clone with `new_qty` as its size, `new_price`
/// in the column [`modified_price_is_trigger`] names (a stop's trigger, every other kind's limit price)
/// and an explicit `new_trigger` as its trigger, which on a stop WINS over `new_price` as it does in
/// [`modified_prices`]. Unlike [`modified_prices`] it judges nothing: the kinds a venue cannot amend,
/// and a trigger on a kind that has none, are the adapter's refusals before it builds the amend.
#[must_use]
pub fn modified_order(
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
    new_trigger: Option<f64>,
) -> OrderRequest {
    let mut amended = order.clone();
    if let Some(q) = new_qty {
        amended.qty = q;
    }
    if let Some(p) = new_price {
        if modified_price_is_trigger(&amended.order_type) {
            amended.trigger_price = Some(p);
        } else {
            amended.price = Some(p);
        }
    }
    if let Some(t) = new_trigger {
        amended.trigger_price = Some(t);
    }
    amended
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

/// `apply_over_lost_prefix`: a venue answer to an order whose first events were lost (W19-D1).
#[cfg(test)]
mod lost_prefix_tests;

/// `accumulate_fill`'s EXACT running-VWAP arithmetic — the companion to `order_state_machine_property_tests.rs`'s
/// `filled_quantity_is_monotone_and_vwap_stays_bounded`, which only bounds `average_fill_price` inside
/// `[min, max]` of the fill prices and cannot tell the weighted mean from any other value there.
#[cfg(test)]
mod fill_accumulation_tests;
