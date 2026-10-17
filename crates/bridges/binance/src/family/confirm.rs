//! The by-coid order-status confirm, shared by binance and aster: what a `GET .../order` answer
//! becomes, and the guard rails around the ACTIVE confirm the core's watchdog prods the exec thread
//! with (`ExecActor::with_confirm`).
//!
//! * **A found order is read by STATUS** ([`found_events`]): `NEW`/`PARTIALLY_FILLED`/`FILLED` are an
//!   `OrderAccepted` (fills arrive through user-data and the gap sentinel); `CANCELED` and
//!   `EXPIRED`/`EXPIRED_IN_MATCH` are the `OrderAccepted` followed by their terminal; `REJECTED` is
//!   the terminal `OrderRejected` alone. A bare "the id exists" turned a closed order into a live one.
//! * **A failed query says nothing, for both callers.** An auth / signature / ban / 5xx / clock
//!   failure says nothing about the order, so the AMBIGUOUS SUBMIT ([`resolve_submit`], whose POST
//!   may be live) and the ACTIVE CONFIRM ([`resolve_confirm`]) both emit NOTHING and the watchdog
//!   ladder, the user-data stream and reconcile decide. An id-less optimistic `OrderAccepted` used to
//!   answer the submit; it was a phantom when the order never landed, which the watchdog never sweeps.
//! * **"Absent" is only trusted once the exec thread has FINISHED dispatching the coid**
//!   ([`DispatchLedger`], marked by [`Dispatched`] around the command loop's REST client). A submit
//!   still queued in the actor behind a slow cancel batch is not on the venue yet, and `-2013` would
//!   reject an order that is about to be placed. The ledger is read BEFORE the query, never after.
//! * **A confirm storm is bounded** ([`confirm_closure`]): one REST client (one TLS pool) per
//!   closure, at most [`MAX_CONFIRMS_IN_FLIGHT`] confirms waiting or running, and a coid is not
//!   re-queried within [`CONFIRM_BACKOFF`]; a skipped confirm is simply the next watchdog tick's.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::ConfirmFn;
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::VenueApiError;
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected};
use vike_model::{OrderRequest, now_ms};

use super::rest_reads::FoundOrder;

/// Most confirms that may be waiting on, or running against, the one REST client of a closure.
pub const MAX_CONFIRMS_IN_FLIGHT: usize = 4;
/// A coid already confirmed this recently is skipped (the watchdog prods again later).
pub const CONFIRM_BACKOFF: Duration = Duration::from_secs(2);
/// Past this many remembered coids the ledger / backoff map is cleared (shortcut: a cleared ledger
/// only makes "absent" silent again for old coids, the safe direction).
const MAP_CAP: usize = 4096;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The events for an order the venue HAS, read by its `status` (see the module doc).
pub fn found_events(coid: &str, ts: i64, found: &FoundOrder) -> Vec<Event> {
    let accepted = || {
        Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(found.order_id.clone().into()),
            ts,
        })
    };
    match found.status.as_str() {
        "REJECTED" => vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: "venue reports the order REJECTED".to_string().into(),
            ts,
        })],
        "CANCELED" => vec![
            accepted(),
            Event::OrderCanceled(OrderCanceled {
                client_order_id: coid.to_string(),
                reason: "venue reports the order CANCELED".to_string().into(),
                ts,
            }),
        ],
        "EXPIRED" | "EXPIRED_IN_MATCH" => vec![
            accepted(),
            Event::OrderExpired(OrderExpired { client_order_id: coid.to_string(), ts }),
        ],
        // NEW / PARTIALLY_FILLED / FILLED / PENDING_CANCEL / anything newer: the order is (or was)
        // live; its fills and terminal ride user-data and the gap sentinel.
        _ => vec![accepted()],
    }
}

/// An AMBIGUOUS SUBMIT's re-query (the POST timed out and may be live): found -> [`found_events`];
/// absent -> `vike_bridge_core::resolve_ambiguous_submit`'s "venue confirms absent" reject; a FAILED
/// query -> NOTHING, like [`resolve_confirm`]: an id-less accept would be a phantom when the order
/// never landed (the stuck-order watchdog sweeps only an order still pre-ack), so the watchdog's
/// active confirm and the user-data stream decide.
pub fn resolve_submit(
    coid: &str,
    ts: i64,
    query: Result<Option<FoundOrder>, VenueApiError>,
) -> Vec<Event> {
    match query {
        Ok(Some(found)) => found_events(coid, ts, &found),
        Ok(None) => vec![vike_bridge_core::resolve_ambiguous_submit(coid, ts, Ok(None))],
        Err(e) => {
            tracing::warn!(
                target: "vike_binance::confirm",
                client_order_id = %coid,
                code = e.code,
                msg = %e.msg,
                "ambiguous submit: status query failed; saying nothing (watchdog confirm and user-data decide)"
            );
            Vec::new()
        }
    }
}

/// The ACTIVE confirm's verdict: found -> [`found_events`]; absent -> the "venue confirms absent"
/// reject ONLY when `dispatched` (the exec thread finished this coid's submit, read BEFORE the
/// query); a failed query, or an absent answer for a coid still queued, -> NOTHING.
pub fn resolve_confirm(
    coid: &str,
    ts: i64,
    query: Result<Option<FoundOrder>, VenueApiError>,
    dispatched: bool,
) -> Vec<Event> {
    match query {
        Ok(Some(found)) => found_events(coid, ts, &found),
        Ok(None) if dispatched => {
            vec![vike_bridge_core::resolve_ambiguous_submit(coid, ts, Ok(None))]
        }
        Ok(None) => {
            tracing::info!(
                target: "vike_binance::confirm",
                client_order_id = %coid,
                "confirm: venue has no such order but its submit is not dispatched yet; saying nothing"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_binance::confirm",
                client_order_id = %coid,
                code = e.code,
                msg = %e.msg,
                "confirm: status query failed; saying nothing (watchdog and reconcile decide)"
            );
            Vec::new()
        }
    }
}

