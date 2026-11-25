//! Amends in flight on the socket backend, and the three ways one ends.
//!
//! TWS has no amend verb: an order is amended by PLACING IT AGAIN under its own `orderId`, and the
//! venue answers with an `openOrder` row carrying the terms it now holds (there is no "modified"
//! status). So the bridge cannot know an amend worked from the write alone, and it must not claim
//! one did. A request is therefore kept here from the moment it is written until the venue says
//! something about it:
//!
//! * **confirmed**: an `openOrder` row of this order whose terms are the requested ones (or any other
//!   terms than the ones it had: the venue's are the truth, and they are what `OrderModified`
//!   carries). A row that still shows the OLD terms is an unrelated update (a status change, a
//!   partial fill) that crossed the request on the wire, and confirms nothing;
//! * **refused**: an order-rejection-class error naming the order while the request is pending. It
//!   refuses the AMEND: the order keeps working at its old terms, so it must not reach
//!   [`crate::execution::event_mapper::EventMapper::on_error`]'s terminal;
//! * **unconfirmed**: nothing came within [`AMEND_CONFIRM_WAIT_MS`]. `OrderModifyRejected` says so, and
//!   says the order may nevertheless have been amended (a refusal TWS sent with no order id cannot be
//!   attributed to the request).
//!
//! One amend per order is in flight at a time: a second is refused (non-terminal, not sent) until the first is
//! answered, because the engine's view of the order lags the venue's and two open requests cannot be told
//! apart by their echoes ([`PendingAmends::note`]).
//!
//! Clockless like the mapper that holds it: [`PendingAmends::sweep`] takes `now_ms` and stamps a request
//! at the first sweep that sees it, so the effective wait is the bound to within one sweep interval.

use std::collections::HashMap;

use vike_model::OrderRefusalId;
use vike_model::events::{Event, OrderModified, OrderModifyRejected};

use crate::execution::event_mapper::IbkrOpenOrder;

/// How long an amend may wait for the venue's echo before it is reported unconfirmed.
pub(crate) const AMEND_CONFIRM_WAIT_MS: i64 = 10_000;

/// Prices and quantities from TWS are decimal strings parsed to `f64`, and the request's own came from
/// the same grid, so an exact compare would do; this absorbs a last-digit difference all the same. The cpapi backend's
/// read back of an amend compares the order list's terms with it too.
pub(crate) fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn same_price(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => same(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// One amend written to the venue and not yet answered.
#[derive(Clone, Debug)]
pub(crate) struct PendingAmend {
    pub(crate) order_id: i32,
    /// The order's total quantity and price (the trigger for a stop) BEFORE the request.
    pub(crate) before_qty: f64,
    pub(crate) before_price: Option<f64>,
    /// The same two, as requested.
    pub(crate) new_qty: f64,
    pub(crate) new_price: Option<f64>,
    /// The price is a stop's trigger (TWS `auxPrice`), not a limit price.
    pub(crate) is_stop: bool,
    /// A TRIGGER amend of a stop-limit or a take-profit (`ExecutionClient::modify_trigger`): its trigger (TWS
    /// `auxPrice`) before and as requested, matched on the echo beside the price ([`PendingAmend::with_trigger`]).
    /// `None` for every other amend, which leaves the trigger alone and is matched exactly as before.
    pub(crate) trigger: Option<(Option<f64>, f64)>,
    /// Stamped by the first sweep that sees the request (see the module doc).
    first_seen_ms: Option<i64>,
}

impl PendingAmend {
    pub(crate) fn new(
        order_id: i32,
        before: (f64, Option<f64>),
        requested: (f64, Option<f64>),
        is_stop: bool,
    ) -> Self {
        Self {
            order_id,
            before_qty: before.0,
            before_price: before.1,
            new_qty: requested.0,
            new_price: requested.1,
            is_stop,
            trigger: None,
            first_seen_ms: None,
        }
    }

    /// The request also moves the trigger, from `before` to `new`.
    #[must_use]
    pub(crate) fn with_trigger(mut self, before: Option<f64>, new: f64) -> Self {
        self.trigger = Some((before, new));
        self
    }
}

/// The amends in flight, by order client id.
#[derive(Default)]
pub(crate) struct PendingAmends {
    by_order_client_id: HashMap<String, PendingAmend>,
    /// The terms (quantity, price) the venue last CONFIRMED for an order of ours. The engine's own view of the
    /// order lags the venue's by the event hop, so the "before" of the next amend is taken from here when known:
    /// a request stamped with stale terms would take a late row of the previous terms for proof of itself. The third
    /// term is the trigger an echo showed (`auxPrice`), `None` when no echo said it.
    confirmed: HashMap<String, (f64, Option<f64>, Option<f64>)>,
}

fn modified(
    order_client_id: &str,
    order_id: i32,
    qty: f64,
    price: Option<f64>,
    trigger: Option<f64>,
) -> Event {
    Event::OrderModified(OrderModified {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(order_id.to_string().into()),
        new_qty: Some(qty),
        new_price: price,
        new_trigger_price: trigger,
        // TWS's order rows state no time: the moment the answer was received
        timestamp: vike_bridge_core::rest::amend_answer_time(None),
    })
}

fn rejected(order_client_id: &str, reason: String, refusal: OrderRefusalId) -> Event {
    Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: vike_bridge_core::rest::amend_answer_time(None),
        refusal_id: vike_model::events::refusal_id_of(refusal.word()),
    })
}

