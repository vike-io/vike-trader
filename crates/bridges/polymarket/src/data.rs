//! Polymarket CLOB data reads (UNAUTHENTICATED). Reuses the existing [`RestTransport`] public-GET
//! seam. A "market" is an ERC-1155 outcome `token_id` (YES/NO); prices/sizes are decimal strings
//! in probability space (0..1).

use vike_bridge_core::transport::{RestTransport, VenueApiError};

use super::config::CLOB_BASE;

/// A CLOB order book snapshot for one outcome token (top-level price/size ladders).
#[derive(Debug, Clone, PartialEq)]
pub struct PolyBook {
    pub asset_id: String,
    /// (price, size) — as returned; use [`PolyBook::best_bid`]/[`best_ask`] for the top.
    pub bids: Vec<(f64, f64)>,
    pub asks: Vec<(f64, f64)>,
}

impl PolyBook {
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.iter().map(|(p, _)| *p).max_by(f64::total_cmp)
    }
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.iter().map(|(p, _)| *p).min_by(f64::total_cmp)
    }
    pub fn mid(&self) -> Option<f64> {
        Some((self.best_bid()? + self.best_ask()?) / 2.0)
    }
}

fn parse_levels(v: &serde_json::Value, key: &str) -> Vec<(f64, f64)> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    let p = l.get("price").and_then(|x| x.as_str())?.parse().ok()?;
                    let s = l.get("size").and_then(|x| x.as_str())?.parse().ok()?;
                    Some((p, s))
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
    }
}

/// Fetch the full order book for an outcome `token_id`.
pub fn fetch_book(t: &dyn RestTransport, token_id: &str) -> Result<PolyBook, VenueApiError> {
    let v = t.public(CLOB_BASE, "/book", &[("token_id", token_id.to_string())])?;
    Ok(parse_book(&v))
}

/// Fetch the midpoint price (probability) for an outcome `token_id`.
pub fn fetch_midpoint(t: &dyn RestTransport, token_id: &str) -> Result<f64, VenueApiError> {
    let v = t.public(CLOB_BASE, "/midpoint", &[("token_id", token_id.to_string())])?;
    v.get("mid")
        .and_then(|m| m.as_str())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| VenueApiError { code: 0, msg: "no mid in /midpoint response".to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_parse_and_top() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"market":"0xabc","asset_id":"71321045",
                "bids":[{"price":"0.51","size":"120"},{"price":"0.52","size":"80"}],
                "asks":[{"price":"0.55","size":"60"},{"price":"0.54","size":"90"}]}"#,
        )
        .unwrap();
        let b = parse_book(&v);
        assert_eq!(b.asset_id, "71321045");
        assert_eq!(b.bids.len(), 2);
        assert_eq!(b.best_bid(), Some(0.52)); // highest bid regardless of order
        assert_eq!(b.best_ask(), Some(0.54)); // lowest ask
        assert_eq!(b.mid(), Some(0.53));
    }

    #[test]
    fn empty_book_has_no_top() {
        let b = parse_book(&serde_json::json!({"asset_id":"x"}));
        assert_eq!(b.best_bid(), None);
        assert_eq!(b.mid(), None);
    }
}
