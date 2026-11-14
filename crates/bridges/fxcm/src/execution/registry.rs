//! **Which of the venue's orders are OURS, by venue order id.** ForexConnect echoes no order client id on an Orders row,
//! so the reconcile listing named every resting order `order_client_id=None`: while a far limit rested, every pass held an
//! `UnknownOrder` (the venue's row) beside an `OrphanLocalOrder` (ours, which the listing never named), MEASURED on the
//! demo 2026-11-09. The exec thread learns the venue order id from the placement's answer and routes the order's later
//! events by it ([`crate::execution`]'s `RouteTable`); this table holds the same binding where the reconcile client can read
//! it, so the listing names our order client id for exactly the ids the exec thread routes.
//!
//! Two writers: the exec thread's `RouteTable`, which binds an id when it routes one (a placement, an exit's leg, a close's
//! leg, a restored order the core taught) and forgets it when the route dies (a cancel the venue confirmed, a rejection, a
//! venue's removal); and the exec client's `adopt_restored`, which binds a restored order's id at once, before the session
//! thread routes it. One reader: the Orders listing ([`crate::reconcile`]'s `parse_orders`).
//!
//! Process-wide on purpose, like [`crate::execution::trade_roots`]: the exec session and the reconcile session are two sessions of
//! one login on two threads and neither can reach the other. Venue order ids are the venue's, unique across its accounts,
//! so one table serves every account of the process.
//!
//! **After a restart of the process that holds the sessions (the gatewayhub)** the table is empty, and the venue keeps no
//! id of ours. The tradehub's re-teaching names each resting order with the venue order id its core holds
//! (`vike_execution::RestoredOrder::venue_order_id`), and `adopt_restored` binds it, so a pre-restart order lists under our order client id
//! again. ⚠ What is still not covered: a restart of BOTH daemons (the boot restore's venue-order-id join,
//! `vike_execution::recon::name_by_venue_order_id`, exists, but fxcm's row of `crates/vike-venues/src/venue_restore.rs` stays
//! `Never` until a demo smoke proves the round trip, so the boot restore does not run for fxcm: nothing teaches this table
//! and the order lists as `order_client_id=None`, held, until it ends), and an exit spanning several trades, whose core order holds
//! only its FIRST leg's id (the other legs stay unbound: [`crate::execution::exit::group`]'s `ExitGroup::restored` says why they cannot
//! be found).

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use vike_bridge_core::execution_client::order_client_id::FifoMap;

/// Bindings kept: the exec thread's own route bound (`MAX_ROUTES` in `execution/mod.rs`), so this table never forgets an id the
/// exec thread still routes. Past it the OLDEST goes.
const MAX_BINDINGS: usize = 100_000;

/// Venue order id -> order client id, bounded FIFO (the shared [`FifoMap`]).
#[derive(Debug, Default)]
pub(crate) struct OrderClientIdLookup {
    order_client_id: FifoMap<String, MAX_BINDINGS>,
}

impl OrderClientIdLookup {
    /// The exec thread routes `venue_order_id` to `order_client_id`. An empty id is ignored.
    pub(crate) fn bind(&mut self, venue_order_id: &str, order_client_id: &str) {
        if venue_order_id.is_empty() {
            return;
        }
        self.order_client_id.insert(venue_order_id.to_string(), order_client_id.to_string());
    }

    /// The route of `venue_order_id` died: the order is no longer ours to name.
    pub(crate) fn forget(&mut self, venue_order_id: &str) {
        self.order_client_id.remove(venue_order_id);
    }

    /// Our order client id for `venue_order_id`, or `None` for an order this process does not route (a foreign one, or one placed
    /// before a restart).
    pub(crate) fn order_client_id_of(&self, venue_order_id: &str) -> Option<String> {
        self.order_client_id.as_map().get(venue_order_id).cloned()
    }
}

/// The process's table. A poisoned lock is read through: the table holds ids only, and a panic elsewhere must not stop
/// the reconcile pass or the exec thread from asking.
pub(crate) fn process() -> MutexGuard<'static, OrderClientIdLookup> {
    static TABLE: OnceLock<Mutex<OrderClientIdLookup>> = OnceLock::new();
    TABLE
        .get_or_init(|| Mutex::new(OrderClientIdLookup::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[path = "registry_tests.rs"]
#[cfg(test)]
mod registry_tests;
