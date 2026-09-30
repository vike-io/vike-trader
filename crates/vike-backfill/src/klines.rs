//! Shared klines-backfill plumbing for the crypto venues: `vike_binance::data::BinanceKlines`,
//! `vike_bybit::data::BybitKlines` and `vike_okx::data::OkxKlines` are byte-identical `KlineSource`
//! impls except for the venue string and which bridge crate's own `fetch_klines_range` supplies
//! the bars — docs/decisions/0094 moved each impl into its own bridge crate, out of this
//! one. This module is the one place the SHARED half still lives — everything that happens to a
//! venue's bars AFTER its [`vike_data::source::KlineSource`] impl has fetched them: [`commit_key`]
//! is the idempotency-key format string, [`drop_forming_tail`] is the still-forming-candle guard,
//! and [`ingest_klines`] is the refusal + guard + commit key + `append_bars` sequence
//! `crate::kline_source::backfill_kline_source` runs for every registry row.
//!
//! ⚠ It also held a `String`-erroring `backfill_klines` wrapper, its persisted-pace twin and the
//! shared CLI body of the one-shot `<venue>_backfill` programs, until docs/decisions/0094 deleted
//! the programs: the datahub's `Backfill` verb is the only path that reaches [`ingest_klines`] now.

use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;

use crate::error::CollectError;

/// The idempotency guard for a `(venue, symbol, interval, [start_ms, end_ms])` backfill window: a
/// re-run with the same window is a no-op in the store (batch-level dedup — never per-row value
/// dedup, per the store contract). Keys on the ORIGINAL interval string (e.g. "1m"), not any
/// venue-specific bar-size code.
pub(crate) fn commit_key(
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> String {
    format!("{venue}:{symbol}:{interval}:{start_ms}-{end_ms}")
}

/// Drop the trailing bar if its close time (`open + interval`) is still in the future relative to
/// `now_ms` — a still-forming candle the venue served as though it were closed. This can only
/// happen when the requested window's `end_ms` reaches into "now" (a live/recent backfill, not a
/// bounded historical one): binance/bybit serve the currently-forming candle as the last row of a
/// window that includes it; OKX's `history-candles` endpoint is closed-candle-only and never does
/// this, so the guard is a harmless no-op there. `interval` unparseable
/// ([`vike_model::time::interval_ms`] returns `None`) leaves `bars` untouched — this is a defensive
/// filter, never a hard failure path. At most one trailing bar can be forming (a venue never serves
/// data past "now"), so a single check (not a pop-while-loop) is exact.
///
/// ⚠ **The decline is no longer how a real `1w` request is answered, and this doc used to stop
/// here.** [`ingest_klines`] now REFUSES an interval [`vike_model::time::measures_bar_step`] says
/// nothing about, above this call and before the fetch, so nothing reaches this filter without a
/// measurable step. The decline survives as what it always was — a defensive answer to a string
/// nobody understands, which must not panic and must not drop a row on a guess — and
/// `unparseable_interval_leaves_bars_untouched` still pins it, with the argument
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 1 asked for
/// written at the test.
pub(crate) fn drop_forming_tail(bars: &mut Vec<Bar>, interval: &str, now_ms: i64) {
    let Some(step) = vike_model::time::interval_ms(interval) else { return };
    if bars.last().is_some_and(|b| b.ts + step > now_ms) {
        bars.pop();
    }
}

/// Fetch `venue` klines for `(symbol, interval)` over `[start_ms, end_ms]` via `fetch`, and
/// `append_bars` them into the store under `(venue, symbol, interval)` — the one place a kline batch
/// lands in the store. Idempotent by [`commit_key`]. Returns rows written (0 if the window was
/// already ingested).
///
/// `fetch` already speaks [`CollectError`], so a refusal crosses it intact: a
/// [`vike_data::source::KlineSource`] returns `SourceError::Refused`, its caller's `From` makes that
/// a `CollectError::Refused` — a request the seam cannot express, which was never a fetch failure
/// and must not be rendered as one (see `CollectError::Refused`'s own doc). That is what
/// `docs/decisions/0059-…`'s Phase 3 bought when it moved this body out of a wrapper whose closure
/// could only carry a `String`.
///
/// The fetched bars pass through [`drop_forming_tail`] against FETCH-time "now" (not the requested
/// `end_ms`), the window spends [`commit_key`], and `append_bars` writes under `(venue, symbol,
/// interval)`.
///
/// # THE FORMING-BAR REFUSAL — `docs/decisions/0059-…`'s Phase 1, bug B
///
/// An interval [`vike_model::time::measures_bar_step`] answers `false` for (`1w`, `1M`, `1mo`) is
/// refused HERE, as a [`CollectError::Refused`] rather than a `Fetch`, **before the `fetch` closure
/// runs** — so nothing is paged, nothing is written, and no [`commit_key`] is spent.
///
/// It sits at the seam for the reason [`drop_forming_tail`] sits here: this is the one place a
/// kline batch lands in the store, so a collector cannot forget it. Before this, the collector
/// supervisor's roster validator and `crates/vike-datahub/src/server.rs`'s `backfill_verb` each
/// refused on their own path and a hand-run `<venue>_backfill` program refused nothing — the gap
/// 0059 measured, reachable on binance, aster and bybit (okx's `history-candles` is
/// closed-candle-only and deribit's `resolution_code` errors above `1d`, so those two were immune by
/// accident rather than by decision). docs/decisions/0094 has since deleted the supervisor and the
/// programs. `backfill_verb` still refuses EARLIER, deliberately — a wire request is refused before
/// any venue is dispatched to, which is a better place to answer from than inside one venue's
/// ingest.
///
/// ⚠ **What it cost, stated because the refusal was a real narrowing.** An operator who ran
/// `binance_backfill … 1w …` before this change got rows; after it the run refused with a message
/// naming the step. That is the intended trade: the rows they got were a still-open weekly candle
/// recorded as closed, and the window's commit key made it PERMANENT —
/// `vike_data::DataFusionHist`'s `commit_rows` answers the corrective re-fetch with `Ok(0)`, and no
/// verb in this workspace retires a single key. Refusing is the only outcome that leaves the series
/// correctable. A caller who wants a week of bars asks for a step the store can measure (`7d`) or
/// resamples from one.
pub(crate) fn ingest_klines(
    hist: &DataFusionHist,
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    fetch: impl FnOnce(&str, &str, i64, i64) -> Result<Vec<Bar>, CollectError>,
) -> Result<usize, CollectError> {
    if !vike_model::time::measures_bar_step(interval) {
        return Err(CollectError::Refused(format!(
            "interval {interval:?} has no bar width this store can measure \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and \
             `1mo` are outside it). Refused BEFORE fetching {venue} {symbol}, because the \
             still-forming-candle guard silently declines on a step it cannot measure: the venue's \
             open candle would be stored as a closed bar and the window's commit key spent, making \
             a corrective re-fetch a silent zero-row success. Ask for a step the store can measure \
             (`7d` for a week), or resample from one."
        )));
    }
    let mut bars = fetch(symbol, interval, start_ms, end_ms)?;
    drop_forming_tail(&mut bars, interval, vike_model::now_ms());
    let key = commit_key(venue, symbol, interval, start_ms, end_ms);
    Ok(hist.append_bars(venue, symbol, interval, &bars, Some(&key))?)
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
