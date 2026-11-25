//! **The tables a bridge keeps about its orders' ids, in ONE shape each** (X5; the owner's "one type given its
//! venue"). Every copy they replace was a bridge's own, and they differed only in facts that are now parameters:
//!
//! * [`OrderVenueIdRegistry`]: our order client id <-> the venue's OWN order id, both directions. For a venue whose
//!   cancel and amend address an order by its own id and whose stream names it by that id: ig's `dealId`, cTrader's
//!   and IBKR's numeric `orderId`, polymarket's CLOB id, deribit's `order_id`. The id's type is the venue's (`Id`).
//!   What a re-bind under a NEW id does with the id the order held before is the venue's fact too:
//!   [`OrderVenueIdRegistry::new`] drops it, [`OrderVenueIdRegistry::keeping_stale_reverse`] leaves it naming the order.
//! * [`FifoMap`]: a table bounded by its insertion order: past its cap (`CAP`, the venue's) the OLDEST row goes.
//!   fxcm's routes, unknown placements, listing bindings, opening commissions and trade roots; ig's deal references.
//!
//! Neither is persisted, like every copy before them: a gatewayhub restart empties them, and each bridge's
//! re-teaching (`adopt_restored`) is what refills one. Neither locks: the bridge chooses the `Mutex` around it,
//! because some keep other state under the same lock (polymarket's park must share its critical section).

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// Our order client id <-> the venue's own order id, both directions (see the module doc).
///
/// Both directions are kept because the readers ask opposite questions and neither can answer with a scan: a cancel
/// holds our id and needs the venue's, a stream frame or a reconcile row holds the venue's and needs ours.
#[derive(Debug, Clone)]
pub struct OrderVenueIdRegistry<Id> {
    venue_order_ids: HashMap<String, Id>,
    order_client_ids: HashMap<Id, String>,
    /// A re-bind under a new venue id leaves the old id naming the order (see [`Self::keeping_stale_reverse`]).
    keeps_stale_reverse: bool,
}

impl<Id> Default for OrderVenueIdRegistry<Id> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id> OrderVenueIdRegistry<Id> {
    /// An empty registry whose re-bind under a NEW venue id drops the old id's reverse row, so the two directions
    /// always hold the same pairs (ig, cTrader, deribit).
    pub fn new() -> Self {
        Self {
            venue_order_ids: HashMap::new(),
            order_client_ids: HashMap::new(),
            keeps_stale_reverse: false,
        }
    }

    /// An empty registry whose re-bind under a NEW venue id leaves the old id still naming the order (IBKR,
    /// polymarket): their copies wrote both directions with a plain insert, and polymarket's states it (a
    /// pre-submit id the venue's answer then replaced keeps re-keying that order's frames).
    pub fn keeping_stale_reverse() -> Self {
        Self { keeps_stale_reverse: true, ..Self::new() }
    }

    /// Every order client id bound now, in no order.
    pub fn order_client_ids(&self) -> impl Iterator<Item = &str> {
        self.venue_order_ids.keys().map(String::as_str)
    }
}

impl<Id: Clone + Eq + Hash> OrderVenueIdRegistry<Id> {
    /// Bind `order_client_id` to `venue_order_id`, in both directions. A re-bind under a new id handles the old one
    /// as this registry was built to ([`Self::new`], [`Self::keeping_stale_reverse`]).
    pub fn bind(&mut self, order_client_id: &str, venue_order_id: Id) {
        let previous =
            self.venue_order_ids.insert(order_client_id.to_string(), venue_order_id.clone());
        if !self.keeps_stale_reverse
            && let Some(stale) = previous.filter(|old| *old != venue_order_id)
        {
            self.order_client_ids.remove(&stale);
        }
        self.order_client_ids.insert(venue_order_id, order_client_id.to_string());
    }

    /// The venue's id of `order_client_id`'s order: what a cancel or an amend addresses it by.
    pub fn venue_order_id_of(&self, order_client_id: &str) -> Option<&Id> {
        self.venue_order_ids.get(order_client_id)
    }

    /// Our id of the order the venue calls `venue_order_id`, or `None` for one this process did not bind.
    pub fn order_client_id_of<Q>(&self, venue_order_id: &Q) -> Option<&str>
    where
        Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.order_client_ids.get(venue_order_id).map(String::as_str)
    }

    /// Forget `order_client_id`'s order in both directions; its venue id, if it had one.
    pub fn forget(&mut self, order_client_id: &str) -> Option<Id> {
        let venue_order_id = self.venue_order_ids.remove(order_client_id)?;
        self.order_client_ids.remove(&venue_order_id);
        Some(venue_order_id)
    }

    /// Forget the order the venue calls `venue_order_id` in both directions; our id of it, if it was bound.
    pub fn forget_venue_order_id<Q>(&mut self, venue_order_id: &Q) -> Option<String>
    where
        Id: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let order_client_id = self.order_client_ids.remove(venue_order_id)?;
        self.venue_order_ids.remove(&order_client_id);
        Some(order_client_id)
    }
}

/// A table bounded by its insertion order: a NEW key joins the queue, and past `CAP` rows the OLDEST go.
///
/// Insertion-ordered, not LRU: the rows it was built for answer "did this process place or see this", and recency of
/// USE says nothing about that. The map and the queue always hold the same keys ([`Self::remove`] says why).
#[derive(Debug, Clone)]
pub struct FifoMap<V, const CAP: usize> {
    map: HashMap<String, V>,
    inserted: VecDeque<String>,
}

impl<V, const CAP: usize> Default for FifoMap<V, CAP> {
    fn default() -> Self {
        Self { map: HashMap::new(), inserted: VecDeque::new() }
    }
}

impl<V, const CAP: usize> FifoMap<V, CAP> {
    /// Insert `key`; a key already present keeps its place in the queue and takes the new value. Past `CAP` rows
    /// the OLDEST goes, and is handed back so a caller can drop what it keeps beside it (ig's references).
    pub fn insert(&mut self, key: String, value: V) -> Option<(String, V)> {
        if self.map.insert(key.clone(), value).is_none() {
            self.inserted.push_back(key);
        }
        // At most one row: the map and the queue hold the same keys, at most `CAP` before this insert added one.
        if self.inserted.len() > CAP {
            let oldest = self.inserted.pop_front()?;
            return self.map.remove(&oldest).map(|value| (oldest, value));
        }
        None
    }

    /// Drop `key` from the map AND the queue, so the two always hold the same keys.
    ///
    /// The queue scan is O(n) and deliberate: a dangling key left behind would let a later eviction pop it, count it
    /// as having freed a row, and evict a LIVE row early. The callers run it once per ended order, on a venue's own
    /// thread, never on the vike-core hot fold.
    pub fn remove(&mut self, key: &str) -> Option<V> {
        let value = self.map.remove(key)?;
        if let Some(pos) = self.inserted.iter().position(|k| k == key) {
            self.inserted.remove(pos);
        }
        Some(value)
    }

    /// The rows, read-only (a write goes through [`Self::insert`] or [`Self::remove`], which keep the queue).
    pub fn as_map(&self) -> &HashMap<String, V> {
        &self.map
    }
}

#[cfg(test)]
#[path = "order_client_id_tests.rs"]
mod order_client_id_tests;
