//! The ACTIVE confirm, ONE form for every venue that can look an order up by client order id
//! (the design record is the PROPOSED decision 0171): the guard rails around the closure the core's stuck-order watchdog prods the
//! exec thread with (`ExecActor::with_confirm`). What a venue adds is only the lookup
//! ([`ConfirmByCoid::confirm_when`]) and the mapping of a found order's state to events.
//!
//! * **Found** is read by the order's STATE, never by existence: live or partly filled is an
//!   `OrderAccepted`; closed is that `OrderAccepted` followed by its terminal; refused by the venue is
//!   the terminal `OrderRejected` alone. The venue's `found` closure does that mapping
//!   ([`resolve_confirm`]); a lookup that returns only the venue order id
//!   ([`resolve_confirm_by_id`]) can say no more than "accepted with this id".
//! * **Absent** is a terminal `OrderRejected` ONLY when the coid was PROVABLY DISPATCHED
//!   ([`DispatchLedger`], stamped by [`Dispatched`] around the command loop's REST client AFTER every
//!   submit returned, read BEFORE the query) AND the dispatch is INSIDE the venue's lookup window
//!   ([`ConfirmByCoid::LOOKUP_WINDOW`]). A submit still queued in the actor behind a slow cancel batch
//!   is not on the venue yet; past the window the venue has forgotten closed orders, so "absent" would
//!   reject an order that filled and aged out. The window counts from the DISPATCH, not from the close,
//!   which is conservative: an order closed at any time after its dispatch has been closed for no
//!   longer than the dispatch's age. A cleared or full ledger only turns "absent" silent.
//! * **A failed query says nothing**, whatever the failure (auth, signature, ban, 5xx, clock,
//!   timeout): the watchdog ladder, the user-data stream and reconcile decide. An id-less optimistic
//!   `OrderAccepted` is the SUBMIT-TIMEOUT path's answer (`crate::rest::resolve_ambiguous_submit`, where a
//!   POST was sent and may be live) and is never the active confirm's.
//! * **A confirm storm is bounded, and a slow confirm is not dropped** ([`confirm_closure`]): one REST
//!   client (one TLS pool) per closure, at most [`MAX_CONFIRMS_IN_FLIGHT`] confirms RUNNING side by side
//!   (never serialised behind one lock) and the overflow QUEUED for a slot. The core's stuck-order
//!   ladder issues its confirm ONCE per coid (it asks again only for an order holding deferred venue
//!   events) and rejects at its backstop whatever is still unanswered, so nothing here may drop a
//!   confirm for lack of a slot. The one skip is a coid asked again inside [`CONFIRM_BACKOFF`]: its
//!   earlier confirm is queued, running or just answered. It never runs on the core fold, and an empty
//!   answer is always legal.
//!
//! A venue that cannot look an order up by coid is not made to: it keeps the clean no-op (the
//! `ExecutionClient::confirm` default) and the watchdog's last resort.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_model::events::Event;
use vike_model::{OrderRequest, now_ms};

use crate::exec_actor::ConfirmFn;
use crate::rest::VenueRest;
use crate::transport::VenueApiError;

