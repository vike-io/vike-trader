//! Binance Spot klines backfill: fetch historical OHLCV klines via `vike_binance::data` and
//! ingest them into the DataFusion hist store (`vike_data::DataFusionHist`).
//!
//! Ported source: none — this is the crypto twin of [`crate::dukascopy`]. The fetch half is
//! `vike_binance::data::fetch_klines_range` (the SAME kline REST fetcher + JSON→bar map the
//! live market feed's warmup seed uses — binance moved into its own bridge crate, crate-reorg
//! Phase 3 PR H — extracted so there is one implementation); the ingest half is the
//! `vike_data::HistStore` seam. This module is the seam between them — no venue code in
//! vike-data, no store code in vike-binance.
//!
//! Flow: `fetch_klines_range` (paged, 1000/req) → `append_bars` (idempotent by commit key). Binance
//! klines are already OHLCV bars, so — unlike the dukascopy tick collector — there is no resample step.
//!
//! The fetch→ingest orchestration and commit-key format are shared with [`crate::bybit`]/
//! [`crate::okx`] in `crate::klines` — this module is just the venue binding.

use vike_binance::data::fetch_klines_range;
use vike_data::DataFusionHist;

use crate::pace_book::PaceSession;

use crate::error::CollectError;

/// Which MARKET a caller's symbol routes to (`"spot"` / `"perp"`) — the second half of a persisted
/// pace record's key. Re-exported from the bridge rather than re-derived from a `.P` check here,
/// because it is the SAME routing decision `fetch_klines_range` makes and the two must not drift.
pub use vike_binance::data::kline_market;

/// Venue tag under which Binance series live in the hist store (`venue=binance` in the tree).
pub const VENUE: &str = "binance";

/// The idempotency guard for a `(symbol, interval, [start_ms, end_ms])` backfill window: a re-run
/// with the same window is a no-op in the store (batch-level dedup — never per-row value dedup, per
/// the store contract).
pub fn klines_commit_key(symbol: &str, interval: &str, start_ms: i64, end_ms: i64) -> String {
    crate::klines::commit_key(VENUE, symbol, interval, start_ms, end_ms)
}

/// Fetch Binance klines for `(symbol, interval)` over `[start_ms, end_ms]` (paging the 1000/req cap),
/// and `append_bars` them into the store under `(venue=binance, symbol, interval)`. Idempotent by
/// [`klines_commit_key`]. Returns rows written (0 if the window was already ingested).
///
/// NOTE: this DOES network I/O (Binance's public REST) — the offline gate exercises the pure map +
/// ingest path directly against a committed fixture; only the `#[ignore]`d live test drives this
/// end to end.
pub fn backfill_binance_klines(
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

/// [`backfill_binance_klines`] with the run's PACE carried across processes: the session's stored
/// observation seeds the pager, and whatever this run measures is folded back into the session.
///
/// Binance is the venue where a stored record moves the PACE, and today the only one: of the venues
/// on the shared CLI body it is the only one that paces against a DISCOVERED budget at all (its
/// `exchangeInfo` publishes `REQUEST_WEIGHT`; bybit/okx/deribit publish none). What it buys here:
/// the first page is paced at the real per-request weight instead of the pacer's pessimistic
/// constant — 2 rather than 5 on spot, i.e. 2.5x — and the backfill's ETA line is answerable from
/// page one rather than page two.
///
/// The three no-budget venues persist a record too (`crate::bybit`/`crate::okx`/`crate::deribit`),
/// but theirs is a REPORT: their pagers still sleep a hardcoded `PAGE_DELAY` whatever is stored, and
/// the record buys the ETA and a visible per-page cost, not speed. "Measures" and "steers" are two
/// properties, not one.
///
/// The session's KEY must be the market this symbol routes to
/// ([`vike_binance::data::kline_market`]): binance's two hosts publish different budgets (6000 vs
/// 2400/min) and price `/klines` differently (weight 2 vs 5), so one shared row would pace the other
/// host wrong. The pacer refuses a record whose budget is not the one it discovered, independently,
/// so a mis-keyed session degrades to "no seed" rather than to a bad one.
///
/// ## Concurrency
/// The fetch runs through [`vike_binance::data::fetch_klines_range_lanes`], which puts several
/// disjoint sub-windows in flight — at the SAME aggregate rate, because every lane draws its
/// dispatch slot from the ONE shared pacer this seed feeds. The lane count is derived per host from
/// what the SEQUENTIAL probe pages measured — the round trip and the venue's real per-request weight
/// — so it is **1 on the perp host** (a 312 ms target gap is already wider than a request, so a perp
/// backfill is exactly the sequential call it was) and **6 on spot**, where a sequential pager
/// physically cannot reach the 50 ms gap the operator's `utilization` asks for. Expect roughly
/// **5x** on a spot backfill (~4.9 min -> ~55 s for 24 months of 1m bars) and **no change** on perp;
/// both are arithmetic from the published budget, not a timed run.
///
/// A stored [`PaceSession`] record helps here in a second way now: it carries the per-request weight,
/// so a seeded run derives the right lane count from page ZERO instead of from the second probe page.
///
/// The failure policy is what makes that safe to write under one commit key: any lane failing fails
/// the whole fetch, so `append_bars` is never handed a partial window that the key would then mark
/// permanently ingested.
///
/// The bars written, the commit key and the returned row count are identical to
/// [`backfill_binance_klines`] — only the request PACING and the number of windows in flight differ.
pub fn backfill_binance_klines_paced(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    pace: &mut PaceSession,
) -> Result<usize, CollectError> {
    // The seed/measure/record protocol is `crate::klines::backfill_klines_paced`'s — identical on
    // all four paced venues, which is why only the fetcher below is spelled here.
    //
    // The lane count is DERIVED from the probe page inside the bridge (1 on fapi, ~6 on spot) and
    // the aggregate rate is unchanged either way, so there is nothing for this seam to configure —
    // `fetch_klines_range_paced` remains the strictly sequential call.
    crate::klines::backfill_klines_paced(
        hist,
        VENUE,
        symbol,
        interval,
        start_ms,
        end_ms,
        pace,
        vike_binance::data::fetch_klines_range_lanes,
    )
}
