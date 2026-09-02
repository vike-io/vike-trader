//! Market perpetual-futures **funding-RATE** history backfill — a pluggable multi-venue collector
//! that fetches each venue's public funding-rate schedule and ingests it as `kind=bar` (the rate
//! riding on [`vike_model::Bar::funding`], with OHLCV/volume left `0.0`) into the `vike-data`
//! `HistStore`. This is the data enabler for a funding-CAPTURE backtest: a strategy's
//! `funding_source` seam replays the real per-interval rate off the stored `Bar.funding` column.
//!
//! DISTINCT from the ACCOUNT realized-funding series
//! ([`crate::hyperliquid::backfill_hyperliquid_funding`] → `kind=funding`/`FundingRow`, a wallet's
//! `userFunding` PAYMENTS): THIS is the venue-wide MARKET rate for a symbol — keyless,
//! position-independent, the SIGNAL a funding-capture strategy trades on, available for every perp
//! whether or not you hold it.
//!
//! The extensibility point is the [`FundingRateSource`] trait: adding a venue is one impl — a PURE,
//! fixture-tested parser over the venue's JSON ([`parse_binance`]/[`parse_hyperliquid`], no network,
//! never panics) plus a paged `fetch`. Two venues ship:
//!   * **Binance** — `GET https://fapi.binance.com/fapi/v1/fundingRate?symbol=&startTime=&endTime=
//!     &limit=1000` → `[{symbol, fundingTime(ms num), fundingRate(decimal str), markPrice(str)}]`;
//!     8h cadence; keyless GET over the shared [`crate::http`] ureq client.
//!   * **Hyperliquid** — `POST /info {"type":"fundingHistory","coin":,"startTime":,"endTime":}` →
//!     `[{coin, fundingRate(decimal str), premium(str), time(ms num)}]`; 1h cadence; keyless MAINNET
//!     read over the shared `vike_hyperliquid::HyperliquidTransport` (no new HTTP dep).
//!
//! ⚠ **Hyperliquid's response carries a SECOND number and this collector now keeps it.** `premium`
//! was parsed and discarded for as long as this file existed; it is the venue's funding premium for
//! that same interval, it is not derivable from `fundingRate` (the venue folds an interest-rate term
//! in and then clamps), and it is the input the cohort study's `premium_z_24h` feature needs. It is
//! stored as its OWN kind — `kind=perp_metrics`, one [`PerpMetricRow`] per interval that had one —
//! rather than on the funding [`Bar`], which has no field for it; `vike_data::perp_metrics_log`
//! carries the argument for that split and for why open interest is NOT beside it (Hyperliquid
//! serves open interest only as a current snapshot, never as history, so no backfill can
//! reconstruct a past value). Binance sends no premium and therefore writes none.
//!
//! Both venues cap ~500–1000 rows/response, so `fetch` PAGES `startTime` forward until a page comes
//! back short/empty or the cursor passes `end_ms` ([`page_forward`], shared by both impls — the
//! paging shape is identical, only the request differs). Idempotent per `(venue, symbol,
//! [start,end])` window via [`funding_rate_commit_key`]. No new external dep — reuses the workspace
//! `ureq`/`serde_json` the other collectors do. Shipped as the `funding_rate_backfill` bin.

use vike_data::{DataFusionHist, HistStore, PerpMetricRow};
use vike_hyperliquid::config::Network;
use vike_hyperliquid::transport::HyperliquidTransport;
use vike_model::Bar;

use crate::error::CollectError;

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

/// What one [`backfill_funding_rate`] call wrote, split by SERIES because it writes two.
///
/// A single `usize` was the old shape and cannot say this: the rate bars and the premium rows are
/// separate series with separate commit keys, so "12 rows" would hide which of the two a re-run had
/// already ingested. Reporting them apart is what lets the bin's log tell an operator that a
/// Binance window ingested rates and — correctly — no premium at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FundingRateWritten {
    /// Rows appended to the `kind=bar` funding-rate series (`interval` as resolved by the caller).
    pub rate_rows: usize,
    /// Rows appended to the `kind=perp_metrics` series — always 0 for a venue that publishes no
    /// premium, and 0 for a window already ingested under the premium commit key.
    pub premium_rows: usize,
}

// --- the pluggable source ----------------------------------------------------------------------