/// Most confirms that may RUN against the one REST client of a closure at once; the rest queue.
pub const MAX_CONFIRMS_IN_FLIGHT: usize = 4;
/// A coid already confirmed this recently is skipped (its earlier confirm is queued, running or just
/// answered; the ladder does not re-ask a coid this fast).
pub const CONFIRM_BACKOFF: Duration = Duration::from_secs(2);
/// Past this many remembered coids the ledger / backoff map is cleared (shortcut: a cleared ledger
/// only makes "absent" silent again for old coids, the safe direction).
const MAP_CAP: usize = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The ACTIVE confirm's verdict over a venue's by-coid lookup: found -> `found(order)` (the venue maps
/// the order's STATE to events); absent -> the "venue confirms order absent" reject ONLY when
/// `absent_rejects` ([`DispatchLedger::provably_dispatched`], read BEFORE the query); a failed query,
/// or an absent answer that proves nothing, -> NOTHING.
pub fn resolve_confirm<T>(
    coid: &str,
    ts: i64,
    query: Result<Option<T>, VenueApiError>,
    absent_rejects: bool,
    found: impl FnOnce(T) -> Vec<Event>,
) -> Vec<Event> {
    match query {
        Ok(Some(order)) => found(order),
        Ok(None) if absent_rejects => {
            vec![crate::rest::resolve_ambiguous_submit(coid, ts, Ok(None))]
        }
        Ok(None) => {
            tracing::info!(
                target: "vike_bridge_core::confirm",
                client_order_id = %coid,
                "confirm: venue has no such order but its submit is not provably dispatched inside the lookup window; saying nothing"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_bridge_core::confirm",
                client_order_id = %coid,
                code = e.code,
                msg = %e.msg,
                "confirm: status query failed; saying nothing (watchdog and reconcile decide)"
            );
            Vec::new()
        }
    }
}

/// [`resolve_confirm`] for a lookup that returns only the venue order id (bybit, okx): found is an
/// `OrderAccepted` carrying it. shortcut: it states no closed state, so an order the venue already
/// cancelled still reads accepted (its terminal rides user-data and reconcile); upgrade when the lookup
/// is widened to read the venue's state word.
pub fn resolve_confirm_by_id(
    coid: &str,
    ts: i64,
    query: Result<Option<String>, VenueApiError>,
    absent_rejects: bool,
) -> Vec<Event> {
    resolve_confirm(coid, ts, query, absent_rejects, |id| {
        vec![crate::rest::resolve_ambiguous_submit(coid, ts, Ok(Some(id)))]
    })
}

/// A REST client that can answer the active confirm. Its `VenueRest::confirm_order` is
/// `confirm_when(.., true)`: that entry point has no ledger, so it assumes the submit was dispatched.
pub trait ConfirmByCoid: VenueRest {
    /// How long after a coid's dispatch the venue's by-coid lookup can still see the order, or `None`
    /// when the venue states no limit. Past it, "absent" is silence (the venue forgets closed orders).
    /// Required, not defaulted, so every venue states it.
    const LOOKUP_WINDOW: Option<Duration>;

    /// Query the order by coid and apply [`resolve_confirm`]. `absent_rejects` = the exec thread has
    /// finished this coid's submit and the dispatch is inside [`Self::LOOKUP_WINDOW`]
    /// ([`DispatchLedger::provably_dispatched`]); [`confirm_closure`] reads it BEFORE calling, so an
    /// "absent" answer cannot predate the submit.
    fn confirm_when(&self, coid: &str, ts: i64, absent_rejects: bool) -> Vec<Event>;
}

/// The coids whose submit the exec thread has finished dispatching, each with the instant its
/// dispatch returned (marked AFTER the REST call returned, so an in-flight POST is not "finished").
/// Bounded by [`MAP_CAP`].
#[derive(Default)]
pub struct DispatchLedger(Mutex<HashMap<String, Instant>>);

impl DispatchLedger {
    pub fn mark(&self, coid: &str) {
        self.mark_at(coid, Instant::now());
    }

    fn mark_at(&self, coid: &str, at: Instant) {
        let mut map = lock(&self.0);
        if map.len() >= MAP_CAP {
            map.clear();
        }
        map.insert(coid.to_string(), at);
    }

    /// Whether `coid`'s dispatch returned AND is no older than `window` (`None` = no limit).
    pub fn provably_dispatched(&self, coid: &str, window: Option<Duration>) -> bool {
        lock(&self.0).get(coid).is_some_and(|at| window.is_none_or(|w| at.elapsed() <= w))
    }
}

/// The command loop's REST client with the [`DispatchLedger`] stamped after every submit. Delegates
/// EVERY [`VenueRest`] method explicitly (a defaulted one would silently drop the inner client's
/// override: the native batch endpoints).
pub struct Dispatched<R> {
    pub rest: R,
    pub ledger: Arc<DispatchLedger>,
}

impl<R: VenueRest> VenueRest for Dispatched<R> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let events = self.rest.submit_order(request);
        self.ledger.mark(&request.client_order_id);
        events
    }
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        self.rest.cancel_order(client_order_id)
    }
    fn confirm_order(&self, client_order_id: &str, ts: i64) -> Vec<Event> {
        self.rest.confirm_order(client_order_id, ts)
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.rest.modify_order(order, new_qty, new_price)
    }
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let events = self.rest.submit_batch(requests);
        for r in requests {
            self.ledger.mark(&r.client_order_id);
        }
        events
    }
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        self.rest.cancel_batch(client_order_ids)
    }
}

