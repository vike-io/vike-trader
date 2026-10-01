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
//!
//! ⚠ **A SECOND ingest lives here beside it: [`ingest_klines_chunked`]**, the opt-in, day-chunked,
//! settled-only sibling for a lane whose window is far too long to hold as one `Vec` (OANDA's
//! twenty-one years of 5-second bars is tens of millions per instrument).
//! `crate::kline_source::backfill_kline_source_chunked` is its `KlineSource` face. A lane reaches it
//! by calling that instead of `backfill_kline_source`; [`ingest_klines`] and the six keyless venues
//! that use it are untouched, their commit keys byte for byte.

use vike_data::{DataFusionHist, HistStore, SeriesId};
use vike_model::{Bar, MS_PER_DAY};

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
/// `append_bars` them into the store under `(venue, symbol, interval)` — the one place a whole-window
/// kline batch lands in the store ([`ingest_klines_chunked`] is the other, a day at a time).
/// Idempotent by [`commit_key`]. Returns rows written (0 if the window was already ingested).
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

/// How long after a day chunk's last possible bar closes [`ingest_klines_chunked`] still waits before
/// it writes the chunk: the slack in its settle rule, `c1 + step + SETTLE_MARGIN_MS <= now_ms`.
///
/// ⚠ **A DEFAULT, not a measurement.** A venue with a candle endpoint (OANDA's, the first lane this
/// serves) answers a candle that closed a moment ago within seconds, so this is slack over that lag
/// rather than the lag itself, and the number is to be replaced by a measurement of how long a
/// just-closed candle takes to appear. The two ways of being wrong are lopsided, which is why it errs
/// long. Too SHORT, and a chunk can be written while the venue is still missing its last candles,
/// spending the chunk's commit key on a short answer for good — no verb in this workspace retires a
/// key. Too LONG, and the recent edge stays unstored a few minutes longer than it had to, which the
/// live feed covers in the meantime.
const SETTLE_MARGIN_MS: i64 = 10 * 60_000;

/// [`MS_PER_DAY`] as the `usize` step `Iterator::step_by` takes; a day in milliseconds fits `usize`
/// on every target this workspace builds for.
const DAY_STEP: usize = MS_PER_DAY as usize;

/// What one [`ingest_klines_chunked`] request did, chunk by chunk. The chunks of a request that ran to
/// the end partition into `skipped + fetched + empty + unsettled == chunks`.
#[derive(Debug, Default)]
pub(crate) struct ChunkedOutcome {
    /// Bars written — what `crate::kline_source::backfill_kline_source_chunked` answers.
    pub(crate) rows: usize,
    /// Day chunks in the request once it is rounded outward to whole days — settled or not, reached
    /// or not.
    pub(crate) chunks: u64,
    /// Settled chunks whose commit key was already spent: never fetched.
    pub(crate) skipped: u64,
    /// Chunks fetched that held at least one in-range bar, and were stored.
    pub(crate) fetched: u64,
    /// Chunks fetched that held no in-range bar: nothing written and no key spent.
    pub(crate) empty: u64,
    /// Chunks left unwritten because they were not yet settled — the first such chunk and every one
    /// after it. `0` when the whole request was settled.
    pub(crate) unsettled: u64,
    /// Bars a source served outside the chunk it was asked for, dropped.
    pub(crate) out_of_range: usize,
    /// When `unsettled > 0`: the first millisecond storage does NOT reach — the start of the first
    /// unsettled chunk.
    pub(crate) stops_before: Option<i64>,
    /// When `unsettled > 0`: the instant that first unsettled chunk settles.
    pub(crate) settles_at: Option<i64>,
}