/// A pluggable market funding-rate data source. One impl per venue. The `name()` doubles as the
/// store `venue` partition; `default_interval()` is the venue's funding cadence label used as the
/// series `interval` when the bin's `--interval` is omitted.
pub trait FundingRateSource {
    /// Venue name — the store `venue` partition (provenance) and CLI selector (e.g. `"binance"`).
    fn name(&self) -> &str;
    /// The default series `interval` LABEL this source stores under when `--interval` is omitted:
    /// the cadence-agnostic namespace `"funding"`. It is DELIBERATELY NOT a cadence (`"8h"`/`"1h"`):
    /// funding cadence varies by venue AND by symbol — 1h (hyperliquid), 8h (most perps), and 2h/4h
    /// on some venues/symbols, and a venue can even re-cadence a symbol over time — so the true cadence
    /// lives in the DATA (the spacing of the stored `Bar.ts`), never in the label. A single stable
    /// label keeps this market funding-rate series in its OWN `(venue, symbol, "funding")` keyspace —
    /// it can NEVER interleave with an OHLCV kline series (`…/8h`, `…/1h`) — and the `funding_source`
    /// reads one well-known key regardless of the symbol's actual cadence. An operator who wants a
    /// cadence-tagged series (e.g. to hold 4h and 8h histories side by side) passes `--interval`.
    fn default_interval(&self) -> &str;
    /// Fetch funding-rate points for the venue's own `source_symbol` over the inclusive
    /// `[start_ms, end_ms]` window, ts-ascending and de-duplicated. DOES the network I/O and the
    /// forward paging (venues cap ~500–1000 rows/response). The pure per-page decode is the
    /// fixture-tested seam; only this method touches the network.
    fn fetch(
        &self,
        source_symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, CollectError>;
}

/// Resolve a source by venue name. Adding a venue = one arm here + one impl section below.
pub fn source_by_name(name: &str) -> Option<Box<dyn FundingRateSource>> {
    match name {
        "binance" => Some(Box::new(BinanceFunding)),
        "hyperliquid" => Some(Box::new(HyperliquidFunding)),
        _ => None,
    }
}

/// The venue names [`source_by_name`] accepts (for `--help` / error messages).
pub const SOURCES: &[&str] = &["binance", "hyperliquid"];

// --- pure parsers (fixture-tested, no network, never panic) ------------------------------------

/// Shared array decode for both venues' funding-rate JSON: a top-level ARRAY of objects, each
/// carrying an epoch-ms timestamp under `ts_field` and a decimal-STRING rate under `"fundingRate"`.
/// STRICT on the envelope (a non-array / malformed body is an `Err`, never a panic) but per-row
/// TOLERANT (a row missing either field, or whose rate won't `.parse::<f64>()`, is skipped) — the
/// same split the sibling `parse_user_funding` / databento parsers use.
///
/// `"premium"` is read OPTIONALLY under exactly that tolerance and one step softer: an absent or
/// unparseable premium yields `None` and keeps the row, because the rate is what the row is
/// primarily about and a venue that publishes no premium (Binance) must not lose its whole funding
/// history to a field it never sends.
fn parse_rate_array(body: &str, ts_field: &str) -> Result<Vec<FundingRatePoint>, String> {
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

/// Parse a Binance `GET /fapi/v1/fundingRate` body → points. Rows: `{symbol, fundingTime (ms
/// number), fundingRate (decimal string), markPrice (string)}` — timestamp from `fundingTime`.
pub fn parse_binance(body: &str) -> Result<Vec<FundingRatePoint>, String> {
    parse_rate_array(body, "fundingTime")
}

/// Parse a Hyperliquid `POST /info {"type":"fundingHistory",…}` body → points. Rows: `{coin,
/// fundingRate (decimal string), premium (string), time (ms number)}` — timestamp from `time`.
pub fn parse_hyperliquid(body: &str) -> Result<Vec<FundingRatePoint>, String> {
    parse_rate_array(body, "time")
}

// --- shared forward pager ----------------------------------------------------------------------

/// Forward-page a funding-rate series over `[start_ms, end_ms]` by repeatedly calling `fetch_page`
/// with an advancing cursor, until a page comes back empty, short (`< cap`), or the cursor passes
/// `end_ms`. The cursor advances to one ms PAST the maximum ts in each page so the next page begins
/// strictly after it (no boundary re-fetch); a non-advancing cursor breaks the loop (defensive — a
/// well-formed venue always returns points at/after `cursor`). Points outside `[start_ms, end_ms]`
/// are filtered; the result is sorted + deduped by ts (cheap insurance against a boundary dup). The
/// per-page `fetch_page(cursor)` owns the network + venue decode; this owns only the loop, shared by
/// both venue impls.
fn page_forward(
    start_ms: i64,
    end_ms: i64,
    cap: usize,
    mut fetch_page: impl FnMut(i64) -> Result<Vec<FundingRatePoint>, CollectError>,
) -> Result<Vec<FundingRatePoint>, CollectError> {
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
        // A short page means the venue ran out of history within the window — nothing more to fetch.
        if page_len < cap {
            break;
        }
        // Advance PAST the largest ts in the page so the next page starts strictly after it. Using
        // the max (not the last element) makes the advance independent of the venue's page ordering.
        let max_ts = page.iter().map(|p| p.ts_ms).max().unwrap_or(cursor);
        let next = max_ts.saturating_add(1);
        if next <= cursor {
            break; // no forward progress (defensive) — stop rather than loop forever
        }
        cursor = next;
    }
    out.sort_by_key(|p| p.ts_ms);
    out.dedup_by_key(|p| p.ts_ms);
    Ok(out)
}

// --- Binance ------------------------------------------------------------------------------------

/// Binance USDⓈ-M funding-rate history endpoint (keyless public GET).
const BINANCE_FUNDING_URL: &str = "https://fapi.binance.com/fapi/v1/fundingRate";
/// Binance caps `fundingRate` at 1000 rows/response.
const BINANCE_PAGE_CAP: usize = 1000;

/// Binance market funding-rate source (USDⓈ-M perps; 8h cadence). Keyless public GET over the shared
/// [`crate::http`] ureq client, paged by [`page_forward`].
pub struct BinanceFunding;

impl FundingRateSource for BinanceFunding {
    fn name(&self) -> &str {
        "binance"
    }

