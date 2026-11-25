//! What a binance-family by-order-client-id `GET .../order` answer becomes, shared by binance and aster. The
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

use super::order_word::OrderWord;
use super::rest_reads::FoundOrder;

/// The events for an order the venue HAS, read by its `status` (see the module doc).
pub fn found_events(order_client_id: &str, ts: i64, found: &FoundOrder) -> Vec<Event> {
    let accepted = || {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(found.order_id.clone().into()),
            timestamp: ts,
        })
    };
    match OrderWord::parse(&found.status) {
        OrderWord::Rejected => vec![Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.to_string(),
            reason: "venue reports the order REJECTED".to_string().into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        })],
        OrderWord::Canceled => vec![
            accepted(),
            Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: "venue reports the order CANCELED".to_string().into(),
                timestamp: ts,
                post_only_would_cross: false,
                refusal_id: None,
            }),
        ],
        OrderWord::Expired => vec![
            accepted(),
            Event::OrderExpired(OrderExpired {
                order_client_id: order_client_id.to_string(),
                timestamp: ts,
            }),
        ],
        // The order is (or was) live; its fills and terminal ride user-data and the gap sentinel.
        OrderWord::New
        | OrderWord::LiveOther
        | OrderWord::PartiallyFilled
        | OrderWord::Filled
        | OrderWord::PendingCancel
        | OrderWord::Unknown => vec![accepted()],
    }
}

/// An AMBIGUOUS SUBMIT's re-query (the POST timed out and may be live): found -> [`found_events`];
/// absent -> NOTHING, as `vike_bridge_core::resolve_ambiguous_submit` says: read milliseconds after a
/// `503` or a `-1007` it proves nothing, and a POST landing after it would have its `NEW` frame dropped
/// as stranded. A FAILED query -> NOTHING too, like the active confirm: an id-less accept would be a
/// phantom when the order never landed (the stuck-order watchdog sweeps only an order still pre-ack).
/// So the watchdog's active confirm and backstop and the user-data stream decide, by 75 s.
pub fn resolve_submit(
    order_client_id: &str,
    ts: i64,
    query: Result<Option<FoundOrder>, VenueApiError>,
) -> Vec<Event> {
    match query {
        Ok(Some(found)) => found_events(order_client_id, ts, &found),
        Ok(None) => {
            tracing::warn!(
                target: "vike_binance::confirm",
                order_client_id = %order_client_id,
                "ambiguous submit: the venue does not list the order yet; saying nothing (watchdog confirm and user-data decide)"
            );
            Vec::new()
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_binance::confirm",
                order_client_id = %order_client_id,
                code = e.code,
                msg = %e.msg,
                "ambiguous submit: status query failed; saying nothing (watchdog confirm and user-data decide)"
            );
            Vec::new()
        }
    }
}