/// **The day-chunked sibling of [`ingest_klines`]** — fetch `[start_ms, end_ms]` of `(symbol,
/// interval)` one UTC day at a time through `fetch`, and store each day under its OWN commit key.
/// Opt-in per lane: [`ingest_klines`], and the six keyless venues that use it, are untouched.
///
/// # Why it exists
///
/// [`ingest_klines`] is one fetch and one commit key per window, so a long window is one `Vec` in the
/// data daemon, written all or nothing. A lane whose window is twenty-one years of 5-second bars
/// cannot be held that way, and cannot be resumed after a failure in year twenty. This holds ONE
/// chunk — at most a day of bars, 17,280 at five seconds — at a time, and every chunk it finishes is
/// durable and is skipped by the next run. `crates/vike-backfill/src/venues/dukascopy.rs`'s
/// `backfill_quotes_then_bars` is the precedent for chunking inside the daemon; this diverges from
/// it in the ways below.
///
/// - **A chunk is one UTC day** on a grid anchored at epoch 0, and the request is rounded OUTWARD to
///   whole days: the first chunk starts at the midnight at or before `start_ms` and the last ends at
///   the last millisecond of the day holding `end_ms`. So every commit is a grid chunk and a
///   ragged-edge duplicate cannot exist — a request for part of a day stores the whole day. A chunk
///   is `[c0, c1]` with `c1 = c0 + MS_PER_DAY - 1`, inclusive, like every window in this file.
/// - **One key per chunk**: [`commit_key`] over the chunk's own bounds, the shared format, so the
///   `kind=bar` row of `crates/vike-data/src/store_kind.rs` describes it unchanged. Never the
///   request's bounds — two requests that cover a day share that day's key, which is the point.
/// - **A chunk whose key is spent is skipped without a fetch.** One lock-free manifest read
///   (`vike_data::DataFusionHist::series_has_commit`) per chunk answers it, so a repeat of a long
///   request costs one read per day and fetches only the days that are missing.
/// - **Settled only.** A chunk is written only once its last possible bar has closed and the venue
///   has had [`SETTLE_MARGIN_MS`] to serve it: `c1 + step + SETTLE_MARGIN_MS <= now_ms` (a bar that
///   opens at `c1` closes at `c1 + step`). The first chunk that fails the rule — the one holding
///   "now" — and every chunk after it are NOT written. The call still returns `Ok` with the rows
///   written before it, and the info log says where storage stops and when the first unwritten
///   chunk settles. The rule REPLACES [`drop_forming_tail`] here rather than adding to it: that
///   guard can only drop a forming candle after the venue served it and the key is about to be
///   spent, while a settled chunk cannot hold one.
/// - **The recent edge is deliberately left to the live feed.** The store can pair an early write
///   with a later one of the SAME bounds (`crates/vike-data/src/datafusion_hist.rs`'s
///   `append_bars_superseding`, the primitive `backfill_quotes_then_bars` builds its provisional
///   bars on), and an "up to now" REQUEST window changes with every call, so no provisional key can
///   be derived from it. The day grid does give a chunk fixed bounds, and that is still not a cure:
///   a provisional chunk would be frozen at its first fetch of the day, because its key stays spent
///   until it is superseded, and the supersede is refused for good if background compaction has
///   folded the provisional part into a multi-key part. So this path stores only what can no longer
///   change.
/// - **Bars outside `[c0, c1]` are dropped**, counted and warned about. A source that serves a little
///   past the window it was asked for would otherwise store the same bar in two adjacent chunks, and
///   the store dedups by commit key, never by row.
/// - **An empty chunk writes nothing and spends no key** — the store records nothing for an empty
///   batch — so a day with no bars (at an FX venue, about one day in seven: Saturdays; Sunday opens
///   in the evening UTC) is asked again on every re-run, one request each. Accepted.
/// - **A failure returns at once.** A failed fetch names the chunk and the bars already written
///   ([`CollectError::Fetch`]); a [`CollectError::Refused`] from the source, and any store error,
///   pass through unchanged. The chunks before it stay written and their keys are spent, so
///   repeating the request skips them and resumes at the chunk that failed.
/// - **The daemon holds one chunk's bars at a time.**
///
/// # Refusals — all before a fetch or a manifest read
///
/// An interval [`vike_model::time::measures_bar_step`] answers `false` for (`1w`, `1M`, `1mo`), one of
/// zero width (`0m` parses as `Some(0)`), an inverted window, a symbol that cannot be a directory
/// name (`vike_model::store_path::refuse_a_path_hostile_symbol`) and a window the UTC-day grid cannot
/// express at the very edge of `i64` are each a [`CollectError::Refused`]. The step must be
/// measurable because the settle rule needs a close time to wait for.
///
/// `now_ms` is the ONE instant every chunk is judged against: the caller reads the clock once per
/// request (`crate::kline_source::backfill_kline_source_chunked` does), and it is a parameter so a
/// test can stand exactly on the settle boundary. Nothing here bounds the NUMBER of chunks — each
/// costs at most one manifest read and one fetch, and the window's size is the caller's to bound.
///
/// # Two traps
///
/// ⚠ **One series, one ingest.** [`ingest_klines`] keys a window by the REQUEST's bounds and this
/// keys a day by the GRID's, so — bar a request that happens to be exactly one whole day — they are
/// different batches to a store that dedups by commit key and never by row: a `(venue, symbol,
/// interval)` series written through both holds every bar they share twice. Choose the lane per
/// venue, never per request.
///
/// ⚠ **A wrong settled chunk cannot be repaired.** A source that answers a settled day short — a
/// paging bug, a venue that lost a candle — has that answer stored and the day's key spent, and a
/// re-run skips it. No verb in this workspace retires a key; the remedy is to delete the series and
/// fetch it again.
pub(crate) fn ingest_klines_chunked(
    hist: &DataFusionHist,
    venue: &str,
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    now_ms: i64,
    mut fetch: impl FnMut(&str, &str, i64, i64) -> Result<Vec<Bar>, CollectError>,
) -> Result<ChunkedOutcome, CollectError> {
    if !vike_model::time::measures_bar_step(interval) {
        return Err(CollectError::Refused(format!(
            "interval {interval:?} has no bar width this store can measure \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and \
             `1mo` are outside it). Refused BEFORE fetching {venue} {symbol}: a day chunk is written \
             only once its last bar has closed, and a step nobody can measure has no close time to \
             wait for. Nothing was fetched or written and no commit key was spent."
        )));
    }
    // `0m` MEASURES — `interval_ms` reads it as `Some(0)` — so the check above lets it through, and
    // a bar of no duration is no bar.
    let Some(step) = vike_model::time::interval_ms(interval).filter(|&step| step > 0) else {
        return Err(CollectError::Refused(format!(
            "interval {interval:?} has zero width: a bar of no duration is no bar, and the settle \
             rule would wait for nothing. Refused BEFORE fetching {venue} {symbol}; nothing was \
             fetched or written and no commit key was spent. Ask for a positive step such as `1m`."
        )));
    };
    if start_ms > end_ms {
        return Err(CollectError::Refused(format!(
            "window [{start_ms}, {end_ms}] is inverted (start after end). Nothing was fetched or \
             written and no commit key was spent."
        )));
    }
    // This path reads a manifest under the series directory the symbol names before it writes
    // anything, so a symbol that cannot be a directory name is refused here rather than at the first
    // append — which is after a fetch, and after that read.
    vike_model::store_path::refuse_a_path_hostile_symbol(symbol).map_err(CollectError::Refused)?;
    let Some((first, last)) = chunk_grid(start_ms, end_ms) else {
        return Err(CollectError::Refused(format!(
            "window [{start_ms}, {end_ms}] reaches the edge of the i64 millisecond range, where the \
             UTC-day grid cannot express its outermost chunk. Nothing was fetched or written and no \
             commit key was spent."
        )));
    };

    let label = format!("{venue} {symbol} {interval}");
    let series = SeriesId::per_symbol("bar", venue, symbol, Some(interval.to_string()));
    let total = ((i128::from(last) - i128::from(first)) / i128::from(MS_PER_DAY) + 1) as u64;
    let started = std::time::Instant::now();
    let mut out = ChunkedOutcome { chunks: total, ..ChunkedOutcome::default() };
    for (n, c0) in (first..=last).step_by(DAY_STEP).enumerate() {
        let c1 = c0 + (MS_PER_DAY - 1);
        if !chunk_is_settled(c1, step, now_ms) {
            // Settledness only ever moves one way as `c1` grows, so this chunk and every one after
            // it are unsettled: the request is written as far as here, and no further.
            out.unsettled = total - n as u64;
            out.stops_before = Some(c0);
            out.settles_at = Some(settle_instant(c1, step));
            break;
        }
        let key = commit_key(venue, symbol, interval, c0, c1);
        if hist.series_has_commit(&series, &key)? {
            out.skipped += 1;
            tracing::debug!("{label} [{c0}, {c1}]: already stored (commit key spent), not fetched");
            continue;
        }
        let mut bars = fetch(symbol, interval, c0, c1).map_err(|e| match e {
            // One chunk of several names itself and what is already written, which stays written.
            CollectError::Fetch(why) => CollectError::Fetch(format!(
                "chunk {} of {total} [{c0}, {c1}] failed after {} {interval} bars were written \
                 (request [{start_ms}, {end_ms}]; the chunks before it stay stored, and repeating \
                 the request resumes at this one): {why}",
                n + 1,
                out.rows
            )),
            other => other,
        })?;
        let served = bars.len();
        bars.retain(|bar| (c0..=c1).contains(&bar.ts));
        let strays = served - bars.len();
        if strays > 0 {
            out.out_of_range += strays;
            tracing::warn!(
                "{label} [{c0}, {c1}]: the source served {strays} bar(s) outside the chunk it was \
                 asked for; dropped, so adjacent chunks cannot store one bar twice"
            );
        }
        if bars.is_empty() {
            out.empty += 1;
            tracing::debug!("{label} [{c0}, {c1}]: no bar in range; nothing written, no key spent");
            continue;
        }
        let written = hist.append_bars(venue, symbol, interval, &bars, Some(&key))?;
        out.rows += written;
        out.fetched += 1;
        tracing::debug!("{label} [{c0}, {c1}]: {} bars fetched, {written} written", bars.len());
    }
    if let Some(line) = unsettled_line(&out, &label, now_ms) {
        tracing::info!("{line}");
    }
    tracing::info!("{}", summary_line(&out, &label, (start_ms, end_ms), started.elapsed()));
    Ok(out)
}