    fn default_interval(&self) -> &str {
        "funding"
    }

    fn fetch(
        &self,
        source_symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, CollectError> {
        page_forward(start_ms, end_ms, BINANCE_PAGE_CAP, |cursor| {
            let url = format!(
                "{BINANCE_FUNDING_URL}?symbol={source_symbol}&startTime={cursor}\
                 &endTime={end_ms}&limit={BINANCE_PAGE_CAP}"
            );
            let opts = crate::http::GetOptions::default();
            let body = crate::http::get_to_string(&url, &opts, "binance-funding")?;
            parse_binance(&body).map_err(CollectError::Fetch)
        })
    }
}

// --- Hyperliquid --------------------------------------------------------------------------------

/// Hyperliquid caps `fundingHistory` at 500 rows/response.
const HL_PAGE_CAP: usize = 500;

/// Hyperliquid market funding-rate source (perps; 1h cadence). Keyless MAINNET `/info` read over the
/// shared `vike_hyperliquid::HyperliquidTransport` (its own `RateGate` self-throttles), paged by
/// [`page_forward`]. The pure [`parse_hyperliquid`] takes the re-serialized `/info` body — the same
/// `Value.to_string()` seam `vike_hyperliquid::funding::fetch_funding` uses.
pub struct HyperliquidFunding;

impl FundingRateSource for HyperliquidFunding {
    fn name(&self) -> &str {
        "hyperliquid"
    }

    fn default_interval(&self) -> &str {
        "funding"
    }

