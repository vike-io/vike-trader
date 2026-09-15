//! EOD (end-of-day) daily-OHLCV backfill — a **pluggable multi-source** collector for equity
//! indices / stocks (S&P 500, VIX, Nasdaq, Dow, individual tickers). Fetch daily bars for a
//! configurable symbol from a selectable provider and ingest them as `kind=bar` (interval `1d`)
//! into the `vike-data` `HistStore`.
//!
//! These are read-only DATA sources, not execution venues — so they live here in vike-backfill (the
//! historical-backfill crate), NOT under `crates/bridges/` (which is for venues we trade). Nothing
//! depends on live trading of the ingested instruments.
//!
//! The extensibility point is the [`EodSource`] trait: adding a provider is one impl (~30 lines) —
//! build a URL, GET it, hand the body to a PURE parser ([`parse_yahoo_json`], fixture-tested, no
//! network). The single shipped provider is [`yahoo`] (live-verified). A stooq provider was dropped
//! after a live test showed stooq now fronts its CSV endpoint with a JavaScript proof-of-work
//! anti-bot wall that a plain HTTP client can't pass; the trait means future keyless sources (or
//! bring-your-own-key ones like Alpha Vantage / Tiingo) slot in as new files.
//! Design: `docs/superpowers/specs/2026-07-11-eod-backfill-design.md`.

pub mod yahoo;

use vike_model::Bar;

use crate::error::CollectError;

/// A pluggable end-of-day data provider. One impl per source.
pub trait EodSource {
    /// Provider name — used as the default store `venue` (provenance) and in logs.
    fn name(&self) -> &str;
    /// Daily OHLCV bars for the provider's own `ticker` symbol, most-recent `years` back,
    /// ts-ascending. `ticker` is the SOURCE-specific symbol (e.g. yahoo `^GSPC` for the S&P 500);
    /// the bin maps a vike symbol → this ticker.
    fn fetch(&self, ticker: &str, years: u32) -> Result<Vec<Bar>, CollectError>;
}

/// Resolve a provider by name. Adding a source = one arm here + one impl file.
pub fn source_by_name(name: &str) -> Option<Box<dyn EodSource>> {
    match name {
        "yahoo" => Some(Box::new(yahoo::YahooSource)),
        _ => None,
    }
}

/// The provider names `source_by_name` accepts (for `--help` / error messages).
pub const SOURCES: &[&str] = &["yahoo"];

/// Wall-clock epoch-ms (binaries/collectors may read the clock; the pure parsers never do).
/// Re-exports the shared [`vike_model::now_ms`] so `eod::now_ms` stays a stable import path.
pub use vike_model::now_ms;

/// Blocking GET → body string. `user_agent` is required by some sources (Yahoo 403s without one).
/// Non-2xx or a read failure → [`CollectError::Fetch`]. Delegates to the shared
/// [`crate::http::get_to_string`] — which uses an `http_status_as_error(false)` agent, so unlike the
/// old plain `ureq::agent()` the non-2xx body head is actually surfaced (finding F21 drift fix).
pub(crate) fn http_get(url: &str, user_agent: Option<&str>) -> Result<String, CollectError> {
    let opts = crate::http::GetOptions { user_agent, ..Default::default() };
    crate::http::get_to_string(url, &opts, "eod")
}

