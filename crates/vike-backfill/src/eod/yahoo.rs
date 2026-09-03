//! Yahoo Finance EOD source — keyless (but ToS-gray, best-effort) daily-OHLCV JSON.
//!
//! `GET https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?interval=1d&range={years}y`
//! returns the v8 chart JSON. Requires a `User-Agent` header (Yahoo 403s without one). Yahoo index
//! tickers use a `^` prefix but a DIFFERENT spelling than stooq (e.g. `^GSPC` S&P500, `^VIX`,
//! `^NDX` Nasdaq-100, `^DJI` Dow) — the bin's per-symbol mapping supplies the right ticker per
//! source. The official Yahoo API is retired; this unofficial endpoint can break without notice.

use vike_model::Bar;

use super::{http_get, parse_yahoo_json, EodSource};
use crate::error::CollectError;

/// A browser-ish UA — Yahoo returns 403 to the default agent UA.
const YAHOO_UA: &str = "Mozilla/5.0 (compatible; vike-backfill/1.0; +https://github.com/vike-io)";

pub struct YahooSource;

impl EodSource for YahooSource {
    fn name(&self) -> &str {
        "yahoo"
    }

    fn fetch(&self, ticker: &str, years: u32) -> Result<Vec<Bar>, CollectError> {
        // `range={years}y` bounds the window server-side; parse_yahoo_json needs no further filter.
        let years = years.max(1);
        let url = format!(
            "https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?interval=1d&range={years}y"
        );
        let body = http_get(&url, Some(YAHOO_UA))?;
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| CollectError::Fetch(format!("yahoo json parse: {e}")))?;
        parse_yahoo_json(&v)
    }
}