/// A REST client that can answer the active confirm. Implemented by the four spot / perp clients of
/// both venues; their `VenueRest::confirm_order` is `confirm_when(.., true)`.
pub trait ConfirmByCoid: VenueRest {
    /// Query the order by coid and apply [`resolve_confirm`]. `dispatched` = the exec thread has
    /// finished this coid's submit ([`DispatchLedger::finished`]); the caller reads it BEFORE
    /// calling, so an "absent" answer cannot predate the submit.
    fn confirm_when(&self, coid: &str, ts: i64, dispatched: bool) -> Vec<Event>;
}

/// The coids whose submit the exec thread has finished dispatching (marked AFTER the REST call
/// returned, so an in-flight POST is not "finished"). Bounded by [`MAP_CAP`].
#[derive(Default)]
pub struct DispatchLedger(Mutex<HashSet<String>>);

impl DispatchLedger {
    pub fn mark(&self, coid: &str) {
        let mut set = lock(&self.0);
        if set.len() >= MAP_CAP {
            set.clear();
        }
        set.insert(coid.to_string());
    }

    pub fn finished(&self, coid: &str) -> bool {
        lock(&self.0).contains(coid)
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

/// Admission to the confirm client: a bounded in-flight count and a per-coid backoff.
#[derive(Default)]
struct ConfirmGate {
    in_flight: AtomicUsize,
    last: Mutex<HashMap<String, Instant>>,
}

struct Permit<'a>(&'a AtomicUsize);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ConfirmGate {
    fn enter(&self, coid: &str) -> Option<Permit<'_>> {
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_CONFIRMS_IN_FLIGHT {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let permit = Permit(&self.in_flight);
        let now = Instant::now();
        let mut last = lock(&self.last);
        if last.get(coid).is_some_and(|t| now.duration_since(*t) < CONFIRM_BACKOFF) {
            return None; // `permit` drops here and releases the slot
        }
        if last.len() >= MAP_CAP {
            last.retain(|_, t| now.duration_since(*t) < CONFIRM_BACKOFF);
        }
        last.insert(coid.to_string(), now);
        Some(permit)
    }
}

/// The `ExecActor::with_confirm` closure for one exec client: ONE REST client for every confirm
/// (one connection pool, serialized behind a mutex), `prepare` run on it until it returns `true`
/// (spot's server-time skew read, lazily on the first confirm so building the closure costs no
/// network; a perp passes `|_| true`), and [`ConfirmGate`] bounding the storm.
pub fn confirm_closure<R, P>(rest: R, ledger: Arc<DispatchLedger>, prepare: P) -> ConfirmFn
where
    R: ConfirmByCoid + 'static,
    P: Fn(&R) -> bool + Send + Sync + 'static,
{
    let client = Mutex::new((rest, false));
    let gate = ConfirmGate::default();
    Arc::new(move |coid: &str| {
        let Some(_permit) = gate.enter(coid) else {
            return Vec::new();
        };
        let mut guard = lock(&client);
        let (rest, prepared) = &mut *guard;
        if !*prepared {
            *prepared = prepare(rest);
        }
        // Read BEFORE the query: "finished" must predate an "absent" answer.
        let dispatched = ledger.finished(coid);
        rest.confirm_when(coid, now_ms(), dispatched)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mock;
    impl VenueRest for Mock {
        fn submit_order(&self, _request: &OrderRequest) -> Vec<Event> {
            Vec::new()
        }
        fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
            Ok(())
        }
    }

    fn request(coid: &str) -> OrderRequest {
        OrderRequest { client_order_id: coid.to_string(), ..Default::default() }
    }

    #[test]
    fn dispatched_marks_a_coid_only_after_its_submit_returned() {
        let ledger = Arc::new(DispatchLedger::default());
        let rest = Dispatched { rest: Mock, ledger: ledger.clone() };
        assert!(!ledger.finished("a"));
        rest.submit_order(&request("a"));
        assert!(ledger.finished("a"));
        // the batch door marks every member (a defaulted submit_batch would have skipped it)
        rest.submit_batch(&[request("b"), request("c")]);
        assert!(ledger.finished("b") && ledger.finished("c"));
        assert!(!ledger.finished("d"));
    }

    #[test]
    fn the_gate_bounds_in_flight_confirms_and_backs_a_coid_off() {
        let gate = ConfirmGate::default();
        let held: Vec<_> = (0..MAX_CONFIRMS_IN_FLIGHT)
            .map(|i| gate.enter(&format!("c{i}")).expect("a free slot"))
            .collect();
        assert!(gate.enter("extra").is_none(), "the fifth concurrent confirm is skipped");
        drop(held);
        let first = gate.enter("again").expect("slots are free again");
        drop(first);
        assert!(gate.enter("again").is_none(), "the same coid inside the backoff is skipped");
        assert!(gate.enter("other").is_some(), "a different coid is not");
        assert_eq!(gate.in_flight.load(Ordering::SeqCst), 0, "a denied entry returns its slot");
    }
}