impl PendingAmends {
    /// Keep a request that is about to be written, or REFUSE it (`Some(advisory)`, nothing kept, and the caller
    /// must not write it) while an earlier amend of the same order is still unanswered.
    ///
    /// One request in flight per order is the whole design: with two open, the engine still shows the OLD terms
    /// when the second is built, so the first one's echo matches neither "asked" nor "before" of the second and
    /// would be taken for proof of it, and the true echo of the second would then find nothing waiting. A
    /// refused amend costs the operator one retry once the first is answered (at most
    /// [`AMEND_CONFIRM_WAIT_MS`]); a wrong `OrderModified` costs the engine's idea of the order's terms.
    pub(crate) fn note(
        &mut self,
        order_client_id: &str,
        mut pending: PendingAmend,
    ) -> Option<Event> {
        if self.by_order_client_id.contains_key(order_client_id) {
            return Some(rejected(
                order_client_id,
                "ibkr: an earlier amend of this order is still waiting for TWS's confirmation, so this one \
                 was NOT sent (the order keeps the terms it has); amend again once that one is answered"
                    .to_string(),
                OrderRefusalId::AmendRefused,
            ));
        }
        if let Some(&(qty, price, trigger)) = self.confirmed.get(order_client_id) {
            pending.before_qty = qty;
            pending.before_price = price;
            if let (Some((before, _)), Some(t)) = (pending.trigger.as_mut(), trigger) {
                *before = Some(t);
            }
        }
        self.by_order_client_id.insert(order_client_id.to_string(), pending);
        None
    }

    /// The venue vouched for the amend of `order_client_id` without an echo (a backend that answers synchronously): its
    /// terms are the last confirmed ones (its trigger is not said, so the next trigger amend's "before" is the
    /// engine's).
    pub(crate) fn record_confirmed(&mut self, order_client_id: &str, qty: f64, price: Option<f64>) {
        self.confirmed.insert(order_client_id.to_string(), (qty, price, None));
    }

    /// The only order with an amend in flight, when exactly one is: an id-less order-rejection notice that
    /// arrives with nothing else it could belong to is that amend's.
    pub(crate) fn sole_open(&self) -> Option<String> {
        let mut it = self.by_order_client_id.keys();
        match (it.next(), it.next()) {
            (Some(order_client_id), None) => Some(order_client_id.clone()),
            _ => None,
        }
    }

