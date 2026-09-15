//! Bybit V5 spot klines backfill: fetch historical OHLCV klines via `vike_bybit::data` and
//! ingest them into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! The crypto twin of [`crate::binance`] — see it for the fetch→ingest contract (the venue fetcher
//! lives in `vike-bybit` — its own bridge crate, crate-reorg Phase 3 PR E — the store in vike-data,
//! this crate the seam between). The fetch half is `vike_bybit::data::fetch_klines_range` (paged,
//! rate-limited, and — because Bybit serves klines newest-first — already reversed to ascending);
//! the ingest half is the `HistStore` seam. Bybit klines are already OHLCV bars, so there is no
//! resample step.
//!
//! ⚠ Like Deribit's (see [`crate::deribit`]), Bybit's kline endpoint is **end-anchored**: it ignores
//! `start` once the window exceeds its 1000-row cap and answers the newest 1000 rows with
//! `retCode: 0` — a truncation that looks like success. `vike_bybit::data::fetch_klines_range` pages
//! BACKWARD to cover the whole window; that module's doc is the authority. A backfill window wider
//! than 1000 intervals is normal here and is NOT silently clipped.
//!
//! The fetch→ingest orchestration and commit-key format are shared with [`crate::binance`]/
//! [`crate::okx`] in `crate::klines` — this module is just the venue binding.

use vike_bybit::data::fetch_klines_range;
use vike_data::DataFusionHist;

use crate::pace_book::PaceSession;

use crate::error::CollectError;

/// Venue tag under which Bybit series live in the hist store (`venue=bybit` in the tree).
pub const VENUE: &str = "bybit";

/// The MARKET half of this venue's persisted pace key — ONE row for the whole venue, whatever the
/// symbol.
///
/// Binance needs `spot`/`perp` because its two HOSTS publish different budgets and price `/klines`
/// differently, so one shared row would pace the other host wrong. Bybit has neither problem: V5
/// serves spot and linear from ONE host (`category` is a query param, not a second base) under one
/// IP limit, and it publishes no weight budget at all. Splitting the row would only halve the
/// evidence behind a number that describes the same endpoint either way.
///
/// Takes the symbol anyway so the CLI's `market_of: fn(&str) -> &'static str` binds directly, and so
/// a venue that later grows a second host has one obvious place to say so.
pub fn kline_market(_symbol: &str) -> &'static str {
    "klines"
}

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract). Keys on the ORIGINAL interval string (e.g. "1m"), not Bybit's "1" code.
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch Bybit klines for `(symbol, interval)` over `[start_ms, end_ms]` (paging the 1000/req cap),
/// and `append_bars` them into the store under `(venue=bybit, symbol, interval)`. Idempotent by
/// [`klines_commit_key`]. Returns rows written (0 if the window was already ingested).
///
/// NOTE: this DOES network I/O (Bybit's public REST) — the offline gate exercises the pure map +
/// ingest path directly against a committed fixture; only the `#[ignore]`d live test drives this
/// end to end.
pub fn backfill_bybit_klines(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    crate::klines::backfill_klines(
        hist,
        VENUE,
        symbol,
        interval,
        start_ms,
        end_ms,
        fetch_klines_range,
    )
}

/// [`backfill_bybit_klines`] with the run's PACE carried across processes: the session's stored
/// observation seeds the pager, and whatever this run measures is folded back into the session.
///
/// ⚠ **On this venue that is a REPORT, not a speed-up** — the opposite of [`crate::binance`], where
/// a seed genuinely re-paces the first page. Bybit publishes no request-weight budget, so its pager
/// runs `vike_bridge_core::pacer::Pacer::fallback`, whose `next_delay()` is the bridge's hardcoded
/// `PAGE_DELAY` no matter what is seeded or observed. What the record buys is visibility: the
/// backfill's ETA line is answerable from page ZERO instead of page one, and the pace file finally
/// says what a page on this venue actually COSTS — a number nobody had ever measured, against a
/// constant nobody had ever checked (the sibling okx pages measured 476–486 ms against a 200 ms
/// delay). Changing the delay on the strength of that is a separate change with its own measurement.
///
/// The bars written, the commit key and the returned row count are identical to
/// [`backfill_bybit_klines`]; so, deliberately, is every sleep.
pub fn backfill_bybit_klines_paced(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    pace: &mut PaceSession,
) -> Result<usize, CollectError> {
    crate::klines::backfill_klines_paced(
        hist,
        VENUE,
        symbol,
        interval,
        start_ms,
        end_ms,
        pace,
        vike_bybit::data::fetch_klines_range_paced,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE pace row per venue, whatever the symbol — and specifically NOT binance's spot/perp split,
    /// which exists only because that venue's two hosts publish different budgets.
    #[test]
    fn every_symbol_shares_one_pace_row() {
        assert_eq!(kline_market("BTCUSDT"), "klines");
        assert_eq!(kline_market("BTCUSDT.P"), "klines");
        assert_eq!(kline_market(""), "klines");
    }
}
