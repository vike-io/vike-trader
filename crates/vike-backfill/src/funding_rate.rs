//! Market perpetual-futures **funding-RATE** history backfill — a pluggable multi-venue collector
//! that fetches each venue's public funding-rate schedule and ingests it as `kind=bar` (the rate
//! riding on [`vike_marketdata::Bar::funding`], with OHLCV/volume left `0.0`) into the `vike-data`
//! `HistoryStore`. This is the data enabler for a funding-CAPTURE backtest: a strategy's
//! `funding_source` seam replays the real per-interval rate off the stored `Bar.funding` column.
//!
//! DISTINCT from the ACCOUNT realized-funding series (`kind=exec_funding`/`ExecutionHistoryFundingRow`, a wallet's
//! `userFunding` PAYMENTS — presently produced by nothing in this crate; docs/decisions/0094): THIS
//! is the venue-wide MARKET rate for a symbol — keyless,
//! position-independent, the SIGNAL a funding-capture strategy trades on, available for every perp
//! whether or not you hold it.
//!
//! The extensibility point is the [`FundingRateSource`] trait, implemented by the venue BRIDGE
//! crates rather than here (`docs/decisions/0094-backfill-names-no-venue.md`) — adding a venue is
//! one bridge-side impl: a PURE, fixture-tested parser over the venue's JSON, no network, never
//! panics, plus a paged `fetch`. Six venues ship, all keyless public reads (aster's is binance's wire
//! and parser on aster's public futures host, `crates/bridges/aster/src/history.rs`'s `FundingRateSourceAster`):
//! `crates/bridges/binance/src/history.rs`'s `FundingRateSourceBinance` (USDⓈ-M and COIN-M perps; 8h, or the
//! symbol's own), `crates/bridges/hyperliquid/src/history/funding.rs`'s `FundingRateSourceHyperliquid` (perps; 1h;
//! MAINNET `/info`), `crates/bridges/bybit/src/history.rs`'s `FundingRateSourceBybit` (linear and inverse perps; 8h,
//! 4h or 1h by symbol), `crates/bridges/okx/src/history.rs`'s `FundingRateSourceOkx` (swaps; the venue keeps about
//! three months) and `crates/bridges/deribit/src/history.rs`'s `FundingRateSourceDeribit` (perpetuals; accrued
//! continuously, published per hour).
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
//! Every venue caps its rows per response, so each bridge's `fetch` PAGES: binance, aster and
//! hyperliquid forward from `startTime` through the shared `vike_data::source::page_walk_forward`, and bybit,
//! okx and deribit, whose endpoints are END-ANCHORED (a window wider than one page answers its newest
//! page), backward through `vike_data::source::page_walk_backward` — one function per paging shape,
//! only the request differs. Idempotent per `(venue, symbol, [start,end])` window via
//! [`funding_rate_commit_key`].
//!
//! ⚠ **This crate no longer decides WHICH source serves a venue.** The roster is the gatewayhub's
//! `FUNDING_SOURCES` (`crates/vike-gatewayhub/src/rest/kline_sources.rs`), whose order the datahub's
//! `Backfill` verb folds into its funding lane — a request whose interval is `vike_data::source::FUNDING_INTERVAL`, e.g.
//! `vike-cli data history fetch hyperliquid:BTC:funding`. It replaced this module's
//! `source_by_name`/`SOURCES` pair and the `funding_rate_backfill` program that read them
//! (docs/decisions/0094); [`backfill_funding_rate`] is handed its source and names no bridge.

use vike_data::source::{FUNDING_INTERVAL, FundingRateSource};
use vike_data::{HistoryStore, PerpMetricRow};
use vike_marketdata::Bar;
use vike_marketdata::FundingRatePoint;
use vike_store::HistoryStoreDataFusion;

use crate::error::CollectError;

/// What one [`backfill_funding_rate`] call wrote, split by SERIES because it writes two.
///
/// A single `usize` was the old shape and cannot say this: the rate bars and the premium rows are
/// separate series with separate commit keys, so "12 rows" would hide which of the two a re-run had
/// already ingested. Reporting them apart is what lets a caller tell an operator that a Binance
/// window ingested rates and — correctly — no premium at all. (The datahub's funding lane reports
/// [`Self::rate_rows`], the series its `BackfillDone` reads back.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FundingRateWritten {
    /// Rows appended to the `kind=bar` funding-rate series (`interval=funding`).
    pub rate_rows: usize,
    /// Rows appended to the `kind=perp_metrics` series — always 0 for a venue that publishes no
    /// premium, and 0 for a window already ingested under the premium commit key.
    pub premium_rows: usize,
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

