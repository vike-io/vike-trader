//! What a binance-family by-coid `GET .../order` answer becomes, shared by binance and aster. The
//! guard rails around the ACTIVE confirm (dispatch ledger, lookup window, the verdict, the bounded
//! closure) are ONE form for every venue in `vike_bridge_core::confirm`; this module is only the
//! family's own part, the status words.
//!
//! * **A found order is read by STATUS** ([`found_events`]): `NEW`/`PARTIALLY_FILLED`/`FILLED` are an
//!   `OrderAccepted` (fills arrive through user-data and the gap sentinel); `CANCELED` and
//!   `EXPIRED`/`EXPIRED_IN_MATCH` are the `OrderAccepted` followed by their terminal; `REJECTED` is
//!   the terminal `OrderRejected` alone. A bare "the id exists" turned a closed order into a live one.
//! * **A failed query says nothing, for both callers.** An auth / signature / ban / 5xx / clock
//!   failure says nothing about the order, so the AMBIGUOUS SUBMIT ([`resolve_submit`], whose POST
//!   may be live) and the ACTIVE CONFIRM (`vike_bridge_core::confirm::resolve_confirm`) both emit
//!   NOTHING and the watchdog ladder, the user-data stream and reconcile decide. An id-less optimistic
//!   `OrderAccepted` used to answer the submit; it was a phantom when the order never landed, which the
//!   watchdog never sweeps.

use vike_bridge_core::transport::VenueApiError;
use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderExpired, OrderRejected};

use super::rest_reads::FoundOrder;

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
/// query -> NOTHING, like the active confirm: an id-less accept would be a phantom when the order
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
