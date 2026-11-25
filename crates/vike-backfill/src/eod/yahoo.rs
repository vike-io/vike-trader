//! Yahoo Finance EOD source — keyless (but ToS-gray, best-effort) daily-OHLCV JSON.
//!
//! `GET {base}/v8/finance/chart/{ticker}?interval=1d&range={years}y` (`base` is the `yahoo`
//! `venue_url` row, `https://query1.finance.yahoo.com` on a fresh box) returns the v8 chart JSON.
//! The request names this client in its `User-Agent` (`vike_transport::http::USER_AGENT`, which every
//! collector agent sends; measured 2026-11-16 from a
//! datacenter box: a `200` with the chart). Yahoo index tickers use a `^` prefix but a DIFFERENT
//! spelling than stooq (e.g. `^GSPC` S&P500, `^VIX`, `^NDX` Nasdaq-100, `^DJI` Dow) — the bin's
//! per-symbol mapping supplies the right ticker per source. The official Yahoo API is retired; this unofficial endpoint can break without notice.
//!
//! The same chart, asked with `events=split`, also lists a ticker's SPLITS ([`fetch_splits`]): the datahub fills the
//! settings database's `stock_split` rows from it at the operator's `vike-cli data catalog refresh`
//! (`docs/decisions/0289-a-backtest-split-adjusts-a-share-series-from-the-operators-split-rows.md`'s amendment of
//! 2026-11-16).

use vike_marketdata::Bar;

use super::{EodSource, http_get, parse_yahoo_json};
use crate::error::CollectError;

pub struct YahooSource;

impl EodSource for YahooSource {
    fn name(&self) -> &str {
        "yahoo"
    }

    fn fetch(&self, base: &str, ticker: &str, years: u32) -> Result<Vec<Bar>, CollectError> {
        let body = http_get(&chart_url(base, ticker, years))?;
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| CollectError::Fetch(format!("yahoo json parse: {e}").into()))?;
        parse_yahoo_json(&v)
    }
}

/// The v8 chart URL of `ticker` at `base`. `range={years}y` bounds the window server-side, so
/// `parse_yahoo_json` needs no further filter.
fn chart_url(base: &str, ticker: &str, years: u32) -> String {
    let years = years.max(1);
    format!("{base}/v8/finance/chart/{ticker}?interval=1d&range={years}y")
}

/// One split Yahoo lists: the first day at the new price (`YYYY-MM-DD`, UTC) and the ratio as Yahoo states it, new
/// shares (`numerator`) per old shares (`denominator`): `4:1` for a 4-for-1, `1:10` for a 1-for-10 reverse split,
/// `1067:1000` for a spin-off Yahoo lists as a split.
#[derive(Clone, Debug, PartialEq)]
pub struct YahooSplit {
    pub date: String,
    pub numerator: f64,
    pub denominator: f64,
}

/// **Every split Yahoo lists for `ticker`**, from `base` (the `yahoo` `venue_url` row), oldest first. Empty for a
/// ticker that never split. One GET of the whole history at a quarterly interval: the events come for the whole range
/// whatever the interval, and the bars beside them stay few.
///
/// # Errors
/// [`YahooSplitsError::Unlisted`]: Yahoo answered `404`, it lists no such ticker. [`YahooSplitsError::Failed`]: any
/// other failure of the GET, or a body that is not a chart ([`parse_yahoo_splits`]).
pub fn fetch_splits(base: &str, ticker: &str) -> Result<Vec<YahooSplit>, YahooSplitsError> {
    let url = format!("{base}/v8/finance/chart/{ticker}?interval=3mo&range=max&events=split");
    let body =
        crate::http::get_to_string_answered(&url, &crate::http::GetOptions::default(), "eod")
            .map_err(|failure| match failure.status {
                Some(404) => YahooSplitsError::Unlisted(failure.error),
                _ => YahooSplitsError::Failed(failure.error),
            })?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        YahooSplitsError::Failed(CollectError::Fetch(format!("yahoo json parse: {e}").into()))
    })?;
    parse_yahoo_splits(&v).map_err(YahooSplitsError::Failed)
}

/// Why [`fetch_splits`] gave no list, in the words of the error each variant holds (its `Display`). A caller decides
/// on the variant, never on the words (decision 0301): the datahub asks a share-class ticker's other spelling
/// (`BRK-B` for `BRKB`) after [`YahooSplitsError::Unlisted`] alone.
#[derive(Debug)]
pub enum YahooSplitsError {
    /// Yahoo answered `404`: it lists no such ticker.
    Unlisted(CollectError),
    /// Any other failure.
    Failed(CollectError),
}