    /// Whether any amend is in flight.
    pub(crate) fn any_open(&self) -> bool {
        !self.by_order_client_id.is_empty()
    }

    /// An id-less order-rejection notice attributed to the amend of `order_client_id` (see [`PendingAmends::sole_open`]):
    /// refuses that amend, saying the attribution is a guess.
    pub(crate) fn refuse_unattributed(
        &mut self,
        order_client_id: &str,
        msg: &str,
    ) -> Option<Event> {
        self.by_order_client_id.remove(order_client_id)?;
        Some(rejected(
            order_client_id,
            format!(
                "ibkr: TWS sent an order rejection that names no order while this amend was in flight and \
                 nothing else was waiting, so it is taken for the amend's refusal: {msg}. The order keeps \
                 its old terms in the platform; if TWS did apply the amend, check it there"
            ),
            OrderRefusalId::VenueRefused,
        ))
    }

    /// Take the request of `order_client_id` out (the write failed, or the backend vouched for it synchronously).
    pub(crate) fn take(&mut self, order_client_id: &str) -> Option<PendingAmend> {
        self.by_order_client_id.remove(order_client_id)
    }

    /// The order ended: an amend of it is moot, and nothing is owed (the engine ignores an advisory for a
    /// finished order).
    pub(crate) fn forget(&mut self, order_client_id: &str) {
        self.confirmed.remove(order_client_id);
        if self.by_order_client_id.remove(order_client_id).is_some() {
            tracing::debug!(
                order_client_id,
                "ibkr: an amend was pending when its order ended; dropped"
            );
        }
    }

    /// Whether an amend of this IB order id is in flight.
    #[cfg(test)]
    pub(crate) fn has_order(&self, order_id: i32) -> bool {
        self.by_order_client_id.values().any(|p| p.order_id == order_id)
    }

    /// An order-rejection-class error named `order_id` while its amend was in flight: it refuses the amend,
    /// not the order. `None` = no amend of that order is pending.
    pub(crate) fn refuse_for_order(&mut self, order_id: i32, msg: &str) -> Option<Event> {
        let order_client_id = self.by_order_client_id.iter().find_map(|(order_client_id, p)| {
            (p.order_id == order_id).then(|| order_client_id.clone())
        })?;
        self.by_order_client_id.remove(&order_client_id);
        Some(rejected(
            &order_client_id,
            format!("ibkr: TWS refused the amend: {msg}"),
            OrderRefusalId::VenueRefused,
        ))
    }

    /// One `openOrder` row of THIS client's order `order_client_id`: it confirms the pending amend of that order, if
    /// there is one and the row speaks for the new terms (see the module doc).
    pub(crate) fn on_row(&mut self, order_client_id: &str, row: &IbkrOpenOrder) -> Option<Event> {
        let p = self.by_order_client_id.get(order_client_id)?;
        if p.order_id != row.order_id {
            return None;
        }
        let row_price = if p.is_stop { row.aux_price } else { row.limit_price };
        // A trigger amend is matched on the trigger too; any other amend reads no trigger, as before.
        let (trigger_asked, trigger_unchanged) = p.trigger.map_or((true, true), |(before, new)| {
            (same_price(row.aux_price, Some(new)), same_price(row.aux_price, before))
        });
        let asked =
            same(row.total_qty, p.new_qty) && same_price(row_price, p.new_price) && trigger_asked;
        let unchanged = same(row.total_qty, p.before_qty)
            && same_price(row_price, p.before_price)
            && trigger_unchanged;
        if !asked && unchanged {
            return None; // a row from before the amend took effect
        }
        let (order_id, qty) = (p.order_id, row.total_qty);
        let price = row_price.or(p.new_price);
        let trigger = p.trigger.map(|(_, new)| row.aux_price.unwrap_or(new));
        // The event carries the venue's terms: the trigger only for a trigger amend, as the echo shows it.
        let reported_trigger = p.trigger.and(row.aux_price);
        self.by_order_client_id.remove(order_client_id);
        self.confirmed.insert(order_client_id.to_string(), (qty, price, trigger.or(row.aux_price)));
        Some(modified(order_client_id, order_id, qty, row_price, reported_trigger))
    }

