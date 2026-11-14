//! The IBKR id model — the one genuinely new cost vs vike's order-client-id-native venues. IB assigns a
//! NUMERIC `orderId` (from `nextValidId`) and reports status keyed by it; vike is `order_client_id`
//! (order client id) native. So the bridge owns an order client id⇄orderId map, carries the order client id in IB's `orderRef`, and
//! resolves inbound events by orderId with an orderRef fallback (dual resolution). The
//! `next_order_id` floor `max(stored, incoming, 101)` and the "connected only when a valid id AND
//! the account list are present" readiness gate follow the same reference-adapter pattern.
//!
//! `OrderVenueIdAllocator` is consumed by the event mapper (Task 7) and the exec loop (Task 8, `execution::run_execution`
//! resolves/binds through the mapper's registry).

use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;

/// What [`OrderVenueIdAllocator::bind_rebound`] did with a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rebound {
    /// A new binding.
    Bound,
    /// This connection already held exactly this binding.
    Known,
    /// The row contradicts a binding this connection holds; nothing changed.
    Conflict,
}

pub(crate) struct OrderVenueIdAllocator {
    next_id: i32,
    have_id: bool,
    accounts_ready: bool,
    /// order client id <-> orderId, the shared registry under IB's rule: an order client id bound again under a new orderId leaves the old id
    /// still naming it ([`OrderVenueIdRegistry::keeping_stale_reverse`]).
    ids: OrderVenueIdRegistry<i32>,
    /// The transport's own diagnostic, set once the inbound stream has terminated for good
    /// (`IbkrInbound::StreamDead`). `Some` is the latch — see [`OrderVenueIdAllocator::mark_stream_dead`].
    stream_death: Option<String>,
    /// `i32::MAX` has been allocated; see [`OrderVenueIdAllocator::next_order_id`].
    exhausted: bool,
}

impl Default for OrderVenueIdAllocator {
    fn default() -> Self {
        Self {
            next_id: 0,
            have_id: false,
            accounts_ready: false,
            ids: OrderVenueIdRegistry::keeping_stale_reverse(),
            stream_death: None,
            exhausted: false,
        }
    }
}

impl OrderVenueIdAllocator {
    /// IB `nextValidId` callback: take the max with the stored value and the 101 floor.
    pub(crate) fn on_next_valid_id(&mut self, id: i32) {
        self.next_id = self.next_id.max(id).max(101);
        self.have_id = true;
    }

    /// Allocate the next order id (post-increment). Floors at 101 even if `nextValidId` never fired.
    ///
    /// `None` once the id space is spent: `i32::MAX` is handed out ONCE, and a second allocation would
    /// hand the same number to a second order, which in IB's model is an AMEND of the first. The caller
    /// refuses the submit instead.
    pub(crate) fn next_order_id(&mut self) -> Option<i32> {
        if self.exhausted {
            return None;
        }
        self.next_id = self.next_id.max(101);
        let id = self.next_id;
        match id.checked_add(1) {
            Some(next) => self.next_id = next,
            None => self.exhausted = true,
        }
        Some(id)
    }

    pub(crate) fn bind(&mut self, order_id: i32, order_client_id: &str) {
        self.ids.bind(order_client_id, order_id);
    }

    /// Bind a row of the VENUE's open-order snapshot (an order this client placed under a previous
    /// connection) without disturbing what this connection already knows: a row that contradicts a
    /// binding it holds (the id is another order client id's, or the order client id another id's) is refused and changes
    /// nothing. A bound id moves the counter past itself, so a new order never takes the id of a resting
    /// one; the caller has already established that the row is OURS (`OwnOrders`), so a foreign id can
    /// not reach the counter through here.
    pub(crate) fn bind_rebound(&mut self, order_id: i32, order_client_id: &str) -> Rebound {
        let id_taken = self.ids.order_client_id_of(&order_id).is_some_and(|c| c != order_client_id);
        let order_client_id_taken =
            self.ids.venue_order_id_of(order_client_id).is_some_and(|id| *id != order_id);
        if id_taken || order_client_id_taken {
            return Rebound::Conflict;
        }
        let known = self.ids.order_client_id_of(&order_id).is_some();
        self.bind(order_id, order_client_id);
        if order_id < i32::MAX {
            self.next_id = self.next_id.max(order_id + 1);
        }
        if known { Rebound::Known } else { Rebound::Bound }
    }

