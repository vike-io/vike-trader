//! The three REST reads the signed spot and perp REST clients of BOTH family venues issue with the
//! SAME shape and a different PATH — `BinanceSpotRest`/`BinancePerpRest` here, `AsterSpotRest`/
//! `AsterPerpRest` in vike-aster. Each pair of methods was a byte-identical copy apart from the
//! endpoint literal (Binance's `/fapi/v1|v2` split vs Aster's single `/fapi/v3`), so the body lives
//! once here and each client's method is a one-line delegation passing its own transport, signer,
//! host, path and symbol.
//!
//! ⚠ What is NOT here, and stays per-venue on purpose: `resolve_ambiguous_submit`. Its four copies
//! read identically, but each one's only callee is its own `query_order_orderid`, and THAT differs —
//! binance re-applies its broker-coid prefix (`link_id`) before the `origClientOrderId` query while
//! aster sends the bare id — so it is the same text over two different semantics. The decision it
//! feeds is already shared one layer down (`vike_bridge_core::resolve_ambiguous_submit`).
//!
//! Trait objects (`&dyn RestTransport`, `&dyn Signer`) rather than generics: the clients are generic
//! over `S: Signer, T: RestTransport` and coerce at the call, and the transport seam's own methods
//! already take `&dyn Signer`, so nothing is erased that the seam had not already erased.

use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

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

/// `GET <path>` (the spot `/api/v3/time` on both venues) → server-minus-local offset in ms, for
/// `Signer::set_offset_ms`-style clock correction. A missing or non-integer `serverTime` reads as
/// "no skew" (`local_now_ms`), never as an error. Pure query — applying the correction to a signer
/// is the caller's job. Used by the spot clients of both venues.
pub fn server_time_offset(
    transport: &dyn RestTransport,
    base_url: &str,
    path: &str,
    local_now_ms: i64,
) -> Result<i64, VenueApiError> {
    let resp = transport.public(base_url, path, &[])?;
    let server = resp.get("serverTime").and_then(|t| t.as_i64()).unwrap_or(local_now_ms);
    Ok(server - local_now_ms)
}

/// fapi wallet balance (`GET <path>`, the signed `balance` endpoint) → the USDT `balance` (total
/// wallet cash, not counting unrealized — consistent with Bybit walletBalance). Default-safe: any
/// failure, or no USDT row, returns 0.0 so reconcile is never broken.
///
/// `path` is the per-venue delta: `/fapi/v2/balance` on binance, `/fapi/v3/balance` on aster. Used
/// by the perp clients of both.
pub fn fetch_usdt_balance(
    transport: &dyn RestTransport,
    signer: &dyn Signer,
    base_url: &str,
    path: &str,
) -> f64 {
    let Ok(rows) = transport.signed(base_url, path, "GET", &[], signer) else {
        return 0.0;
    };
    for entry in rows.as_array().unwrap_or(&vec![]) {
        if entry.get("asset").and_then(|a| a.as_str()) == Some("USDT") {
            return entry.get("balance").and_then(json_num).unwrap_or(0.0);
        }
    }
    0.0
}