    /// Close every request that has waited `wait_ms` for an echo that did not come.
    pub(crate) fn sweep(&mut self, now_ms: i64, wait_ms: i64) -> Vec<Event> {
        let mut out = Vec::new();
        self.by_order_client_id.retain(|order_client_id, p| {
            let since = *p.first_seen_ms.get_or_insert(now_ms);
            if now_ms - since < wait_ms {
                return true;
            }
            out.push(rejected(
                order_client_id,
                format!(
                    "ibkr: TWS did not confirm the amend within {}s (no openOrder echo with the new \
                     terms, and no refusal it could attribute to this order). The order may still have \
                     been amended: check it in TWS; the platform keeps its old terms",
                    wait_ms / 1000
                ),
                OrderRefusalId::OutcomeUnknown,
            ));
            false
        });
        out
    }

    /// How many amends are in flight.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_order_client_id.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(order_id: i32, qty: f64, limit: Option<f64>, aux: Option<f64>) -> IbkrOpenOrder {
        IbkrOpenOrder {
            order_id,
            order_ref: "c1".into(),
            total_qty: qty,
            status: "Submitted".into(),
            limit_price: limit,
            aux_price: aux,
            ..Default::default()
        }
    }

    /// A limit order at 100 for 10, asked to go to 101 for 12.
    fn pending_limit() -> PendingAmend {
        PendingAmend::new(105, (10.0, Some(100.0)), (12.0, Some(101.0)), false)
    }

    #[test]
    fn the_venue_row_with_the_new_terms_confirms_and_closes_the_request() {
        let mut p = PendingAmends::default();
        assert!(p.note("c1", pending_limit()).is_none());
        let event = p.on_row("c1", &row(105, 12.0, Some(101.0), None)).expect("confirmed");
        match event {
            Event::OrderModified(m) => {
                assert_eq!(m.order_client_id, "c1");
                assert_eq!(m.venue_order_id.as_deref(), Some("105"));
                assert_eq!(m.new_qty, Some(12.0));
                assert_eq!(m.new_price, Some(101.0));
            }
            other => panic!("expected OrderModified, got {other:?}"),
        }
        assert_eq!(p.len(), 0, "a confirmed request is closed");
        assert!(p.on_row("c1", &row(105, 12.0, Some(101.0), None)).is_none(), "once");
    }

    /// An `openOrder` that crossed the request on the wire still shows the OLD terms: it confirms nothing.
    #[test]
    fn a_row_still_showing_the_old_terms_confirms_nothing() {
        let mut p = PendingAmends::default();
        p.note("c1", pending_limit());
        assert!(p.on_row("c1", &row(105, 10.0, Some(100.0), None)).is_none());
        assert_eq!(p.len(), 1, "the request is still waiting");
    }

    /// TWS holding other terms than asked (it snapped the price to a tick, say) are the truth, and are what
    /// `OrderModified` carries.
    #[test]
    fn terms_the_venue_adjusted_are_reported_as_the_venue_holds_them() {
        let mut p = PendingAmends::default();
        p.note("c1", pending_limit());
        match p.on_row("c1", &row(105, 12.0, Some(100.95), None)) {
            Some(Event::OrderModified(m)) => assert_eq!(m.new_price, Some(100.95)),
            other => panic!("expected OrderModified, got {other:?}"),
        }
    }

    #[test]
    fn a_stop_is_confirmed_on_its_aux_price_not_its_limit_price() {
        let mut p = PendingAmends::default();
        p.note("c1", PendingAmend::new(105, (10.0, Some(180.0)), (10.0, Some(182.0)), true));
        assert!(p.on_row("c1", &row(105, 10.0, None, Some(180.0))).is_none(), "old trigger");
        match p.on_row("c1", &row(105, 10.0, None, Some(182.0))) {
            Some(Event::OrderModified(m)) => assert_eq!(m.new_price, Some(182.0)),
            other => panic!("expected OrderModified, got {other:?}"),
        }
    }

    /// A TRIGGER amend (a limit-if-touched at limit 201 / trigger 200, asked to trigger at 205 with the limit kept) is
    /// confirmed only by a row showing the new trigger as `auxPrice`, and `OrderModified` carries it; a row that still
    /// shows the old trigger confirms nothing. A plain amend says nothing of a trigger, as before.
    #[test]
    fn a_trigger_amend_is_confirmed_on_its_aux_price_and_reports_the_trigger() {
        let mut p = PendingAmends::default();
        let pending = PendingAmend::new(105, (10.0, Some(201.0)), (10.0, Some(201.0)), false)
            .with_trigger(Some(200.0), 205.0);
        assert!(p.note("c1", pending).is_none());
        assert!(p.on_row("c1", &row(105, 10.0, Some(201.0), Some(200.0))).is_none(), "old trigger");
        match p.on_row("c1", &row(105, 10.0, Some(201.0), Some(205.0))) {
            Some(Event::OrderModified(m)) => {
                assert_eq!((m.new_price, m.new_trigger_price), (Some(201.0), Some(205.0)))
            }
            other => panic!("expected OrderModified, got {other:?}"),
        }
        // The next trigger amend takes its "before" trigger from that echo, not from a stale engine view: a late row
        // of the first amend's terms proves nothing of the second.
        let second = PendingAmend::new(105, (10.0, Some(201.0)), (10.0, Some(201.0)), false)
            .with_trigger(Some(200.0), 206.0);
        assert!(p.note("c1", second).is_none());
        assert!(p.on_row("c1", &row(105, 10.0, Some(201.0), Some(205.0))).is_none());
        assert!(matches!(
            p.on_row("c1", &row(105, 10.0, Some(201.0), Some(206.0))),
            Some(Event::OrderModified(m)) if m.new_trigger_price == Some(206.0)
        ));

        p.note("c2", pending_limit());
        match p.on_row("c2", &row(105, 12.0, Some(101.0), Some(99.0))) {
            Some(Event::OrderModified(m)) => assert_eq!(m.new_trigger_price, None, "a plain amend"),
            other => panic!("expected OrderModified, got {other:?}"),
        }
    }

    #[test]
    fn a_row_of_another_order_or_without_a_request_is_ignored() {
        let mut p = PendingAmends::default();
        assert!(p.on_row("c1", &row(105, 12.0, Some(101.0), None)).is_none(), "no request");
        p.note("c1", pending_limit());
        assert!(p.on_row("c1", &row(999, 12.0, Some(101.0), None)).is_none(), "another id");
        assert!(
            p.on_row("c2", &row(105, 12.0, Some(101.0), None)).is_none(),
            "another order client id"
        );
    }

    #[test]
    fn an_unanswered_request_is_reported_unconfirmed_after_the_bound_and_not_before() {
        let mut p = PendingAmends::default();
        p.note("c1", pending_limit());
        assert!(p.sweep(0, 10_000).is_empty(), "the first sweep only stamps it");
        assert!(p.sweep(9_999, 10_000).is_empty());
        let evs = p.sweep(10_000, 10_000);
        match evs.as_slice() {
            [Event::OrderModifyRejected(r)] => {
                assert_eq!(r.order_client_id, "c1");
                assert!(r.reason.contains("did not confirm"), "{}", r.reason);
                assert!(r.reason.contains("may still have been amended"), "{}", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(OrderRefusalId::OutcomeUnknown.word()));
            }
            other => panic!("expected one OrderModifyRejected, got {other:?}"),
        }
        assert_eq!(p.len(), 0);
    }

    #[test]
    fn a_second_amend_is_refused_while_the_first_is_unanswered_and_the_first_stays_valid() {
        let mut p = PendingAmends::default();
        assert!(p.note("c1", pending_limit()).is_none());
        let second = PendingAmend::new(105, (10.0, Some(100.0)), (10.0, Some(102.0)), false);
        match p.note("c1", second) {
            Some(Event::OrderModifyRejected(r)) => {
                assert_eq!(r.order_client_id, "c1");
                assert!(r.reason.contains("NOT sent"), "{}", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(OrderRefusalId::AmendRefused.word()));
            }
            other => panic!("expected the refusal of the second amend, got {other:?}"),
        }
        assert_eq!(p.len(), 1, "only the first is kept");
        // The first one's echo (the only one the venue will send) confirms the FIRST request, with its terms.
        match p.on_row("c1", &row(105, 12.0, Some(101.0), None)) {
            Some(Event::OrderModified(m)) => {
                assert_eq!(m.new_qty, Some(12.0));
                assert_eq!(m.new_price, Some(101.0));
            }
            other => panic!("expected OrderModified of the first amend, got {other:?}"),
        }
        assert_eq!(p.len(), 0);
    }

    /// Once the first is answered the next amend is accepted again, and its "before" is the terms the venue
    /// last CONFIRMED even when the engine's snapshot still showed the original ones (the event hop): so a
    /// late row of the first amend's terms confirms nothing of the second.
    #[test]
    fn the_next_amend_takes_its_before_from_the_last_confirmed_terms() {
        let mut p = PendingAmends::default();
        assert!(p.note("c1", pending_limit()).is_none());
        assert!(p.on_row("c1", &row(105, 12.0, Some(101.0), None)).is_some());
        // Built from the engine's stale snapshot (10 @ 100) although the venue holds 12 @ 101.
        let second = PendingAmend::new(105, (10.0, Some(100.0)), (12.0, Some(102.0)), false);
        assert!(p.note("c1", second).is_none(), "the first is answered: the second is accepted");
        assert!(
            p.on_row("c1", &row(105, 12.0, Some(101.0), None)).is_none(),
            "a late row of the first amend's terms proves nothing of the second"
        );
        assert!(matches!(
            p.on_row("c1", &row(105, 12.0, Some(102.0), None)),
            Some(Event::OrderModified(m)) if m.new_price == Some(102.0)
        ));
    }

    #[test]
    fn an_unattributed_rejection_is_the_amends_only_when_exactly_one_is_open() {
        let mut p = PendingAmends::default();
        assert!(!p.any_open());
        assert!(p.sole_open().is_none());
        p.note("c1", pending_limit());
        assert!(p.any_open());
        assert_eq!(p.sole_open().as_deref(), Some("c1"));
        p.note("c2", PendingAmend::new(106, (1.0, Some(5.0)), (1.0, Some(6.0)), false));
        assert!(p.sole_open().is_none(), "two open: nothing to pick");
        assert!(p.refuse_unattributed("c3", "x").is_none());
        match p.refuse_unattributed("c1", "price") {
            Some(Event::OrderModifyRejected(r)) => {
                assert!(r.reason.contains("price"));
                assert_eq!(r.refusal_id.as_deref(), Some(OrderRefusalId::VenueRefused.word()));
            }
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        }
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn a_rejection_naming_the_order_refuses_the_amend_and_an_ended_order_drops_it() {
        let mut p = PendingAmends::default();
        p.note("c1", pending_limit());
        assert!(p.has_order(105));
        assert!(!p.has_order(106));
        assert!(p.refuse_for_order(106, "x").is_none());
        match p.refuse_for_order(105, "price outside limits") {
            Some(Event::OrderModifyRejected(r)) => {
                assert_eq!(r.order_client_id, "c1");
                assert!(r.reason.contains("price outside limits"));
                assert_eq!(r.refusal_id.as_deref(), Some(OrderRefusalId::VenueRefused.word()));
            }
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        }
        p.note("c1", pending_limit());
        p.forget("c1");
        assert_eq!(p.len(), 0, "the order ended: the amend is moot");
    }
}
