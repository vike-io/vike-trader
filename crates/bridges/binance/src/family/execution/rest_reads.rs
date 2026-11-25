//! The REST reads (and the one by-order-client-id cancel, [`delete_by_order_client_id`], and the one perp amend,
//! [`modify_perp_order`]) the signed spot and perp REST clients of BOTH family venues issue with the
//! SAME shape and a different PATH — `VenueRestBinanceSpot`/`VenueRestBinancePerp` here, `VenueRestAsterSpot`/
//! `VenueRestAsterPerp` in vike-aster. Each pair of methods was a byte-identical copy apart from the
//! endpoint literal (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`), so the body lives
//! once here and each client's method is a one-line delegation passing its own transport, signer,
//! host, path and symbol.
//!
//! ⚠ What is NOT here: `resolve_ambiguous_submit`. What each venue hands [`query_order_orderid`]
//! differs — binance re-applies its broker-order-client-id prefix (`link_id`) to build the `origClientOrderId`
//! while aster sends the bare id — so this module takes the venue's `origClientOrderId` IN. The one
//! `resolve_ambiguous_submit` (and the one by-order-client-id lookup, the cancel, the submit and the rest of the
//! clients' bodies) is [`super::signed_rest`]'s since the owner's ruling of 2026-11-13, where the
//! stamp is the client's `FamilySignedRest::link_id` (`None` on aster: the bare id, byte for byte); the
//! decision it feeds is [`super::confirm`]'s (`resolve_submit` / `resolve_confirm`, over
//! `vike_bridge_core::resolve_ambiguous_submit`).
//!
//! Trait objects (`&dyn RestTransport`, `&dyn Signer`) rather than generics: the clients are generic
//! over `S: Signer, T: RestTransport` and coerce at the call, and the transport seam's own methods
//! already take `&dyn Signer`, so nothing is erased that the seam had not already erased.

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::rest::amend_answer_time;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport, VenueApiError};
use vike_bridge_core::venue_adapter::missing_time_field;
use vike_model::events::{Event, OrderModified};
use vike_model::{OrderRequest, SymbolProperties};

use super::order_map::json_id;

/// Audit A3 resync: recent order states (`GET <path>`, an `allOrders` endpoint) for the
/// post-reconnect history replay, on the short-timeout requery transport so the resync can't stall.
/// Returns the raw JSON array.
///
/// `path` is the per-venue delta — spot is `/api/v3/allOrders` on both venues, perp is
/// `/fapi/v1/allOrders` on binance and `/fapi/v3/allOrders` on aster. Used by the spot and perp
/// clients of both.
pub fn get_all_orders(
    transport: &dyn RestTransport,
    signer: &dyn Signer,
    base_url: &str,
    path: &str,
    symbol: &str,
    limit: u32,
) -> Result<serde_json::Value, VenueApiError> {
    let params: Vec<(&str, String)> =
        vec![("symbol", symbol.to_string()), ("limit", limit.to_string())];
    transport.signed_requery(base_url, path, "GET", &params, signer)
}

/// What the by-order-client-id status query found: the venue's `orderId` and its `status` word (`NEW`,
/// `PARTIALLY_FILLED`, `FILLED`, `CANCELED`, `EXPIRED`, `EXPIRED_IN_MATCH`, `REJECTED`, ...; empty
/// when the body carried none). The status is what lets a re-query tell a LIVE order from one the
/// venue has already closed ([`super::confirm::found_events`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundOrder {
    pub order_id: String,
    pub status: String,
}

/// Audit T1: re-query the venue for the order by our idempotent order_client_id (`GET <path>`, the
/// signed single-order endpoint) to resolve an ambiguous (timed-out) submit. `Ok(Some(found))` = the
/// venue HAS the order (read its `status`: it may already be CANCELED/EXPIRED/REJECTED),
/// `Ok(None)` = the venue says it never landed (`crate::transport::error_codes::says_order_unknown`), `Err` = the
/// query itself failed.
/// Runs on `signed_requery` (the short-timeout transport) so a double-timeout can't stall the core
/// toward ~60s.
///
/// `orig` is the `origClientOrderId` AS THE VENUE STORED IT, and building it is the caller's job —
/// it must match the venue's OWN stored `clientOrderId` exactly. Binance re-prefixes the bare local
/// id the same way submit stamped it ([`super::order_map::binance_broker_order_client_id`], task 6) and so
/// must never send it bare when a link id is configured, or the venue would report the order
/// unknown; aster stamps no prefix and sends the bare id. `path` is the other per-venue delta:
/// each client's `PATH_ORDER`. Used by the spot and perp clients of both.
pub fn query_order_orderid(
    transport: &dyn RestTransport,
    signer: &dyn Signer,
    base_url: &str,
    path: &str,
    symbol: &str,
    orig: &str,
) -> Result<Option<FoundOrder>, VenueApiError> {
    let params: Vec<(&str, String)> =
        vec![("symbol", symbol.to_string()), ("origClientOrderId", orig.to_string())];
    match transport.signed_requery(base_url, path, "GET", &params, signer) {
        // A 2xx that names no order is not "the venue has no such order": only the venue's own code says
        // that. Read as absent, it was the terminal reject of an order that may rest.
        Ok(response) => match response.get("orderId").map(json_id).filter(|id| !id.is_empty()) {
            Some(order_id) => Ok(Some(FoundOrder {
                order_id,
                status: response.get("status").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            })),
            None => Err(VenueApiError {
                code: E_TIMEOUT_AMBIGUOUS,
                msg: format!("GET {path} answered 2xx with no orderId"),
            }),
        },
        Err(exc) if crate::transport::error_codes::says_order_unknown(&exc) => Ok(None),
        Err(exc) => Err(exc),
    }
}

