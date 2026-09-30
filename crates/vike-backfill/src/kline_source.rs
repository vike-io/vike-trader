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
//! `crates/vike-ops/tests/collector_dispatch_gate.rs` still walks the `KlineSource` impls under
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

use vike_data::DataFusionHist;
use vike_data::source::KlineSource;

use crate::error::CollectError;

/// **The ingest half** — fetch through `source`, then guard and store. The one place a kline lands
/// in the store; since docs/decisions/0094 deleted the one-shot kline programs, the datahub's wire
/// verb (folding `vike_datahub::backfill::KLINE_SOURCES`) is the only path that asks.
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