impl std::fmt::Display for YahooSplitsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            YahooSplitsError::Unlisted(error) | YahooSplitsError::Failed(error) => error.fmt(f),
        }
    }
}

/// Parse the splits of a v8 chart body asked with `events=split`: `chart.result[0].events.splits`, a map whose
/// values carry `date` (epoch SECONDS, the market open of the first day at the new price), `numerator` and
/// `denominator`. A chart with no `events.splits` lists none. An event whose two sides are not finite numbers above
/// zero is skipped (it states no split).
///
/// # Errors
/// No `chart.result[0]`: Yahoo's own `chart.error.description` when it gave one.
pub fn parse_yahoo_splits(v: &serde_json::Value) -> Result<Vec<YahooSplit>, CollectError> {
    let chart = v.get("chart");
    let Some(result) = chart.and_then(|c| c.get("result")).and_then(|r| r.get(0)) else {
        let why = chart
            .and_then(|c| c.pointer("/error/description"))
            .and_then(|d| d.as_str())
            .unwrap_or("missing chart.result[0]");
        return Err(CollectError::Fetch(format!("yahoo: {why}").into()));
    };
    let Some(events) = result.pointer("/events/splits").and_then(|s| s.as_object()) else {
        return Ok(Vec::new());
    };
    let side = |x: f64| x.is_finite() && x > 0.0;
    let mut out: Vec<(i64, f64, f64)> = events
        .values()
        .filter_map(|e| {
            let seconds = e.get("date")?.as_i64()?;
            let (numerator, denominator) =
                (e.get("numerator")?.as_f64()?, e.get("denominator")?.as_f64()?);
            (side(numerator) && side(denominator)).then_some((seconds, numerator, denominator))
        })
        .collect();
    out.sort_by_key(|(seconds, _, _)| *seconds);
    Ok(out
        .into_iter()
        .map(|(seconds, numerator, denominator)| YahooSplit {
            date: vike_model::time::epoch_ms_to_utc_date(seconds * 1000),
            numerator,
            denominator,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(date: &str, numerator: f64, denominator: f64) -> YahooSplit {
        YahooSplit { date: date.to_string(), numerator, denominator }
    }

    /// **A real answer of Yahoo's chart with `events=split` is parsed** (fetched 2026-11-16 and trimmed to two bars):
    /// Netflix's three splits, the 10-for-1 of 2025-11-17 last; Apple's five, the 4-for-1 of 2020-08-31 last; and a
    /// ticker that never split (Accenture) lists none.
    #[test]
    fn a_yahoo_chart_answer_lists_its_splits_oldest_first() {
        let parse = |body: &str| {
            parse_yahoo_splits(&serde_json::from_str(body).expect("json")).expect("a chart")
        };
        assert_eq!(
            parse(crate::testing::YAHOO_NFLX_SPLITS),
            [
                split("2004-02-12", 2.0, 1.0),
                split("2015-07-15", 7.0, 1.0),
                split("2025-11-17", 10.0, 1.0)
            ]
        );
        assert_eq!(
            parse(crate::testing::YAHOO_AAPL_SPLITS),
            [
                split("1987-06-16", 2.0, 1.0),
                split("2000-06-21", 2.0, 1.0),
                split("2005-02-28", 2.0, 1.0),
                split("2014-06-09", 7.0, 1.0),
                split("2020-08-31", 4.0, 1.0)
            ]
        );
        assert!(parse(crate::testing::YAHOO_ACN_NO_SPLITS).is_empty());
    }

    /// Yahoo's own words for a ticker it does not know, never an empty list in their place; a reverse split keeps its
    /// two sides, and an event that states no ratio is skipped.
    #[test]
    fn an_unknown_ticker_is_an_error_and_a_reverse_split_keeps_its_sides() {
        let unknown: serde_json::Value = serde_json::from_str(
            r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#,
        )
        .unwrap();
        let why = parse_yahoo_splits(&unknown).expect_err("no chart").to_string();
        assert!(why.contains("No data found, symbol may be delisted"), "{why}");
        let reverse: serde_json::Value = serde_json::from_str(
            r#"{"chart":{"result":[{"events":{"splits":{
                "1":{"date":1700000400,"numerator":1.0,"denominator":10.0},
                "2":{"date":1600000000,"numerator":2.0,"denominator":0.0}}}}]}}"#,
        )
        .unwrap();
        assert_eq!(
            parse_yahoo_splits(&reverse).expect("a chart"),
            [split("2023-11-14", 1.0, 10.0)]
        );
    }

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