/// `DELETE <path>` one order by `origClientOrderId`: the family's single cancel, the raw verb (the
/// caller judges the venue's answer, `vike_bridge_core::rest::OrderCancelAck::from_venue_answer` over
/// [`crate::transport::error_codes::cancel_says_order_unknown`] where "unknown" is "already gone"). `orig` is the
/// id AS THE VENUE STORED IT, as for [`query_order_orderid`]: binance's broker-stamped, aster's bare.
/// `path` is each client's `PATH_ORDER`. Used by the spot and perp clients of both.
pub fn delete_by_order_client_id(
    transport: &dyn RestTransport,
    signer: &dyn Signer,
    base_url: &str,
    path: &str,
    symbol: &str,
    orig: &str,
) -> Result<(), VenueApiError> {
    let params: Vec<(&str, String)> =
        vec![("symbol", symbol.to_string()), ("origClientOrderId", orig.to_string())];
    transport.signed(base_url, path, "DELETE", &params, signer).map(|_| ())
}

/// What a family venue's perp amend ([`modify_perp_order`]) differs by: venue facts, each venue's own
/// constant (`crate::execution::rest::perp`'s `PERP_MODIFY`, aster's twin).
pub struct PerpModifySpec {
    /// The venue's `PATH_ORDER`: binance `/fapi/v1/order`, aster `/fapi/v3/order`.
    pub path: &'static str,
    /// The reply field holding the venue's time of the amend (`updateTime` on both; the owner's
    /// 2026-11-12 ruling: absent, the time the reply was received).
    pub time_field: &'static str,
}

/// `PUT <path>`: the native perp amend of both family venues (RUST-NATIVE, no Python twin). The fapi
/// modify requires `side` PLUS both `quantity` and `price`, so an unchanged field falls back to the
/// resting `order`'s value (this is why the modify seam passes the whole order). `[OrderModified]` on
/// the venue's ack, carrying its `orderId` and its time ([`PerpModifySpec::time_field`], else the
/// receipt time); a failure keeps the order's terms and is the non-terminal
/// [`crate::transport::error_codes::modify_failed`] advisory (the failed INTENT must not vanish, audit T2).
///
/// `orig` is the `origClientOrderId` AS THE VENUE STORED IT, as for [`query_order_orderid`]: binance's
/// broker-stamped, aster's bare. `symbol` is the EXCHANGE symbol and `properties` its grid.
pub fn modify_perp_order(
    transport: &dyn RestTransport,
    signer: &dyn Signer,
    base_url: &str,
    spec: &PerpModifySpec,
    symbol: &str,
    properties: &SymbolProperties,
    orig: &str,
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Vec<Event> {
    let qty = new_qty.unwrap_or(order.qty);
    let price = new_price.or(order.price).unwrap_or(0.0);
    let params: Vec<(&str, String)> = vec![
        ("symbol", symbol.to_string()),
        ("side", if order.side > 0 { "BUY" } else { "SELL" }.to_string()),
        ("origClientOrderId", orig.to_string()),
        ("quantity", format_to_step_f(qty, properties.step_size)),
        ("price", format_to_step_f(price, properties.tick_size)),
    ];
    match transport.signed(base_url, spec.path, "PUT", &params, signer) {
        Ok(response) => vec![Event::OrderModified(OrderModified {
            order_client_id: order.order_client_id.clone(),
            venue_order_id: Some(response.get("orderId").map(json_id).unwrap_or_default().into()),
            new_qty,
            new_price,
            new_trigger_price: None,
            timestamp: amend_answer_time(response.get(spec.time_field).and_then(|t| t.as_i64())),
        })],
        Err(exc) => {
            vec![crate::transport::error_codes::modify_failed(&order.order_client_id, &exc)]
        }
    }
}

/// `GET <path>` (a host's `/time`) → server-minus-local offset in ms, for clock correction. A missing
/// or non-integer `serverTime` reads as "no skew" (`local_now_ms`), never as an error. Pure query:
/// applying the correction is the caller's job, [`crate::family::transport::RestTransportOnVenueClock`]'s for every client.
pub fn server_time_offset(
    transport: &dyn RestTransport,
    base_url: &str,
    path: &str,
    local_now_ms: i64,
) -> Result<i64, VenueApiError> {
    let response = transport.public(base_url, path, &[])?;
    let server = response.get("serverTime").and_then(|t| t.as_i64()).unwrap_or(local_now_ms);
    Ok(server - local_now_ms)
}

/// `{"serverTime": <epoch ms>}`, the `/time` answer of both venues, for each venue adapter's clock
/// read. Unlike [`server_time_offset`] a missing field is an ERROR here. Split from the fetch so the
/// field name is gated by each venue's own captured fixture (its `adapter_tests.rs`), not a live call.
pub fn parse_server_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("serverTime")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| missing_time_field("serverTime"))
}