    fn fetch(
        &self,
        source_symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, CollectError> {
        let transport = HyperliquidTransport::new(Network::Mainnet);
        page_forward(start_ms, end_ms, HL_PAGE_CAP, |cursor| {
            let body = serde_json::json!({
                "type": "fundingHistory",
                "coin": source_symbol,
                "startTime": cursor,
                "endTime": end_ms,
            });
            // `/info` returns a `Value`; re-serialize once for the pure `&str` parser seam (the same
            // `Value.to_string()` seam `vike_hyperliquid::funding::fetch_funding` uses).
            let raw = match transport.info(&body) {
                Ok(v) => v.to_string(),
                Err(e) => return Err(CollectError::Fetch(format!("fundingHistory: {}", e.msg))),
            };
            parse_hyperliquid(&raw).map_err(CollectError::Fetch)
        })
    }
}

// --- map + ingest -------------------------------------------------------------------------------

/// Floor a venue funding timestamp onto its nominal hour.
///
/// ⚠ **This is what makes the stored series MATCHABLE, and it was measured missing.** Hyperliquid's
/// `fundingHistory` stamps carry millisecond jitter off the settlement grid (`1775538000006`,
/// `…030` — observed live), while `kind=bar` price rows sit exactly on the hour. The
/// `PerpMetricRow` doc promises the two series *"align by timestamp without interpolating"*, and a
/// raw jittered stamp breaks that promise silently: an exact-`ts` join finds nothing, and every
/// consumer's premium/funding column is quietly all-NaN — which is precisely how the ported cohort
/// study ran with `funding_z_24h` and `premium_z_24h` dead while its store looked fully served.
/// Binance's 8h stamps are exact, so this is a no-op there.
fn floor_to_hour(ts_ms: i64) -> i64 {
    ts_ms - ts_ms.rem_euclid(3_600_000)
}

/// Map one funding-rate point → a `vike_model::Bar` carrying the rate on `funding` (OHLCV/volume
/// `0.0`; `bid`/`ask`/`symbol` `None`). This is the storage shape the backtest `funding_source` seam
/// reads back off `Bar.funding`. The stamp is floored onto its nominal hour — [`floor_to_hour`]
/// says why that is the contract rather than a convenience.
pub fn point_to_bar(p: &FundingRatePoint) -> Bar {
    Bar {
        ts: floor_to_hour(p.ts_ms),
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: 0.0,
        volume: 0.0,
        funding: Some(p.rate),
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// Map one funding-rate point → a `vike_data::PerpMetricRow`, or `None` when the venue published no
/// premium for that interval. This is the storage shape `StudyContext::perp_metrics` reads back.
///
/// Returning `None` rather than a row with a zero is the whole point: zero is a REAL premium (a
/// perp trading exactly at its oracle), so a filled-in zero would be indistinguishable from an
/// observation and would drag any z-score computed over the series toward it.
pub fn point_to_perp_metric(p: &FundingRatePoint) -> Option<PerpMetricRow> {
    p.premium.map(|premium| PerpMetricRow {
        ts: floor_to_hour(p.ts_ms),
        premium,
        open_interest: None,
    })
}

/// The idempotency guard for a `(venue, symbol, [start_ms, end_ms])` funding-rate backfill window: a
/// re-run with the same window is a no-op in the store (batch-level dedup — never per-row value
/// dedup, per the store contract). The `funding_rate:` prefix keeps this key-space distinct from the
/// crypto klines' `{venue}:{symbol}:{interval}:…` window keys.
pub fn funding_rate_commit_key(venue: &str, symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("funding_rate:{venue}:{symbol}:{start_ms}-{end_ms}")
}

/// The same guard for the `kind=perp_metrics` half of the same window.
///
/// ⚠ A SEPARATE key from [`funding_rate_commit_key`], deliberately, even though one fetch fills
/// both series. Commit keys are batch-level and global, so sharing one would make the second append
/// of a window a no-op against the first: a store whose funding bars were ingested before this
/// series existed would then refuse the premium rows for every window it already holds, silently,
/// and the only repair would be re-running under a window that had never been fetched.
pub fn perp_metrics_commit_key(venue: &str, symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("perp_metrics:{venue}:{symbol}:{start_ms}-{end_ms}")
}

/// Fetch `source`'s market funding-rate history for the venue `source_symbol` over
/// `[start_ms, end_ms]` ONCE, and fan the one response into the TWO series it fills:
/// each point → a funding-bearing [`Bar`] appended under `(venue = source.name(), symbol,
/// interval)` (`kind=bar`), and each point that carried a premium → a [`PerpMetricRow`] appended
/// under `(venue, symbol)` (`kind=perp_metrics`). Idempotent per window per series via
/// [`funding_rate_commit_key`] and [`perp_metrics_commit_key`].
///
/// Returns [`FundingRateWritten`] — `0`s where the window was already ingested OR the fetch returned
/// nothing (an empty fetch is NOT committed, so a later re-run still retries it), and
/// `premium_rows: 0` for a venue that publishes no premium at all.
///
/// ⚠ **The premium append is BEST-EFFORT and never fails the call.** The funding-rate bars are what
/// this collector exists for and what a funding-capture backtest replays; a store that rejects the
/// newer kind (an older build's tree, a read-only mount) must not cost an operator the rate history
/// they asked for. A failure is logged and the rate result stands — the opposite ordering, where a
/// premium error discards a successful bar append, would make the cheap half hostage to the
/// additional one.
///
/// `symbol` is the unified vike store key; `source_symbol` is the venue's own market symbol the fetch
/// pages (the bin's `--symbols vikeSym=sourceSym` mapping; they coincide for e.g. `BTC`).
pub fn backfill_funding_rate(
    hist: &DataFusionHist,
    source: &dyn FundingRateSource,
    symbol: &str,
    source_symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<FundingRateWritten, CollectError> {
    let venue = source.name();
    let points = source.fetch(source_symbol, start_ms, end_ms)?;
    let mut bars: Vec<Bar> = points.iter().map(point_to_bar).collect();
    // Two jittered stamps inside one nominal hour would floor onto one row. Every wired venue
    // serves one point per interval, so this is a guard rather than an expectation — but the
    // upstream dedup keys on the RAW stamp and cannot see a post-floor collision.
    bars.dedup_by_key(|b| b.ts);
    if bars.is_empty() {
        // Never poison either commit key on an empty/failed fetch — let a re-run retry.
        return Ok(FundingRateWritten::default());
    }
    let key = funding_rate_commit_key(venue, symbol, start_ms, end_ms);
    let rate_rows = hist.append_bars(venue, symbol, interval, &bars, Some(&key))?;

    let mut metrics: Vec<PerpMetricRow> = points.iter().filter_map(point_to_perp_metric).collect();
    metrics.dedup_by_key(|m| m.ts);
    let premium_rows = if metrics.is_empty() {
        0
    } else {
        let pkey = perp_metrics_commit_key(venue, symbol, start_ms, end_ms);
        match hist.append_perp_metrics(venue, symbol, &metrics, Some(&pkey)) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    "{venue}/{symbol}: funding rates stored, but the {} premium row(s) could not \
                     be: {e} — re-run this window to retry the perp_metrics half",
                    metrics.len(),
                );
                0
            }
        }
    };
    Ok(FundingRateWritten { rate_rows, premium_rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Binance parser ---

    /// The real `/fapi/v1/fundingRate` shape: `fundingTime` is a NUMBER; `fundingRate`/`markPrice`
    /// are STRINGS. Both the string→f64 decode and a negative rate must survive.
    #[test]
    fn parse_binance_decodes_ts_and_string_rate() {
        let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1700000000000,"fundingRate":"0.00010000","markPrice":"37000.0"},
            {"symbol":"BTCUSDT","fundingTime":1700028800000,"fundingRate":"-0.00005500","markPrice":"37100.0"}
        ]"#;
        let pts = parse_binance(body).unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].ts_ms, 1_700_000_000_000);
        assert!((pts[0].rate - 0.0001).abs() < 1e-12, "decimal string → f64");
        assert_eq!(pts[1].ts_ms, 1_700_028_800_000);
        assert!((pts[1].rate + 0.000_055).abs() < 1e-12, "negative rate survives");
    }