/// Admission to the confirm client: a per-coid backoff, then a counting semaphore of
/// [`MAX_CONFIRMS_IN_FLIGHT`] slots that QUEUES the overflow. A confirm is never dropped for lack of a
/// slot: the stuck-order ladder asks once per coid and rejects at its backstop whatever is still
/// unanswered, so a dropped confirm is a live order rejected. (Each confirm already has its own worker
/// thread, so waiting here parks no shared resource.)
#[derive(Default)]
struct ConfirmGate {
    busy: Mutex<usize>,
    freed: Condvar,
    last: Mutex<HashMap<String, Instant>>,
}

struct Permit<'a>(&'a ConfirmGate);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) -= 1;
        self.0.freed.notify_one();
    }
}

impl ConfirmGate {
    /// `None` only for a coid asked again inside [`CONFIRM_BACKOFF`]: its earlier confirm is queued,
    /// running or just answered. Otherwise blocks until a slot is free.
    fn enter(&self, coid: &str) -> Option<Permit<'_>> {
        {
            let now = Instant::now();
            let mut last = lock(&self.last);
            if last.get(coid).is_some_and(|t| now.duration_since(*t) < CONFIRM_BACKOFF) {
                return None;
            }
            if last.len() >= MAP_CAP {
                last.retain(|_, t| now.duration_since(*t) < CONFIRM_BACKOFF);
            }
            last.insert(coid.to_string(), now);
        }
        let mut busy = lock(&self.busy);
        while *busy >= MAX_CONFIRMS_IN_FLIGHT {
            busy = self.freed.wait(busy).unwrap_or_else(PoisonError::into_inner);
        }
        *busy += 1;
        Some(Permit(self))
    }
}

/// The `ExecActor::with_confirm` closure for one exec client: ONE REST client for every confirm (one
/// connection pool; confirms run side by side up to [`MAX_CONFIRMS_IN_FLIGHT`], never serialised behind
/// a lock, so a slow lookup does not hold the next stuck order past the ladder's backstop), `prepare`
/// run on it until it returns `true` (spot's server-time skew read, lazily on the first confirm so
/// building the closure costs no network; a venue with none passes `|_| true`), and [`ConfirmGate`]
/// bounding the storm.
pub fn confirm_closure<R, P>(rest: R, ledger: Arc<DispatchLedger>, prepare: P) -> ConfirmFn
where
    R: ConfirmByCoid + Sync + 'static,
    P: Fn(&R) -> bool + Send + Sync + 'static,
{
    let prepared = Mutex::new(false);
    let gate = ConfirmGate::default();
    Arc::new(move |coid: &str| {
        let Some(_permit) = gate.enter(coid) else {
            return Vec::new();
        };
        {
            // Held only while preparing, so concurrent first confirms prepare once, in turn.
            let mut done = lock(&prepared);
            if !*done {
                *done = prepare(&rest);
            }
        }
        // Read BEFORE the query: "dispatched" must predate an "absent" answer.
        let absent_rejects = ledger.provably_dispatched(coid, R::LOOKUP_WINDOW);
        rest.confirm_when(coid, now_ms(), absent_rejects)
    })
}

#[cfg(test)]
#[path = "confirm_tests.rs"]
mod confirm_tests;
