//! Polymarket CLOB data reads (UNAUTHENTICATED). Reuses the existing [`RestTransport`] public-GET
//! seam. A "market" is an ERC-1155 outcome `token_id` (YES/NO); prices/sizes are decimal strings
//! in probability space (0..1).

use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_marketdata::L2BookLevel;

/// A CLOB order book snapshot for one outcome token, as the wire states it: the WIRE DTO, never a
/// depth book. Folding and every read of the top (best bid/ask, mid) are `vike_marketdata::L2Book`'s;
/// the live feed folds these levels into one (`crate::feed`) and records them raw.
#[derive(Debug, Clone, PartialEq)]
pub struct PolyBook {
    pub asset_id: String,
    /// (price, size) — as returned, in the wire's order and at the wire's exact prices.
    pub bids: Vec<L2BookLevel>,
    pub asks: Vec<L2BookLevel>,
    /// The tick the venue states beside the levels (`tick_size`), `None` when it states no usable one: what
    /// seats a token whose `/tick-size` lookup failed ([`super::state::TokenSlot::awaiting_tick`]).
    pub tick_size: Option<f64>,
}

fn parse_levels(v: &serde_json::Value, key: &str) -> Vec<L2BookLevel> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    let p = l.get("price").and_then(|x| x.as_str())?.parse().ok()?;
                    let s = l.get("size").and_then(|x| x.as_str())?.parse().ok()?;
                    Some(L2BookLevel { price: p, qty: s })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a CLOB `/book` response into a [`PolyBook`].
pub fn parse_book(v: &serde_json::Value) -> PolyBook {
    PolyBook {
        asset_id: v.get("asset_id").and_then(|a| a.as_str()).unwrap_or_default().to_string(),
        bids: parse_levels(v, "bids"),
        asks: parse_levels(v, "asks"),
        tick_size: v
            .get("tick_size")
            .and_then(vike_bridge_core::json::json_number)
            .filter(|tick| tick.is_finite() && *tick > 0.0),
    }
}

/// Fetch the full order book for an outcome `token_id` from the CLOB at `clob` (the venue's CLOB
/// row, `crate::config`'s `clob_base`).
pub fn fetch_book(
    t: &dyn RestTransport,
    clob: &str,
    token_id: &str,
) -> Result<PolyBook, VenueApiError> {
    let v = t.public(clob, "/book", &[("token_id", token_id.to_string())])?;
    Ok(parse_book(&v))
}

/// Fetch the midpoint price (probability) for an outcome `token_id` from the CLOB at `clob`.
pub fn fetch_midpoint(
    t: &dyn RestTransport,
    clob: &str,
    token_id: &str,
) -> Result<f64, VenueApiError> {
    let v = t.public(clob, "/midpoint", &[("token_id", token_id.to_string())])?;
    v.get("mid")
        .and_then(|m| m.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| VenueApiError { code: 0, msg: "no mid in /midpoint response".to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladders come back as the wire wrote them: its order, its exact prices.
    #[test]
    fn book_parse_keeps_the_wire_levels() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"market":"0xabc","asset_id":"71321045",
                "bids":[{"price":"0.51","size":"120"},{"price":"0.52","size":"80"}],
                "asks":[{"price":"0.55","size":"60"},{"price":"0.54","size":"90"}]}"#,
        )
        .unwrap();
        let b = parse_book(&v);
        assert_eq!(b.asset_id, "71321045");
        assert_eq!(b.bids, vec![L2BookLevel::new(0.51, 120.0), L2BookLevel::new(0.52, 80.0)]);
        assert_eq!(b.asks, vec![L2BookLevel::new(0.55, 60.0), L2BookLevel::new(0.54, 90.0)]);
    }

    #[test]
    fn empty_book_has_no_levels() {
        let b = parse_book(&serde_json::json!({"asset_id":"x"}));
        assert!(b.bids.is_empty() && b.asks.is_empty());
    }
}
