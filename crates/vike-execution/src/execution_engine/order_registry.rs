//! [`OrderRegistry`]: every order an engine holds, keyed by order client id in registration order, and an
//! index of the LIVE ones ([`vike_model::OrderStatus::is_live`]).
//!
//! The registry never evicts an ENDED order (decision 0207: it refuses a reused order client id the engine
//! holds answered or finished), so it only grows with uptime, while the walks that run per publish and per
//! submit (the margin and exposure of the orders in flight, the reduce-only cover, the cash book's orders
//! out) count only the live ones. [`OrderRegistry::live`] walks those at the cost of the live ones alone.
//!
//! **The index cannot be bypassed**: the map is private, reads go through `Deref` to the `IndexMap` (no
//! `DerefMut`), and every write is a method here that keeps the index in step: [`OrderRegistry::insert`]
//! (a re-insert over an ended order's key makes it live again), [`OrderRegistry::get_mut`],
//! [`OrderRegistry::shift_remove`], [`OrderRegistry::clear`] and `collect` (the snapshot rebuild).
//!
//! The index is the ascending registry positions of every live order, plus the one order
//! [`OrderRegistry::get_mut`] lent out last whatever its caller wrote to it: a SUPERSET of the live set,
//! settled (an order that ended is dropped) at the next write, when the borrow is over. [`OrderRegistry::live`]
//! re-tests each status, so it yields exactly what `iter().filter(|(_, o)| o.status.is_live())` does, in
//! the same order, and every sum over it is bit-identical to the full walk's
//! (`crates/vike-execution/tests/integration/order_registry_live_index_property_tests.rs`).
//!
//! **Every write stamps its order** ([`OrderRegistry::write_stamp`]): a reader that keeps something built from an
//! order between two reads (the core's published order list, `crates/vike-core/src/snapshot/order_window.rs`) asks
//! whether the order was written since instead of comparing it. A stamp names the registry too (each construction and
//! clone draws its own), so a registry put in another's place never matches a stamp of the one it replaced. A
//! `get_mut` stamps at the lend, whether or not its caller writes: a stamp that moved means "maybe changed".

use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::{Equivalent, IndexMap};
use vike_model::OrderRecord;

/// The engine's orders and the index of its live ones (the module doc). Reads are the `IndexMap`'s.
pub struct OrderRegistry {
    orders: IndexMap<String, OrderRecord>,
    /// Ascending positions in `orders`: every live order's, and possibly `lent`'s.
    live: Vec<usize>,
    /// The position [`Self::get_mut`] lent out last, still in `live` until the next write settles it.
    lent: Option<usize>,
    /// Per position in `orders`, the value of `writes` at that order's last write ([`Self::write_stamp`]).
    written: Vec<u64>,
    /// How many writes this registry has taken.
    writes: u64,
    /// Which registry this is: drawn from `next_identity` at each construction and clone.
    identity: u64,
}

impl OrderRegistry {
    /// The LIVE orders in registry order: `iter().filter(|(_, o)| o.status.is_live())`, at the cost of the
    /// live ones only.
    pub fn live(&self) -> impl Iterator<Item = (&String, &OrderRecord)> {
        self.live_positions().filter_map(|at| self.orders.get_index(at))
    }

    /// When the order at registry position `at` was last written, as `(registry, write)`: equal at two reads only if
    /// the same registry holds it and nothing wrote it in between (the module doc). `None` past the end.
    pub fn write_stamp(&self, at: usize) -> Option<(u64, u64)> {
        self.written.get(at).map(|&write| (self.identity, write))
    }

    /// The registry positions of [`Self::live`]'s orders, ascending (`IndexMap::get_index` names each).
    pub fn live_positions(&self) -> impl Iterator<Item = usize> {
        self.live.iter().copied().filter(|&at| {
            self.orders.get_index(at).is_some_and(|(_, order)| order.status.is_live())
        })
    }

    /// `IndexMap::insert`: a new key goes last, an existing one keeps its place and takes `order`, live or not.
    pub fn insert(&mut self, order_client_id: String, order: OrderRecord) -> Option<OrderRecord> {
        self.settle();
        let is_live = order.status.is_live();
        let (at, old) = self.orders.insert_full(order_client_id, order);
        mark(&mut self.live, at, is_live);
        self.stamp(at);
        old
    }