/// The first and last day-chunk STARTS of `[start_ms, end_ms]` rounded outward to whole UTC days —
/// the grid anchored at epoch 0, `div_euclid` rather than `/` so a pre-1970 stamp rounds down like
/// any other. `None` when the grid cannot express the window: the first start would fall below
/// `i64::MIN`, or the last chunk's END would pass `i64::MAX`. Callers pass `start_ms <= end_ms`.
fn chunk_grid(start_ms: i64, end_ms: i64) -> Option<(i64, i64)> {
    let first = start_ms.div_euclid(MS_PER_DAY).checked_mul(MS_PER_DAY)?;
    let last = end_ms.div_euclid(MS_PER_DAY).checked_mul(MS_PER_DAY)?;
    last.checked_add(MS_PER_DAY - 1)?;
    Some((first, last))
}

/// The instant a day chunk whose last millisecond is `c1` becomes SETTLED: its last possible bar
/// opens at `c1` and closes at `c1 + step`, and the venue is then given [`SETTLE_MARGIN_MS`] to have
/// served it. Saturating, so a chunk at the edge of `i64` is never settled by an overflow.
fn settle_instant(c1: i64, step: i64) -> i64 {
    c1.saturating_add(step).saturating_add(SETTLE_MARGIN_MS)
}