    /// Dual resolution: numeric orderId first, then the orderRef(=order client id) fallback.
    pub(crate) fn resolve(&self, order_id: i32, order_ref: &str) -> Option<String> {
        // The `orderRef` is the order client id itself, so when the message carries one it decides: a numeric id is
        // only an ID IN THIS CLIENT'S NAMESPACE, and a message about another API client's order whose id
        // happens to equal one of ours must not resolve to our order client id (review 18, R3).
        if !order_ref.is_empty() {
            return self.ids.venue_order_id_of(order_ref).is_some().then(|| order_ref.to_string());
        }
        self.ids.order_client_id_of(&order_id).map(str::to_owned)
    }

    pub(crate) fn order_id_of(&self, order_client_id: &str) -> Option<i32> {
        self.ids.venue_order_id_of(order_client_id).copied()
    }

    /// The reverse of [`OrderVenueIdAllocator::order_id_of`], borrowing rather than cloning — the
    /// stream-death arm walks the whole unacked set to name it.
    pub(crate) fn order_client_id_of(&self, order_id: i32) -> Option<&str> {
        self.ids.order_client_id_of(&order_id)
    }

    pub(crate) fn forget(&mut self, order_id: i32) {
        self.ids.forget_venue_order_id(&order_id);
    }

    pub(crate) fn set_accounts_ready(&mut self, ready: bool) {
        self.accounts_ready = ready;
    }

    /// Latch the transport's inbound stream as permanently dead, recording the transport's own
    /// diagnostic. **One-way by construction**: there is no un-set, because the only backend that
    /// calls it (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `pump_loop`) has no reconnect
    /// to clear it with. A backend that DOES recover sends `IbkrInbound::StreamResync`, which never
    /// reaches here. The FIRST reason wins — a later death notice cannot overwrite the fault that
    /// actually killed the session.
    pub(crate) fn mark_stream_dead(&mut self, reason: &str) {
        self.stream_death.get_or_insert_with(|| reason.to_string());
    }

    /// Whether the inbound stream has terminated for good.
    pub(crate) fn stream_dead(&self) -> bool {
        self.stream_death.is_some()
    }

    /// The transport's diagnostic for the death, so a refusal can name the original fault instead
    /// of a generic "not connected".
    pub(crate) fn stream_death_reason(&self) -> Option<&str> {
        self.stream_death.as_deref()
    }

    /// Ready to accept an order: a valid order id AND the account list are present, AND the inbound
    /// stream is still alive.
    ///
    /// The third conjunct is the one that was missing. Without it this predicate answers "connected"
    /// for the entire life of a transport whose stream died seconds after the handshake — `have_id`
    /// and `accounts_ready` are both latched true by the connect-time seeds and nothing ever clears
    /// them. It is consumed by `crates/bridges/vike-ibkr/src/execution/mod.rs`'s `submit_refusal`; before
    /// that caller existed this was `#[allow(dead_code)]`.
    pub(crate) fn is_connected(&self) -> bool {
        self.have_id && self.accounts_ready && !self.stream_dead()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_has_101_floor_and_takes_max() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(50); // below floor
        assert_eq!(r.next_order_id(), Some(101)); // floored
        assert_eq!(r.next_order_id(), Some(102));
        r.on_next_valid_id(500); // server jumps ahead
        assert_eq!(r.next_order_id(), Some(500));
    }