    #[test]
    fn parse_binance_empty_array_is_ok_empty() {
        assert_eq!(parse_binance("[]").unwrap(), Vec::new());
    }

    /// Per-row tolerance: a row missing `fundingRate`, and a row whose rate won't parse, are both
    /// skipped; the one well-formed row survives.
    #[test]
    fn parse_binance_skips_bad_rows_keeps_good() {
        let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1},
            {"symbol":"BTCUSDT","fundingTime":2,"fundingRate":"not-a-number"},
            {"symbol":"BTCUSDT","fundingTime":3,"fundingRate":"0.0002"}
        ]"#;
        let pts = parse_binance(body).unwrap();
        assert_eq!(pts.len(), 1, "only the well-formed row survives");
        assert_eq!(pts[0].ts_ms, 3);
        assert!((pts[0].rate - 0.0002).abs() < 1e-12);
    }

    #[test]
    fn parse_binance_malformed_or_non_array_is_error() {
        assert!(parse_binance("not json").is_err());
        assert!(parse_binance("{}").is_err(), "an object is not the expected array");
        assert!(parse_binance("null").is_err());
    }

    // --- Hyperliquid parser ---

    /// The real `fundingHistory` shape: `time` is a NUMBER; `fundingRate`/`premium` are STRINGS, and
    /// the timestamp is keyed on `time` (not `fundingTime`).
    #[test]
    fn parse_hyperliquid_decodes_ts_and_string_rate() {
        let body = r#"[
            {"coin":"BTC","fundingRate":"0.0000125","premium":"0.0001","time":1700000000000},
            {"coin":"BTC","fundingRate":"-0.0000088","premium":"-0.0002","time":1700003600000}
        ]"#;
        let pts = parse_hyperliquid(body).unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].ts_ms, 1_700_000_000_000);
        assert!((pts[0].rate - 0.000_012_5).abs() < 1e-15);
        assert_eq!(pts[1].ts_ms, 1_700_003_600_000);
        assert!((pts[1].rate + 0.000_008_8).abs() < 1e-15, "negative rate survives");
    }

    #[test]
    fn parse_hyperliquid_empty_array_is_ok_empty() {
        assert_eq!(parse_hyperliquid("[]").unwrap(), Vec::new());
    }

    #[test]
    fn parse_hyperliquid_malformed_or_non_array_is_error() {
        assert!(parse_hyperliquid("not json").is_err());
        assert!(parse_hyperliquid("{}").is_err());
        assert!(parse_hyperliquid("42").is_err());
    }

    // --- map + key + registry ---

    /// The rate rides on `Bar.funding`; OHLCV/volume are zeroed and bid/ask/symbol absent.
    #[test]
    fn point_to_bar_rides_rate_on_funding_with_zeroed_ohlcv() {
        // On-the-hour stamp: unchanged by the floor.
        let b = point_to_bar(&FundingRatePoint {
            ts_ms: 1_699_999_200_000,
            rate: 0.0001,
            premium: Some(0.0002),
        });
        assert_eq!(b.ts, 1_699_999_200_000);
        // Exact-float checks compared bitwise (codebase idiom) to stay clear of float-eq lints.
        assert_eq!(b.funding.map(f64::to_bits), Some(0.0001_f64.to_bits()));
        for x in [b.open, b.high, b.low, b.close, b.volume] {
            assert_eq!(x.to_bits(), 0.0_f64.to_bits(), "OHLCV/volume are zeroed");
        }
        assert_eq!(b.bid, None);
        assert_eq!(b.ask, None);
        assert_eq!(b.symbol, None);
    }

    #[test]
    fn commit_key_has_venue_symbol_and_window() {
        assert_eq!(
            funding_rate_commit_key("binance", "BTCUSDT", 1000, 2000),
            "funding_rate:binance:BTCUSDT:1000-2000"
        );
    }

    /// The two halves of one fetch take DIFFERENT keys. Sharing one would make the second append of
    /// a window a silent no-op against the first — the hazard `perp_metrics_commit_key` documents.
    #[test]
    fn the_premium_commit_key_is_distinct_from_the_rate_one() {
        let rate = funding_rate_commit_key("hyperliquid", "BTC", 1000, 2000);
        let prem = perp_metrics_commit_key("hyperliquid", "BTC", 1000, 2000);
        assert_eq!(prem, "perp_metrics:hyperliquid:BTC:1000-2000");
        assert_ne!(rate, prem, "one window, two series, two keys");
    }

    // --- premium: the value that used to be parsed and dropped ---

    /// Hyperliquid ships `premium` beside `fundingRate` in the SAME row, and both are decimal
    /// strings. This is the regression the whole change exists to prevent: the field was on the
    /// wire and discarded.
    #[test]
    fn parse_hyperliquid_keeps_the_premium_beside_the_rate() {
        let body = r#"[
            {"coin":"BTC","fundingRate":"0.0000125","premium":"0.0003354037","time":1700000000000},
            {"coin":"BTC","fundingRate":"-0.0000088","premium":"-0.0006236109","time":1700003600000}
        ]"#;
        let pts = parse_hyperliquid(body).unwrap();
        assert_eq!(pts.len(), 2);
        assert!((pts[0].premium.unwrap() - 0.000_335_403_7).abs() < 1e-15);
        assert!(
            (pts[1].premium.unwrap() + 0.000_623_610_9).abs() < 1e-15,
            "a negative premium survives — the perp traded BELOW its oracle"
        );
    }

    /// Binance's `/fapi/v1/fundingRate` carries no premium at all. The row must still parse and
    /// keep its rate: a venue that sends no premium must not lose its funding history to one.
    #[test]
    fn parse_binance_has_no_premium_but_keeps_the_rate() {
        let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1700000000000,"fundingRate":"0.0001","markPrice":"37000.0"}
        ]"#;
        let pts = parse_binance(body).unwrap();
        assert_eq!(pts.len(), 1, "the row survives");
        assert_eq!(pts[0].premium, None, "no premium field on this venue");
        assert!((pts[0].rate - 0.0001).abs() < 1e-12);
    }

    /// A present-but-unparseable premium degrades to `None` WITHOUT dropping the row — one step
    /// softer than the rate's own tolerance, which skips the row entirely.
    #[test]
    fn an_unparseable_or_nonfinite_premium_degrades_to_none_and_keeps_the_row() {
        for bad in ["\"not-a-number\"", "\"NaN\"", "\"inf\"", "0.0003", "null"] {
            let body = format!(
                r#"[{{"coin":"BTC","fundingRate":"0.0000125","premium":{bad},"time":1700000000000}}]"#
            );
            let pts = parse_hyperliquid(&body).unwrap();
            assert_eq!(pts.len(), 1, "the rate row survives a bad premium ({bad})");
            assert_eq!(pts[0].premium, None, "premium {bad} must not become a number");
        }
    }

    /// A point with a premium becomes a row; one without becomes nothing. ⚠ A ZERO premium is a
    /// REAL observation (the perp at its oracle) and must be kept — inventing a zero for an absent
    /// premium is precisely what `point_to_perp_metric` refuses.
    #[test]
    fn point_to_perp_metric_keeps_a_zero_and_drops_an_absent_premium() {
        let with = point_to_perp_metric(&FundingRatePoint {
            ts_ms: 1_700_000_000_000,
            rate: 0.0001,
            premium: Some(0.0),
        })
        .expect("a zero premium is an observation, not an absence");
        assert_eq!(with.ts, 1_699_999_200_000, "the jittered stamp floors onto its hour");
        assert_eq!(with.premium.to_bits(), 0.0_f64.to_bits());

        assert_eq!(
            point_to_perp_metric(&FundingRatePoint {
                ts_ms: 1_700_000_000_000,
                rate: 0.0001,
                premium: None,
            }),
            None,
            "no premium reported → no row, never a zero-filled one"
        );
    }

    /// ⚠ The stamp jitter that made every stored hour UNMATCHABLE, pinned with the observed
    /// values: Hyperliquid stamps funding at `…000006`/`…000030` — milliseconds past the hour —
    /// while price bars sit exactly on it, so a raw stamp broke the row doc's "align by
    /// timestamp" promise for every consumer doing an exact-ts join (measured: the ported cohort
    /// study ran with `funding_z_24h`/`premium_z_24h` all-NaN and nothing said so).
    #[test]
    fn a_jittered_stamp_floors_onto_its_nominal_hour_in_both_series() {
        let p = FundingRatePoint {
            ts_ms: 1_775_538_000_006,
            rate: 6.365e-7,
            premium: Some(-4.949083e-4),
        };
        assert_eq!(point_to_bar(&p).ts, 1_775_538_000_000);
        assert_eq!(point_to_perp_metric(&p).unwrap().ts, 1_775_538_000_000);
    }

    /// The premium does NOT ride the funding bar: `Bar` has no field for it, and the funding rate
    /// keeps its one home. This pins the split the module doc argues for.
    #[test]
    fn the_funding_bar_carries_the_rate_and_never_the_premium() {
        let b = point_to_bar(&FundingRatePoint {
            ts_ms: 1_700_000_000_000,
            rate: 0.0001,
            premium: Some(0.42),
        });
        assert_eq!(b.funding.map(f64::to_bits), Some(0.0001_f64.to_bits()), "the RATE, not 0.42");
    }

    #[test]
    fn source_registry_names_and_intervals() {
        assert_eq!(SOURCES, &["binance", "hyperliquid"]);
        assert!(source_by_name("nope").is_none());
        let b = source_by_name("binance").unwrap();
        assert_eq!(b.name(), "binance");
        assert_eq!(b.default_interval(), "funding", "cadence-agnostic funding namespace");
        let h = source_by_name("hyperliquid").unwrap();
        assert_eq!(h.name(), "hyperliquid");
        assert_eq!(h.default_interval(), "funding", "cadence-agnostic funding namespace");
    }
}