/// Whether the chunk ending at `c1` is settled at `now_ms` — the ONE spelling of the rule
/// [`ingest_klines_chunked`] writes by. At the instant itself it IS settled: the margin is slack, not
/// a strict inequality anyone measured.
fn chunk_is_settled(c1: i64, step: i64, now_ms: i64) -> bool {
    settle_instant(c1, step) <= now_ms
}

/// UTC, to the second, as `2026-09-30T00:00:00Z` — what an operator reads a log instant as.
fn utc(ms: i64) -> String {
    vike_model::runs::utc_rfc3339(ms.div_euclid(1_000))
}

/// The line that says where storage STOPS and when the rest settles — `None` when the whole request
/// was settled and nothing was left unwritten. A function returning its text rather than a log call,
/// so a test holds it to what an operator reads; [`ingest_klines_chunked`] logs it at `info`.
fn unsettled_line(out: &ChunkedOutcome, label: &str, now_ms: i64) -> Option<String> {
    let (stops, settles) = (out.stops_before?, out.settles_at?);
    Some(format!(
        "{label}: storage stops before {} ({stops}). {} of {} day chunks are not settled at {} \
         ({now_ms}) and were NOT written; the first settles at {} ({settles}). The recent edge is \
         the live feed's — ask again after that instant for the rest.",
        utc(stops),
        out.unsettled,
        out.chunks,
        utc(now_ms),
        utc(settles),
    ))
}

/// The ONE summary line a request leaves in the log, at `info`; per-chunk lines are `debug`, at the
/// site. A function returning its text for the reason [`unsettled_line`] is.
fn summary_line(
    out: &ChunkedOutcome,
    label: &str,
    (start_ms, end_ms): (i64, i64),
    elapsed: std::time::Duration,
) -> String {
    let strays = if out.out_of_range > 0 {
        format!("; {} out-of-range bars dropped", out.out_of_range)
    } else {
        String::new()
    };
    format!(
        "{label} [{start_ms}, {end_ms}]: {} day chunks: {} skipped (already stored), {} fetched, {} \
         empty, {} unsettled; {} bars written in {elapsed:.1?}{strays}",
        out.chunks, out.skipped, out.fetched, out.empty, out.unsettled, out.rows,
    )
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;