    #[test]
    fn resolution_goes_by_the_order_ref_when_the_message_has_one_else_by_the_order_id() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id().unwrap();
        r.bind(id, "order-client-id-A");
        // by numeric id
        assert_eq!(r.resolve(id, ""), Some("order-client-id-A".into()));
        // by orderRef fallback when id unknown (e.g. status arrived before bind rebuilt on reconnect)
        assert_eq!(r.resolve(9999, "order-client-id-A"), Some("order-client-id-A".into()));
        // unknown both → None
        assert_eq!(r.resolve(9999, "order-client-id-Z"), None);
        // R3: a message about ANOTHER API client's order whose id equals one of ours carries ITS ref (or
        // none, and then it is told apart by the client id upstream): a ref that is not ours never
        // resolves to our order client id through the numeric id.
        assert_eq!(r.resolve(id, "theirs"), None);
        assert_eq!(
            r.resolve(id, "order-client-id-A"),
            Some("order-client-id-A".into()),
            "the ref and the id agree"
        );
    }

    #[test]
    fn readiness_needs_id_and_accounts() {
        let mut r = OrderVenueIdAllocator::default();
        assert!(!r.is_connected());
        r.on_next_valid_id(101);
        assert!(!r.is_connected()); // still no accounts
        r.set_accounts_ready(true);
        assert!(r.is_connected());
    }

    /// The conjunct that was missing. `have_id`/`accounts_ready` are latched true by the
    /// connect-time seeds and nothing ever clears them, so without the stream-death term this
    /// predicate answers "connected" for the whole life of a transport whose stream died.
    #[test]
    fn a_dead_stream_makes_a_handshaken_registry_not_connected() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(101);
        r.set_accounts_ready(true);
        assert!(r.is_connected());
        assert!(!r.stream_dead());
        assert_eq!(r.stream_death_reason(), None);

        r.mark_stream_dead("connection reset by peer");
        assert!(!r.is_connected(), "a dead stream must not read as connected");
        assert!(r.stream_dead());
        assert_eq!(r.stream_death_reason(), Some("connection reset by peer"));
    }

    /// The latch is one-way and FIRST-WINS: a later notice cannot overwrite the fault that actually
    /// killed the session, which is the one an operator needs named.
    #[test]
    fn the_stream_death_latch_keeps_the_first_reason() {
        let mut r = OrderVenueIdAllocator::default();
        r.mark_stream_dead("connection reset by peer");
        r.mark_stream_dead("stream closed");
        assert_eq!(r.stream_death_reason(), Some("connection reset by peer"));
    }

    #[test]
    fn order_client_id_of_is_the_reverse_of_order_id_of() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id().unwrap();
        r.bind(id, "order-client-id-A");
        assert_eq!(r.order_client_id_of(id), Some("order-client-id-A"));
        assert_eq!(r.order_client_id_of(9999), None);
    }

    /// Review of W14b, finding 9: the highest possible id used to be handed out TWICE (the counter
    /// saturated), so a second order took the first one's live id, which in IB's model is an AMEND of
    /// it. It is handed out once, and then there are no more.
    #[test]
    fn the_highest_possible_id_is_handed_out_once_and_never_reused() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(i32::MAX - 1);
        assert_eq!(r.next_order_id(), Some(i32::MAX - 1));
        assert_eq!(r.next_order_id(), Some(i32::MAX));
        assert_eq!(r.next_order_id(), None, "the id space is spent: no second order may take MAX");
        assert_eq!(r.next_order_id(), None);
    }

    /// An order of ours that rests from before moves the counter past its id (a new order must never take
    /// the id of a live one), and a row that contradicts what this connection holds changes nothing.
    #[test]
    fn a_rebound_row_moves_the_counter_and_a_conflicting_one_changes_nothing() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(101);
        assert_eq!(r.bind_rebound(105, "order-client-id-rested"), Rebound::Bound);
        assert_eq!(r.next_order_id(), Some(106), "past the resting order's id");
        assert_eq!(r.bind_rebound(105, "order-client-id-rested"), Rebound::Known);
        // The id is another order client id's, or the order client id another id's: refused, nothing moves.
        assert_eq!(r.bind_rebound(105, "order-client-id-other"), Rebound::Conflict);
        assert_eq!(r.bind_rebound(999, "order-client-id-rested"), Rebound::Conflict);
        assert_eq!(r.order_client_id_of(105), Some("order-client-id-rested"));
        assert_eq!(r.order_id_of("order-client-id-rested"), Some(105));
        assert_eq!(r.order_client_id_of(999), None);
        assert_eq!(r.next_order_id(), Some(107), "a refused row did not move the counter");
    }

    #[test]
    fn forget_removes_both_directions() {
        let mut r = OrderVenueIdAllocator::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id().unwrap();
        r.bind(id, "order-client-id-A");
        r.forget(id);
        assert_eq!(r.resolve(id, ""), None);
        assert_eq!(r.resolve(0, "order-client-id-A"), None);
    }
}
