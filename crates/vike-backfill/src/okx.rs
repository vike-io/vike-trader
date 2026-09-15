//! OKX V5 klines backfill: fetch historical OHLCV candles via `vike_okx::data` and ingest
//! them into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! The crypto twin of [`crate::binance`] — see it for the fetch→ingest contract (the venue fetcher
//! lives in `vike-okx` — its own bridge crate, crate-reorg Phase 3 PR G — the store in vike-data,
//! this crate the seam between). The fetch half is `vike_okx::data::fetch_klines_range` (paged
//! BACKWARD through the `history-candles` 100/req cap via the `after` cursor, rate-limited, and —
//! because OKX serves candles newest-first — reversed to ascending); the ingest half is the
//! `HistStore` seam. OKX candles are already OHLCV bars, so there is no resample step.
//!
//! The fetch→ingest orchestration and commit-key format are shared with [`crate::binance`]/
//! [`crate::bybit`] in `crate::klines` — this module is just the venue binding.

use vike_data::DataFusionHist;
use vike_okx::data::fetch_klines_range;

use crate::pace_book::PaceSession;

use crate::error::CollectError;

/// Venue tag under which OKX series live in the hist store (`venue=okx` in the tree).
pub const VENUE: &str = "okx";

/// The MARKET half of this venue's persisted pace key — ONE row for the whole venue, whatever the
/// symbol. See [`crate::bybit::kline_market`] for why this is not binance's spot/perp split: OKX
/// serves every `instId` from ONE `history-candles` endpoint under one IP limit, and publishes no
/// weight budget at all.
pub fn kline_market(_symbol: &str) -> &'static str {
    "klines"
}

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract). Keys on the ORIGINAL interval string (e.g. "1m"), not OKX's "1m"/"1H" bar.
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch OKX candles for `(symbol, interval)` over `[start_ms, end_ms]` (paging the 100/req
/// `history-candles` cap), and `append_bars` them into the store under `(venue=okx, symbol,
/// interval)`. Idempotent by [`klines_commit_key`]. Returns rows written (0 if the window was
/// already ingested).
///
/// NOTE: this DOES network I/O (OKX's public REST) — the offline gate exercises the pure map +
/// ingest path directly against a committed fixture; only the `#[ignore]`d live test drives this
/// end to end.
pub fn backfill_okx_klines(
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

/// [`backfill_okx_klines`] with the run's PACE carried across processes: the session's stored
/// observation seeds the pager, and whatever this run measures is folded back into the session.
///
/// ⚠ **On this venue that is a REPORT, not a speed-up** — the opposite of [`crate::binance`], where
/// a seed genuinely re-paces the first page. OKX publishes no request-weight budget, so its pager
/// runs `vike_bridge_core::pacer::Pacer::fallback`, whose `next_delay()` is the bridge's hardcoded
/// `PAGE_DELAY` no matter what is seeded or observed. What the record buys is visibility: the
/// backfill's ETA line is answerable from page ZERO instead of page one, and the pace file finally
/// says what a page on this venue actually COSTS. That number is why this exists — MEASURED on the CI box
/// 2026-08-04, okx spot and perp pages average **476–486 ms** against a hardcoded 200 ms delay, so
/// the constant is not even the dominant term of the gap the venue sees, and nobody had checked,
/// because nothing on this path was timed. Changing the delay on the strength of that is a separate
/// change with its own measurement.
///
/// The bars written, the commit key and the returned row count are identical to
/// [`backfill_okx_klines`]; so, deliberately, is every sleep.
pub fn backfill_okx_klines_paced(
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
        vike_okx::data::fetch_klines_range_paced,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE pace row per venue, whatever the symbol — spot, swap and dated futures all page the same
    /// `history-candles` endpoint under the same IP limit.
    #[test]
    fn every_symbol_shares_one_pace_row() {
        assert_eq!(kline_market("BTC-USDT"), "klines");
        assert_eq!(kline_market("BTC-USDT-SWAP"), "klines");
        assert_eq!(kline_market(""), "klines");
    }
}