/// Parse a Yahoo `v8/finance/chart` JSON body: `chart.result[0].timestamp[]` (epoch SECONDS) +
/// `indicators.quote[0].{open,high,low,close,volume}[]`. An index with any `null` OHLC field is
/// skipped (Yahoo emits nulls for no-trade days). `CollectError::Fetch` on a shape mismatch / empty.
pub fn parse_yahoo_json(v: &serde_json::Value) -> Result<Vec<Bar>, CollectError> {
    let result = v
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.get(0))
        .ok_or_else(|| CollectError::Fetch("yahoo: missing chart.result[0]".into()))?;
    let ts = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .ok_or_else(|| CollectError::Fetch("yahoo: missing timestamp[]".into()))?;
    let quote = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.get(0))
        .ok_or_else(|| CollectError::Fetch("yahoo: missing indicators.quote[0]".into()))?;
    let col = |name: &str| quote.get(name).and_then(|a| a.as_array());
    let (open, high, low, close, volume) =
        match (col("open"), col("high"), col("low"), col("close"), col("volume")) {
            (Some(o), Some(h), Some(l), Some(c), Some(v)) => (o, h, l, c, v),
            _ => return Err(CollectError::Fetch("yahoo: missing OHLCV arrays".into())),
        };
    let mut out = Vec::with_capacity(ts.len());
    for (i, tsv) in ts.iter().enumerate() {
        let (Some(t), Some(o), Some(h), Some(l), Some(c)) = (
            tsv.as_i64(),
            open.get(i).and_then(|x| x.as_f64()),
            high.get(i).and_then(|x| x.as_f64()),
            low.get(i).and_then(|x| x.as_f64()),
            close.get(i).and_then(|x| x.as_f64()),
        ) else {
            continue; // null OHLC (no-trade day) or short array → skip this index
        };
        let vol = volume.get(i).and_then(|x| x.as_f64()).unwrap_or(0.0);
        out.push(Bar {
            // Yahoo's `t` is market-open epoch-SECONDS, not UTC midnight — floor to UTC-midnight
            // like every other `1d`-interval consolidator so `vike_backtest::timeframe::resample`'s
            // weekly rollup (which buckets by day) doesn't misalign against an intraday-open ts.
            ts: vike_model::time::floor_ms_to_utc_day(t * 1000),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: vol,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
    }
    if out.is_empty() {
        return Err(CollectError::Fetch("yahoo: no usable bars".into()));
    }
    out.sort_by_key(|b| b.ts);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_yahoo_json_seconds_to_ms_and_null_skip() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"chart":{"result":[{
                "timestamp":[1577836800,1577923200,1578009600],
                "indicators":{"quote":[{
                    "open":[100.0,null,102.0],
                    "high":[101.0,null,103.0],
                    "low":[99.0,null,101.5],
                    "close":[100.5,null,102.5],
                    "volume":[1000,null,1200]
                }]}
            }]}}"#,
        )
        .unwrap();
        let bars = parse_yahoo_json(&v).unwrap();
        assert_eq!(bars.len(), 2, "the null middle row is skipped");
        assert_eq!(bars[0].ts, 1_577_836_800_000, "seconds → ms");
        assert!((bars[0].open - 100.0).abs() < 1e-9);
        assert_eq!(bars[1].ts, 1_578_009_600_000);
        assert!((bars[1].close - 102.5).abs() < 1e-9);
        // ts-ascending
        assert!(bars.windows(2).all(|w| w[0].ts <= w[1].ts));
    }

    #[test]
    fn parse_yahoo_json_floors_market_open_ts_to_utc_midnight() {
        // Yahoo's real `t` is market-open epoch-seconds (NYSE ~14:30 UTC), NOT midnight — unlike
        // the sibling test above whose fixture timestamps happen to already be midnight-aligned.
        // 1_700_000_400 = 2023-11-14 21:00:00 UTC (an arbitrary non-midnight instant); its UTC day
        // starts at 1_699_920_000.
        let market_open_s = 1_700_000_400_i64;
        let day_start_ms = 1_699_920_000_000_i64;
        assert_eq!(
            vike_model::time::epoch_ms_to_utc_date(market_open_s * 1000),
            vike_model::time::epoch_ms_to_utc_date(day_start_ms),
            "test fixture sanity: same calendar day"
        );
        let v: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"chart":{{"result":[{{
                "timestamp":[{market_open_s}],
                "indicators":{{"quote":[{{
                    "open":[100.0],"high":[101.0],"low":[99.0],"close":[100.5],"volume":[1000]
                }}]}}
            }}]}}}}"#
        ))
        .unwrap();
        let bars = parse_yahoo_json(&v).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].ts, day_start_ms, "stored ts is UTC-midnight-floored, not market-open");
    }

    #[test]
    fn parse_yahoo_json_bad_shape_is_error() {
        let v: serde_json::Value = serde_json::from_str(r#"{"chart":{"result":[]}}"#).unwrap();
        assert!(parse_yahoo_json(&v).is_err());
        let v2: serde_json::Value =
            serde_json::from_str(r#"{"chart":{"result":[{"timestamp":[]}]}}"#).unwrap();
        assert!(parse_yahoo_json(&v2).is_err(), "no quote arrays → error");
    }

    #[test]
    fn source_registry() {
        assert!(source_by_name("yahoo").is_some());
        assert!(source_by_name("stooq").is_none(), "stooq was dropped (anti-bot wall)");
        assert_eq!(SOURCES, &["yahoo"]);
    }
}
