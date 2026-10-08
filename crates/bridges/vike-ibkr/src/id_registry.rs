//! The IBKR id model — the one genuinely new cost vs vike's coid-native venues. IB assigns a
//! NUMERIC `orderId` (from `nextValidId`) and reports status keyed by it; vike is `client_order_id`
//! (coid) native. So the bridge owns a coid⇄orderId map, carries the coid in IB's `orderRef`, and
//! resolves inbound events by orderId with an orderRef fallback (Nautilus dual-resolution). The
//! `next_order_id` floor `max(stored, incoming, 101)` and the "connected only when a valid id AND
//! the account list are present" readiness gate are also Nautilus patterns.
//!
//! `IdRegistry` is consumed by the event mapper (Task 7) and the exec loop (Task 8, `exec::run_exec`
//! resolves/binds through the mapper's registry).

use std::collections::HashMap;

#[derive(Default)]
pub struct IdRegistry {
    next_id: i32,
    have_id: bool,
    accounts_ready: bool,
    coid_by_order_id: HashMap<i32, String>,
    order_id_by_coid: HashMap<String, i32>,
    /// The transport's own diagnostic, set once the inbound stream has terminated for good
    /// (`IbInbound::StreamDead`). `Some` is the latch — see [`IdRegistry::mark_stream_dead`].
    stream_death: Option<String>,
}

impl IdRegistry {
    /// IB `nextValidId` callback: take the max with the stored value and the 101 floor.
    pub fn on_next_valid_id(&mut self, id: i32) {
        self.next_id = self.next_id.max(id).max(101);
        self.have_id = true;
    }

    /// Allocate the next order id (post-increment). Floors at 101 even if `nextValidId` never fired.
    pub fn next_order_id(&mut self) -> i32 {
        self.next_id = self.next_id.max(101);
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn bind(&mut self, order_id: i32, coid: &str) {
        self.coid_by_order_id.insert(order_id, coid.to_string());
        self.order_id_by_coid.insert(coid.to_string(), order_id);
    }

    /// Dual resolution: numeric orderId first, then the orderRef(=coid) fallback.
    pub fn resolve(&self, order_id: i32, order_ref: &str) -> Option<String> {
        if let Some(coid) = self.coid_by_order_id.get(&order_id) {
            return Some(coid.clone());
        }
        if !order_ref.is_empty() && self.order_id_by_coid.contains_key(order_ref) {
            return Some(order_ref.to_string());
        }
        None
    }

    pub fn order_id_of(&self, coid: &str) -> Option<i32> {
        self.order_id_by_coid.get(coid).copied()
    }

    /// The reverse of [`IdRegistry::order_id_of`], borrowing rather than cloning — the
    /// stream-death arm walks the whole unacked set to name it.
    pub fn coid_of(&self, order_id: i32) -> Option<&str> {
        self.coid_by_order_id.get(&order_id).map(String::as_str)
    }

    pub fn forget(&mut self, order_id: i32) {
        if let Some(coid) = self.coid_by_order_id.remove(&order_id) {
            self.order_id_by_coid.remove(&coid);
        }
    }

    pub fn set_accounts_ready(&mut self, ready: bool) {
        self.accounts_ready = ready;
    }

    /// Latch the transport's inbound stream as permanently dead, recording the transport's own
    /// diagnostic. **One-way by construction**: there is no un-set, because the only backend that
    /// calls it (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s `pump_loop`) has no reconnect
    /// to clear it with. A backend that DOES recover sends `IbInbound::StreamResync`, which never
    /// reaches here. The FIRST reason wins — a later death notice cannot overwrite the fault that
    /// actually killed the session.
    pub fn mark_stream_dead(&mut self, reason: &str) {
        self.stream_death.get_or_insert_with(|| reason.to_string());
    }

    /// Whether the inbound stream has terminated for good.
    pub fn stream_dead(&self) -> bool {
        self.stream_death.is_some()
    }

    /// The transport's diagnostic for the death, so a refusal can name the original fault instead
    /// of a generic "not connected".
    pub fn stream_death_reason(&self) -> Option<&str> {
        self.stream_death.as_deref()
    }

    /// Ready to accept an order: a valid order id AND the account list are present, AND the inbound
    /// stream is still alive.
    ///
    /// The third conjunct is the one that was missing. Without it this predicate answers "connected"
    /// for the entire life of a transport whose stream died seconds after the handshake — `have_id`
    /// and `accounts_ready` are both latched true by the connect-time seeds and nothing ever clears
    /// them. It is consumed by `crates/bridges/vike-ibkr/src/exec.rs`'s `submit_refusal`; before
    /// that caller existed this was `#[allow(dead_code)]`.
    pub fn is_connected(&self) -> bool {
        self.have_id && self.accounts_ready && !self.stream_dead()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_has_101_floor_and_takes_max() {
        let mut r = IdRegistry::default();
        r.on_next_valid_id(50); // below floor
        assert_eq!(r.next_order_id(), 101); // floored
        assert_eq!(r.next_order_id(), 102);
        r.on_next_valid_id(500); // server jumps ahead
        assert_eq!(r.next_order_id(), 500);
    }

    #[test]
    fn dual_resolution_prefers_order_id_then_order_ref() {
        let mut r = IdRegistry::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id();
        r.bind(id, "coid-A");
        // by numeric id
        assert_eq!(r.resolve(id, ""), Some("coid-A".into()));
        // by orderRef fallback when id unknown (e.g. status arrived before bind rebuilt on reconnect)
        assert_eq!(r.resolve(9999, "coid-A"), Some("coid-A".into()));
        // unknown both → None
        assert_eq!(r.resolve(9999, "coid-Z"), None);
    }

    #[test]
    fn readiness_needs_id_and_accounts() {
        let mut r = IdRegistry::default();
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
        let mut r = IdRegistry::default();
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
        let mut r = IdRegistry::default();
        r.mark_stream_dead("connection reset by peer");
        r.mark_stream_dead("stream closed");
        assert_eq!(r.stream_death_reason(), Some("connection reset by peer"));
    }

    #[test]
    fn coid_of_is_the_reverse_of_order_id_of() {
        let mut r = IdRegistry::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id();
        r.bind(id, "coid-A");
        assert_eq!(r.coid_of(id), Some("coid-A"));
        assert_eq!(r.coid_of(9999), None);
    }

    #[test]
    fn forget_removes_both_directions() {
        let mut r = IdRegistry::default();
        r.on_next_valid_id(101);
        let id = r.next_order_id();
        r.bind(id, "coid-A");
        r.forget(id);
        assert_eq!(r.resolve(id, ""), None);
        assert_eq!(r.resolve(0, "coid-A"), None);
    }
}
