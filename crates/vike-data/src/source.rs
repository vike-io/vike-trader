//! **A venue's HISTORY SOURCE — the store-free fetch half of a backfill.**
//!
//! A venue bridge implements one of these traits; `vike-backfill` owns the other half (the
//! commit key, the still-forming-candle guard, the append); `vike-datahub` holds the registries
//! that say which bridge serves which venue. This module is the seam, in the storage crate
//! because it is the lowest crate both ends already name: every venue bridge depends on
//! `vike-data` for `live::DataClient`, and `vike-backfill` writes the store.
//!
//! **Store-free by construction.** A source returns rows. It never sees a `HistStore`, never
//! spells a commit key and never decides whether a candle is still forming, so every venue
//! inherits the same ingest and a new venue is one impl plus one registry row
//! (`docs/decisions/0094-backfill-names-no-venue.md`).

use vike_model::Bar;

#[path = "source_tests.rs"]
#[cfg(test)]
mod source_tests;

/// Why a source returned no rows. `Fetch`: the venue was asked and the answer failed. `Refused`:
/// the request cannot be expressed, the venue was NOT asked, and retrying cannot help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// The venue was asked and failed: network, HTTP status or decode.
    Fetch(String),
    /// The request cannot be expressed by this source; nothing was asked of the venue.
    Refused(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Fetch(e) => write!(f, "venue fetch: {e}"),
            SourceError::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

impl std::error::Error for SourceError {}

/// One venue's OHLCV history. `Send + Sync` because the datahub's registry is a `static` of
/// `&'static dyn KlineSource` and its table moves each borrow into a `Send + Sync` closure.
pub trait KlineSource: Send + Sync {
    /// The store `venue` partition the rows land under — the bridge's own venue id.
    fn venue(&self) -> &str;

    /// Fetch `[start_ms, end_ms]` of `(symbol, interval)` bars. Does the network I/O and the
    /// venue's own paging; touches no store. `symbol` is the unified store symbol: a venue whose
    /// own spelling differs maps it here, and REFUSES ([`SourceError::Refused`]) rather than
    /// guessing when the mapping cannot be expressed.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError>;
}

/// The reserved `interval` LABEL of the market funding-rate series — not a cadence. Funding
/// cadence varies by venue and symbol and lives in the spacing of the stored `Bar::ts`; one
/// stable label keeps the series in its own `(venue, symbol, "funding")` keyspace, where it can
/// never interleave with an OHLCV series. The datahub's `Backfill` verb routes a request carrying
/// this interval to a [`FundingRateSource`].
pub const FUNDING_INTERVAL: &str = "funding";

/// One market funding-rate observation for a perp symbol: the interval timestamp and the rate
/// applied (decimal-string venue field decoded to `f64` — negative when shorts pay longs). `Copy` so
/// the pager can filter/collect points without cloning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FundingRatePoint {
    /// Funding timestamp, ms since epoch (Binance `fundingTime` / Hyperliquid `time`).
    pub ts_ms: i64,
    /// The funding rate for this interval (`fundingRate`, a decimal STRING on the wire, `.parse`d).
    pub rate: f64,
    /// The venue's published funding PREMIUM for this interval, when it publishes one — signed, a
    /// fraction rather than a percentage, and NOT derivable from [`Self::rate`] (a venue folds an
    /// interest-rate term into the rate and then CLAMPS it, so the rate loses information the
    /// premium keeps).
    ///
    /// `None` for a venue whose funding-rate response carries no premium field: Binance's
    /// `/fapi/v1/fundingRate` rows are `{symbol, fundingTime, fundingRate, markPrice}` and have
    /// none, while Hyperliquid's `fundingHistory` rows are `{coin, fundingRate, premium, time}` and
    /// do. It is stored as `kind=perp_metrics` rather than on the funding [`Bar`] — the funding
    /// rate's home — because `Bar` has no field for it and
    /// `vike_data::perp_metrics_log::PerpMetricRow` is where the argument for that split is made.
    pub premium: Option<f64>,
}

/// One venue's market funding-rate history. `Send + Sync` for the same reason as
/// [`KlineSource`].
pub trait FundingRateSource: Send + Sync {
    /// The store `venue` partition — the bridge's own venue id.
    fn venue(&self) -> &str;

    /// Fetch the venue's funding points for `symbol` over `[start_ms, end_ms]`, ts-ascending and
    /// de-duplicated. Does the network I/O and the forward paging; touches no store.
    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, SourceError>;
}

/// Forward-page a funding series over `[start_ms, end_ms]`: call `fetch_page(cursor)` until a page
/// is empty or short (`< cap`) or the cursor passes `end_ms`. The cursor advances one ms past the
/// page's maximum ts, and a non-advancing cursor stops the loop. Points outside the window are
/// dropped; the result is sorted and de-duplicated by ts.
pub fn page_funding_forward(
    start_ms: i64,
    end_ms: i64,
    cap: usize,
    mut fetch_page: impl FnMut(i64) -> Result<Vec<FundingRatePoint>, SourceError>,
) -> Result<Vec<FundingRatePoint>, SourceError> {
    let mut out: Vec<FundingRatePoint> = Vec::new();
    let mut cursor = start_ms;
    while cursor <= end_ms {
        let page = fetch_page(cursor)?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        for p in &page {
            if start_ms <= p.ts_ms && p.ts_ms <= end_ms {
                out.push(*p);
            }
        }
        if page_len < cap {
            break;
        }
        let max_ts = page.iter().map(|p| p.ts_ms).max().unwrap_or(cursor);
        let next = max_ts.saturating_add(1);
        if next <= cursor {
            break;
        }
        cursor = next;
    }
    out.sort_by_key(|p| p.ts_ms);
    out.dedup_by_key(|p| p.ts_ms);
    Ok(out)
}

/// Decode the funding-rate JSON shape both wired venues share: a top-level ARRAY of objects, each
/// with an epoch-ms timestamp under `ts_field`, a decimal-STRING `fundingRate` and an optional
/// decimal-string `premium`. Strict on the envelope (a non-array body is an `Err`), tolerant per
/// row (a row missing either field, or whose rate will not parse, is skipped), and softer still
/// for the premium: absent, unparseable or non-finite is `None` and the row is kept.
pub fn parse_funding_rate_rows(
    body: &str,
    ts_field: &str,
) -> Result<Vec<FundingRatePoint>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v.as_array().ok_or_else(|| "expected a top-level JSON array".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|row| {
            let ts_ms = row.get(ts_field).and_then(serde_json::Value::as_i64)?;
            let rate_str = row.get("fundingRate").and_then(serde_json::Value::as_str)?;
            let rate = rate_str.parse::<f64>().ok()?;
            let premium = row
                .get("premium")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|p| p.is_finite());
            Some(FundingRatePoint { ts_ms, rate, premium })
        })
        .collect())
}