    /// `IndexMap::get_mut`. The order stays indexed whatever the caller writes, until the next write.
    pub fn get_mut<Q: ?Sized + Hash + Equivalent<String>>(
        &mut self,
        order_client_id: &Q,
    ) -> Option<&mut OrderRecord> {
        self.settle();
        let (at, _, order) = self.orders.get_full_mut(order_client_id)?;
        mark(&mut self.live, at, true);
        self.lent = Some(at);
        self.writes += 1;
        self.written[at] = self.writes;
        Some(order)
    }

    /// `IndexMap::shift_remove`. Every later order moves up a place, so the index is rebuilt (no core
    /// removes an order; a test does).
    pub fn shift_remove<Q: ?Sized + Hash + Equivalent<String>>(
        &mut self,
        order_client_id: &Q,
    ) -> Option<OrderRecord> {
        let (at, _, removed) = self.orders.shift_remove_full(order_client_id)?;
        self.written.remove(at);
        self.reindex();
        Some(removed)
    }

    /// `IndexMap::clear`.
    pub fn clear(&mut self) {
        self.orders.clear();
        self.written.clear();
        self.reindex();
    }

    /// Stamp a write of the order at `at`: a new key's goes last, as the key does.
    fn stamp(&mut self, at: usize) {
        self.writes += 1;
        match self.written.get_mut(at) {
            Some(written) => *written = self.writes,
            None => self.written.push(self.writes),
        }
    }

    /// Drop the order [`Self::get_mut`] lent out from the index if it is no longer live: its borrow is over.
    fn settle(&mut self) {
        if let Some(at) = self.lent.take() {
            let is_live = self.orders.get_index(at).is_some_and(|(_, o)| o.status.is_live());
            mark(&mut self.live, at, is_live);
        }
    }

    fn reindex(&mut self) {
        self.lent = None;
        self.live.clear();
        let live = self.orders.values().enumerate().filter(|(_, o)| o.status.is_live());
        self.live.extend(live.map(|(at, _)| at));
    }
}

/// Put `at` in or out of the ascending `live` positions.
fn mark(live: &mut Vec<usize>, at: usize, is_live: bool) {
    match (live.binary_search(&at), is_live) {
        (Err(slot), true) => live.insert(slot, at),
        (Ok(slot), false) => {
            live.remove(slot);
        }
        _ => {}
    }
}

/// A registry identity no other construction or clone in this process has drawn.
fn next_identity() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Default for OrderRegistry {
    fn default() -> Self {
        OrderRegistry {
            orders: IndexMap::new(),
            live: Vec::new(),
            lent: None,
            written: Vec::new(),
            writes: 0,
            identity: next_identity(),
        }
    }
}

/// A copy with its own identity: the two take writes apart from here, so no stamp of one may match the other's.
impl Clone for OrderRegistry {
    fn clone(&self) -> Self {
        OrderRegistry {
            orders: self.orders.clone(),
            live: self.live.clone(),
            lent: self.lent,
            written: self.written.clone(),
            writes: self.writes,
            identity: next_identity(),
        }
    }
}

impl std::ops::Deref for OrderRegistry {
    type Target = IndexMap<String, OrderRecord>;

    fn deref(&self) -> &Self::Target {
        &self.orders
    }
}

impl FromIterator<(String, OrderRecord)> for OrderRegistry {
    fn from_iter<I: IntoIterator<Item = (String, OrderRecord)>>(orders: I) -> Self {
        let mut registry =
            OrderRegistry { orders: orders.into_iter().collect(), ..OrderRegistry::default() };
        registry.writes = registry.orders.len() as u64;
        registry.written = (1..=registry.writes).collect();
        registry.reindex();
        registry
    }
}

/// The orders alone: the index is derived from them.
impl PartialEq for OrderRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.orders == other.orders
    }
}

/// The orders alone, printed as the `IndexMap` always was.
impl std::fmt::Debug for OrderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.orders.fmt(f)
    }
}
