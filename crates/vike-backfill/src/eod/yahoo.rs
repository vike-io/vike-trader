//! Yahoo Finance EOD source — keyless (but ToS-gray, best-effort) daily-OHLCV JSON.
//!
//! `GET {base}/v8/finance/chart/{ticker}?interval=1d&range={years}y` (`base` is the `yahoo`
//! `venue_url` row, `https://query1.finance.yahoo.com` on a fresh box) returns the v8 chart JSON. Requires a `User-Agent` header (Yahoo 403s without one). Yahoo index
//! tickers use a `^` prefix but a DIFFERENT spelling than stooq (e.g. `^GSPC` S&P500, `^VIX`,
//! `^NDX` Nasdaq-100, `^DJI` Dow) — the bin's per-symbol mapping supplies the right ticker per
//! source. The official Yahoo API is retired; this unofficial endpoint can break without notice.

use vike_marketdata::Bar;

use super::{EodSource, http_get, parse_yahoo_json};
use crate::error::CollectError;

/// A browser-ish UA — Yahoo returns 403 to the default agent UA.
const YAHOO_UA: &str = "Mozilla/5.0 (compatible; vike-backfill/1.0; +https://github.com/vike-io)";

pub struct YahooSource;

impl EodSource for YahooSource {
    fn name(&self) -> &str {
        "yahoo"
    }

    fn fetch(&self, base: &str, ticker: &str, years: u32) -> Result<Vec<Bar>, CollectError> {
        let body = http_get(&chart_url(base, ticker, years), Some(YAHOO_UA))?;
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| CollectError::Fetch(format!("yahoo json parse: {e}")))?;
        parse_yahoo_json(&v)
    }
}

/// The v8 chart URL of `ticker` at `base`. `range={years}y` bounds the window server-side, so
/// `parse_yahoo_json` needs no further filter.
fn chart_url(base: &str, ticker: &str, years: u32) -> String {
    let years = years.max(1);
    format!("{base}/v8/finance/chart/{ticker}?interval=1d&range={years}y")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On the seed's row the URL is the one the bin dialled before the base became a row, byte for
    /// byte.
    #[test]
    fn the_chart_url_on_the_seed_is_the_one_dialled_before() {
        use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};
        let seed = VenueUrls::seed("yahoo");
        let base = seed.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "").unwrap();
        assert_eq!(
            chart_url(base, "^GSPC", 0),
            "https://query1.finance.yahoo.com/v8/finance/chart/^GSPC?interval=1d&range=1y"
        );
    }
}
