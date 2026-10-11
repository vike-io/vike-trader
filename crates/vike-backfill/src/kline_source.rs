//! **The store-touching half of the kline pipeline** — [`backfill_kline_source`], which turns a
//! [`KlineSource`] fetch into rows landed in the hist store.
//!
//! # Where the registry went
//!
//! Until docs/decisions/0094 this file also held `KLINE_SOURCES`, the ONE dispatch
//! registry every kline path folded instead of re-listing venues
//! (`docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 3). It
//! moved to `crates/vike-datahub/src/backfill.rs`'s `KLINE_SOURCES` — the one process that actually
//! DISPATCHES a venue string to a collector — so this crate no longer names a single kline bridge
//! crate; only the venue's own bridge crate and the datahub do. The wire verb's
//! `real_backfill_table` folds that static directly, with no cross-crate static to reach through.
//! `crates/vike-ops/tests/venues/collector_dispatch_gate.rs` still walks the `KlineSource` impls under
//! `crates/bridges/<venue>/src/` against it, in both directions — the gate moved with the registry,
//! not with this file.
//!
//! # Why `fetch` is store-free and ingest is not
//!
//! [`FundingRateSource`](vike_data::source::FundingRateSource) and [`crate::eod::EodSource`] make
//! the same split: the trait returns rows and touches no store, and ONE non-trait function owns the
//! commit key, the dedup and the append. For klines that function is [`backfill_kline_source`] —
//! `crate::klines::ingest_klines`, so `drop_forming_tail`, the
//! `{venue}:{symbol}:{interval}:{start}-{end}` commit key and `append_bars` are inherited by every
//! source and cannot be forgotten by one, and the forming-bar REFUSAL (0059 Phase 1) is inherited
//! the same way. A venue's own [`KlineSource`] impl is `fetch` and nothing else — paged, and a
//! request the one-symbol seam cannot express is a `SourceError::Refused` there
//! (`vike_hyperliquid::history::HyperliquidKlines` is the worked example), never a guess; this
//! function carries it into a `CollectError::Refused` unchanged.
//!
//! # The chunked sibling
//!
//! [`backfill_kline_source_chunked`] is the same fetch-then-ingest for a lane whose window is too
//! long to hold as one `Vec`: it asks the source ONE UTC DAY at a time and stores each day under its
//! own commit key, settled days only. It is opt-in per lane — a lane reaches it by calling it in
//! place of [`backfill_kline_source`] — and the six keyless venues never do, so their per-window keys
//! stay byte for byte what they were. The whole argument, and its policies, is on
//! `crate::klines::ingest_klines_chunked`.

use vike_data::DataFusionHist;
use vike_data::source::KlineSource;

use crate::error::CollectError;

/// **The ingest half** — fetch through `source`, then guard and store. The one place a whole window
/// lands in the store as ONE batch (its chunked sibling below is the other, a day at a time); since
/// docs/decisions/0094 deleted the one-shot kline programs, the datahub's wire verb (folding
/// `vike_datahub::backfill::KLINE_SOURCES`) is the only path that asks.
///
/// It is `crate::klines::ingest_klines` — the still-forming-candle guard (`drop_forming_tail`,
/// against FETCH-time "now", not the requested `end_ms`), the
/// `{venue}:{symbol}:{interval}:{start}-{end}` commit key, and `append_bars` under
/// `source.venue()`. Returns rows written; 0 means the window's commit key was already spent.
///
/// ⚠ An interval `vike_model::time::measures_bar_step` answers `false` for (`1w`, `1M`, `1mo`) is
/// REFUSED by `crate::klines::ingest_klines` before `source.fetch` is called — so no `KlineSource`
/// impl ever sees one, and none needs its own check. This used to read "the guard DECLINES … and
/// the consequence belongs to the CALLER"; the caller it belonged to was every one-shot bin, which
/// decided nothing, and `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`
/// Phase 1 moved the decision here. The datahub's wire verb still refuses EARLIER, on purpose — see
/// `crate::klines::ingest_klines`'s own doc.
pub fn backfill_kline_source(
    hist: &DataFusionHist,
    source: &dyn KlineSource,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    crate::klines::ingest_klines(
        hist,
        source.venue(),
        symbol,
        interval,
        start_ms,
        end_ms,
        |sym, iv, s, e| source.fetch(sym, iv, s, e).map_err(CollectError::from),
    )
}

/// **The chunked ingest half** — [`backfill_kline_source`] for a window too long to hold as one
/// `Vec`: fetch through `source` ONE UTC DAY at a time and store each day under its own commit key,
/// so the daemon holds one day's bars at a time and a crash costs only the day in flight. Returns
/// rows written; `0` means nothing new was written, whether every day was already stored or none was
/// settled yet.
///
/// It is `crate::klines::ingest_klines_chunked`, whose doc carries the whole contract and which this
/// only adapts to a [`KlineSource`]. What a caller must know:
///
/// - the window is rounded OUTWARD to whole UTC days, so asking for part of a day stores the whole
///   day, and the keys are the day grid's (`{venue}:{symbol}:{interval}:{c0}-{c1}` with `venue` from
///   [`KlineSource::venue`]) rather than the request's;
/// - only SETTLED days are written — the day that holds "now", and every day after it, are left to
///   the live feed, and the info log says where storage stops and when the rest settles. The clock is
///   read ONCE per request, here, so every day of it is judged against the same instant;
/// - a day whose key is already spent is never fetched, so repeating a request resumes it, and a
///   failure names the day and the bars already written, which stay written;
/// - an interval [`vike_model::time::measures_bar_step`] answers `false` for is REFUSED before any
///   fetch, as [`backfill_kline_source`] refuses it — and so is one of zero width (`0m`), which
///   that function leaves to the datahub verb's own check;
/// - `should_stop` is asked at the top of every day chunk, never inside one, and `true` ends the
///   request at that boundary in a [`CollectError::Stopped`] — never `Ok` — with every chunk before
///   it stored; repeating the request resumes there. A caller that cannot be cancelled passes a
///   probe that never fires (`&|| false`).
///
/// ⚠ One series takes ONE of the two ingests. This one's keys are the grid's and the other's are the
/// request's, so — bar a request that is exactly one whole day — they are different batches to a store
/// that dedups by commit key and never by row: a series written through both stores what they share
/// twice.
pub fn backfill_kline_source_chunked(
    hist: &DataFusionHist,
    source: &dyn KlineSource,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    backfill_kline_source_chunked_at(
        hist,
        source,
        symbol,
        interval,
        start_ms,
        end_ms,
        vike_model::now_ms(),
        should_stop,
    )
}

/// [`backfill_kline_source_chunked`] against an explicit `now_ms` instead of the wall clock: the same
/// ingest, so a test can stand a request exactly on a chunk's settle boundary and stay deterministic.
/// The public entry reads the clock once and calls this.
pub(crate) fn backfill_kline_source_chunked_at(
    hist: &DataFusionHist,
    source: &dyn KlineSource,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    now_ms: i64,
    should_stop: &dyn Fn() -> bool,
) -> Result<usize, CollectError> {
    crate::klines::ingest_klines_chunked(
        hist,
        source.venue(),
        symbol,
        interval,
        (start_ms, end_ms),
        now_ms,
        should_stop,
        |sym, iv, s, e| source.fetch(sym, iv, s, e).map_err(CollectError::from),
    )
    .map(|outcome| outcome.rows)
}