/// Map one funding-rate point → a `vike_marketdata::Bar` carrying the rate on `funding` and the
/// venue's published price, when it publishes one ([`FundingRatePoint::mark`]), on `close` (open,
/// high, low and volume `0.0`, and `close` `0.0` for no price; `bid`/`ask`/`symbol` `None`). This is
/// the storage shape the backtest's funding join reads back (`crates/vike-backtest/src/run/dispatch.rs`'s
/// `attach_funding_series`). The stamp is floored onto its nominal hour — [`floor_to_hour`] says why
/// that is the contract rather than a convenience.
pub fn point_to_bar(p: &FundingRatePoint) -> Bar {
    Bar {
        timestamp: floor_to_hour(p.timestamp_ms),
        open: 0.0,
        high: 0.0,
        low: 0.0,
        close: p.mark.unwrap_or(0.0),
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
        ts: floor_to_hour(p.timestamp_ms),
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

/// Fetch `source`'s market funding-rate history for `symbol` over `[start_ms, end_ms]` ONCE, and
/// fan the one response into the TWO series it fills: each point → a funding-bearing [`Bar`]
/// appended under `(venue = source.venue(), symbol, interval = FUNDING_INTERVAL)` (`kind=bar`),
/// and each point that carried a premium → a [`PerpMetricRow`] appended under `(venue, symbol)`
/// (`kind=perp_metrics`). Idempotent per window per series via [`funding_rate_commit_key`] and
/// [`perp_metrics_commit_key`], and ONE RATE BAR PER SETTLEMENT HOUR across windows: a settlement the
/// funding series already holds (an overlapping window, a settlement the datahub recorded live) is not
/// appended again, so overlapping and recorded settlements never double up.
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
/// `symbol` is the STORE key both series land under, and it is handed to `source` verbatim — any
/// mapping onto the venue's wire spelling is the source's job, never this function's. Hyperliquid
/// sends it unchanged (`BTC`). Binance does not: the store spells a Binance perpetual with the
/// perpetual marker (`BTCUSDT.P` — the key its kline lane files perp bars under, a bare `BTCUSDT`
/// being SPOT), so `vike_binance::history::FundingRateSourceBinance` strips the suffix for the request and
/// REFUSES a bare spot symbol before asking anything, because spot has no funding. The
/// `vikeSym=sourceSym` pair the deleted `funding_rate_backfill` program accepted went with that
/// program: the datahub's `Backfill` verb carries ONE symbol, so a store key its source cannot map
/// onto the venue's spelling is not expressible through this collector.
pub fn backfill_funding_rate(
    hist: &HistoryStoreDataFusion,
    source: &dyn FundingRateSource,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<FundingRateWritten, CollectError> {
    let venue = source.venue();
    let points = source.fetch(symbol, start_ms, end_ms)?;
    let mut bars: Vec<Bar> = points.iter().map(point_to_bar).collect();
    // Two jittered stamps inside one nominal hour would floor onto one row. Every wired venue
    // serves one point per interval, so this is a guard rather than an expectation — but the
    // upstream dedup keys on the RAW stamp and cannot see a post-floor collision.
    bars.dedup_by_key(|b| b.timestamp);
    if bars.is_empty() {
        // Never poison either commit key on an empty/failed fetch — let a re-run retry.
        return Ok(FundingRateWritten::default());
    }
    // ONE POINT PER SETTLEMENT HOUR: a settlement the series already holds (an overlapping window, or
    // one the datahub recorded from the live lane, `vike_datahub`'s `funding_record`) is not written
    // again. The store dedups by commit key and never by row, so without this the two would double up.
    let (first, last) = (bars[0].timestamp, bars[bars.len() - 1].timestamp);
    let held: std::collections::HashSet<i64> = hist
        .load_bars(
            venue,
            symbol,
            FUNDING_INTERVAL,
            vike_data::TimestampRange { start: Some(first), end: Some(last + 3_599_999) },
        )?
        .iter()
        .map(|b| floor_to_hour(b.timestamp))
        .collect();
    bars.retain(|b| !held.contains(&b.timestamp));
    let key = funding_rate_commit_key(venue, symbol, start_ms, end_ms);
    let rate_rows = if bars.is_empty() {
        0
    } else {
        hist.append_bars(venue, symbol, FUNDING_INTERVAL, &bars, Some(&key))?
    };

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

#[path = "funding_rate_tests.rs"]
#[cfg(test)]
mod funding_rate_tests;
