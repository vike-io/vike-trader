//! Deribit klines backfill: fetch historical OHLCV candles via `vike_deribit::data` and ingest them
//! into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! The crypto twin of [`crate::binance`] — see it for the fetch→ingest contract (the venue fetcher
//! lives in `vike-deribit`, its own bridge crate; the store in vike-data; this crate the seam
//! between). Deribit candles are already OHLCV bars, so — unlike the dukascopy tick collector —
//! there is no resample step, and the fetch→ingest orchestration, commit-key format and CLI body are
//! shared with [`crate::binance`]/[`crate::bybit`]/[`crate::okx`]/[`crate::aster`] in
//! `crate::klines`.
//!
//! Two Deribit-specific facts, both absorbed INSIDE the bridge so this seam stays the plain 4-arg
//! binding [`crate::binance`] is (no closure, unlike [`crate::aster`]'s `Environment`):
//!
//! 1. The venue's candle endpoint (`public/get_tradingview_chart_data`, keyless) answers in COLUMNAR
//!    form and is END-ANCHORED — an over-wide window silently returns only the NEWEST page, with
//!    `status: "ok"` and no error — so `vike_deribit::data::fetch_klines_range` pages BACKWARD
//!    (OKX's pattern, not binance's). None of that reaches this seam: it exposes the same
//!    `(symbol, interval, start_ms, end_ms) -> Vec<Bar>` shape every other venue does.
//! 2. There is no `.P` perp suffix to handle here. A Deribit instrument NAME (`BTC-PERPETUAL`) is
//!    already unambiguous — which is precisely why `vike_catalog::uses_perp_suffix("deribit")` is
//!    `false` — so the caller's symbol passes through VERBATIM: wire symbol and store/series key are
//!    the same string, and this seam never splits anything.

use vike_data::DataFusionHist;
use vike_deribit::data::fetch_klines_range;

use crate::pace_book::PaceSession;

use crate::error::CollectError;

/// Venue tag under which Deribit series live in the hist store (`venue=deribit` in the tree).
pub const VENUE: &str = "deribit";

/// The MARKET half of this venue's persisted pace key — ONE row for the whole venue, whatever the
/// symbol. See [`crate::bybit::kline_market`] for why this is not binance's spot/perp split: every
/// Deribit instrument name — perpetual, dated future, option — pages the SAME keyless
/// `get_tradingview_chart_data` endpoint out of the same non-matching-engine credit pool, and the
/// venue publishes no weight budget at all.
pub fn kline_market(_symbol: &str) -> &'static str {
    "klines"
}

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract). Keys on the CALLER's symbol, which for Deribit is the venue's own
/// instrument name (`BTC-PERPETUAL`) verbatim.
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch Deribit candles for `(symbol, interval)` over `[start_ms, end_ms]` (the bridge pages the
/// endpoint's per-request cap BACKWARD, since it is end-anchored), and `append_bars` them into the
/// store under `(venue=deribit, symbol, interval)`. Idempotent by [`klines_commit_key`]. Returns
/// rows written (0 if the window was already ingested).
///
/// `symbol` is a Deribit instrument name (`BTC-PERPETUAL`) and is passed through untouched — the
/// endpoint accepts dated futures and option names on the same path, with no special handling.
///
/// NOTE: this DOES network I/O (Deribit's public JSON-RPC REST, keyless) — the offline gate
/// exercises the pure columnar parse + ingest path directly against a committed fixture; only the
/// `#[ignore]`d live test drives this end to end.
pub fn backfill_deribit_klines(
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

/// [`backfill_deribit_klines`] with the run's PACE carried across processes: the session's stored
/// observation seeds the pager, and whatever this run measures is folded back into the session.
///
/// ⚠ **On this venue that is a REPORT, not a speed-up** — the opposite of [`crate::binance`], where
/// a seed genuinely re-paces the first page. Deribit publishes no request-weight budget for a pager
/// to discover, so its pager runs `vike_bridge_core::pacer::Pacer::fallback`, whose `next_delay()`
/// is the bridge's hardcoded `PAGE_DELAY` no matter what is seeded or observed. What the record buys
/// is visibility, and this is the venue that needed it most: the bin's own doc warns that a 24-month
/// 1m pull here is "a LOT of requests", and until now that run printed no ETA at all. Now the first
/// page reports one, and the pace file says what a page actually COSTS — a number nobody had
/// measured, against a constant nobody had checked (the sibling okx pages measured 476–486 ms
/// against a 200 ms delay). Changing the delay on the strength of that is a separate change with its
/// own measurement.
///
/// The bars written, the commit key and the returned row count are identical to
/// [`backfill_deribit_klines`]; so, deliberately, is every sleep.
pub fn backfill_deribit_klines_paced(
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
        vike_deribit::data::fetch_klines_range_paced,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ONE pace row per venue, whatever the instrument — perpetuals, dated futures and options all
    /// page the same chart-data endpoint out of the same credit pool.
    #[test]
    fn every_symbol_shares_one_pace_row() {
        assert_eq!(kline_market("BTC-PERPETUAL"), "klines");
        assert_eq!(kline_market("BTC-28MAR25-100000-C"), "klines");
        assert_eq!(kline_market(""), "klines");
    }

    /// The commit key is the venue tag + the CALLER's symbol verbatim. Deribit instrument names
    /// carry no `.P` suffix to strip, so this pins the one thing that could still go wrong: a seam
    /// that rewrote the symbol (case-folded it, split on `-`, appended a suffix) would key a window
    /// on a series the store never wrote, and every re-run would look like fresh data.
    #[test]
    fn commit_key_is_venue_plus_the_verbatim_instrument_name() {
        assert_eq!(
            klines_commit_key("BTC-PERPETUAL", "1m", 10, 20),
            "deribit:BTC-PERPETUAL:1m:10-20"
        );
    }
}
