//! Shared klines-backfill plumbing for the crypto venues: `crate::binance`, `crate::bybit`, and
//! `crate::okx` were byte-identical except for the venue string and which bridge crate's
//! `fetch_klines_range` supplies the bars (`vike_binance`/`vike_bybit`/`vike_okx`, one bridge
//! crate per venue). This module is the one place that shape lives: [`commit_key`] is the
//! idempotency-key format string, [`backfill_klines`] is the fetch→ingest orchestration each
//! venue's `backfill_<venue>_klines` binds to (with its own `VENUE` const + fetcher fn),
//! [`backfill_klines_paced`] is that same orchestration wrapped in the seed/measure/record protocol
//! each venue's `backfill_<venue>_klines_paced` binds to, and
//! [`run_klines_backfill_cli`] is the CLI body the `<venue>_backfill` bins call with their
//! venue's already-bound backfill fn — same 5-positional-arg shape, same usage/error text,
//! differing only in the venue name. It also owns the PERSISTED PACE session (see its own doc): the
//! CLI body is a binary's whole body and the one place every venue passes through, which makes it
//! the right layer for a file read, and the wrong one for anything in a library to do.

use std::process::ExitCode;

use vike_data::{DataFusionHist, HistStore};
use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

use crate::cli::log_config;
use crate::error::CollectError;
use crate::pace_book::PaceSession;

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

/// Fetch `venue` klines for `(symbol, interval)` over `[start_ms, end_ms]` via `fetch` (each
/// venue's own paged `fetch_klines_range`), and `append_bars` them into the store under `(venue,
/// symbol, interval)`. Idempotent by [`commit_key`]. Returns rows written (0 if the window was
/// already ingested).
///
/// The fetched bars pass through [`drop_forming_tail`] first (fetch-time "now", not the requested
/// `end_ms`) — centrally, so every venue-direct kline collector in this crate gets the
/// still-forming-candle guard from this one pager rather than each needing its own.
///
/// ⚠ **An interval `vike_model::time::measures_bar_step` answers `false` for is REFUSED by
/// [`ingest_klines`], before the fetch** — `1w`, `1M`, `1mo`. This paragraph used to say the
/// consequence "belongs to the CALLER" and name the two automated dispatch paths that decided it,
/// ending "a hand-run `*_backfill` bin still does not, which is
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 1 work".
/// That work is this change: the decision moved to the SEAM, so a caller cannot forget it, and the
/// five one-shot bins inherit it exactly as they inherit [`drop_forming_tail`] and [`commit_key`].
/// `crate::supervisor::config`'s `validate` and `crates/vike-datahub/src/server.rs`'s
/// `backfill_verb` still refuse EARLIER and deliberately keep doing so — a roster is refused at
/// parse time and a wire request before any venue is dispatched to, which are better places to
/// answer from than inside one venue's ingest.
pub(crate) fn backfill_klines(
    hist: &DataFusionHist,
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    fetch: impl FnOnce(&str, &str, i64, i64) -> Result<Vec<Bar>, String>,
) -> Result<usize, CollectError> {
    ingest_klines(hist, venue, symbol, interval, start_ms, end_ms, |sym, iv, s, e| {
        fetch(sym, iv, s, e).map_err(CollectError::Fetch)
    })
}

/// [`backfill_klines`]'s BODY, over a fetch that already speaks [`CollectError`] — the one place a
/// kline batch lands in the store.
///
/// The split is `docs/decisions/0059-…`'s Phase 3 and buys exactly one thing:
/// `crate::kline_source::backfill_kline_source` can reach this same body while letting a
/// [`crate::kline_source::KlineSource`] return a `CollectError::Refused` — a request the seam
/// cannot express, which was never a fetch failure and must not be rendered as one (see
/// `CollectError::Refused`'s own doc). [`backfill_klines`] above is now a two-line wrapper that
/// re-applies the `String` → `CollectError::Fetch` mapping it always did, so the venue entry points
/// that still pass a `String`-erroring closure are byte-identical.
///
/// Everything else is unchanged and deliberately so: the fetched bars pass through
/// [`drop_forming_tail`] against FETCH-time "now", the window spends [`commit_key`], and
/// `append_bars` writes under `(venue, symbol, interval)`.
///
/// # THE FORMING-BAR REFUSAL — `docs/decisions/0059-…`'s Phase 1, bug B
///
/// An interval [`vike_model::time::measures_bar_step`] answers `false` for (`1w`, `1M`, `1mo`) is
/// refused HERE, as a [`CollectError::Refused`] rather than a `Fetch`, **before the `fetch` closure
/// runs** — so nothing is paged, nothing is written, and no [`commit_key`] is spent.
///
/// It sits at the seam rather than at the five one-shot bins for the reason
/// [`drop_forming_tail`] sits here: this is the one place a kline batch lands in the store, so a
/// collector cannot forget it. Before this, `crate::supervisor::config`'s `validate` and
/// `crates/vike-datahub/src/server.rs`'s `backfill_verb` each refused on their own path and a
/// hand-run `<venue>_backfill` bin refused nothing — the gap 0059 measured, reachable on binance,
/// aster and bybit (okx's `history-candles` is closed-candle-only and deribit's `resolution_code`
/// errors above `1d`, so those two were immune by accident rather than by decision).
///
/// ⚠ **What it costs, stated because the refusal is a real narrowing of what the bins accept.** An
/// operator who ran `binance_backfill … 1w …` yesterday got rows; today the run refuses with a
/// message naming the step. That is the intended trade: the rows they got were a still-open weekly
/// candle recorded as closed, and the window's commit key made it PERMANENT —
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

/// [`backfill_klines`] with the run's PACE carried across processes: the session's stored
/// observation seeds `fetch`, and whatever that pager measured is folded back into the session.
///
/// This is the `_paced` half of the same argument [`backfill_klines`] makes for the plain half.
/// `crate::binance`, `crate::bybit`, `crate::okx` and `crate::deribit` each hand-repeated this
/// protocol — `seed()`, the `measured` cell, the wrapping closure, `record()` — and the four copies
/// differed in NOTHING but the venue const and which bridge's fetcher was called. All
/// four fetchers already share one signature to the letter — `(&str, &str, i64, i64,
/// Option<&PaceSample>) -> Result<(Vec<Bar>, Option<PaceSample>), String>` — which is what lets a
/// venue pass its fn item straight in, exactly as the non-paced twins pass `fetch_klines_range` to
/// [`backfill_klines`].
///
/// ⚠ **The pager's report rides out through the `measured` CELL rather than through
/// [`backfill_klines`]'s return type**, and that is deliberate rather than incidental: the fetch
/// closure is the only thing that sees a report, and widening the shared fetch→ingest seam for it
/// would put a pace concept in front of every venue that produces none (aster today, and the plain
/// `backfill_<venue>_klines` entry points always).
///
/// `record` runs only on the SUCCESS path here, unchanged from the four copies: a fetch that
/// returned `Err` propagates through `?` before it, and the run's measurement is instead persisted
/// one layer out by [`run_klines_backfill_cli`], whose `pace.save()` sits outside its `match` for
/// exactly that reason.
///
/// What a seeded record actually BUYS is per-venue and is argued at each venue's own entry point —
/// binance re-paces its first page against a discovered budget, while bybit/okx/deribit sleep their
/// hardcoded `PAGE_DELAY` whatever is stored and get an ETA plus a per-page cost out of it. Nothing
/// about that difference lives in this function: it is entirely a property of the `fetch` handed in.
// The pace session on top of `backfill_klines`' existing seven — the same shape, and the same
// reason, as the paged fetcher one layer down in `crates/bridges/binance/src/family/klines.rs`.
// Bundling them into a struct would buy nothing: every argument is already the caller's own
// parameter, forwarded verbatim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn backfill_klines_paced(
    hist: &DataFusionHist,
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    pace: &mut PaceSession,
    fetch: impl FnOnce(
        &str,
        &str,
        i64,
        i64,
        Option<&PaceSample>,
    ) -> Result<(Vec<Bar>, Option<PaceSample>), String>,
) -> Result<usize, CollectError> {
    let seed = pace.seed();
    let mut measured = None;
    let rows =
        backfill_klines(hist, venue, symbol, interval, start_ms, end_ms, |sym, iv, s, e| {
            let (bars, report) = fetch(sym, iv, s, e, seed.as_ref())?;
            measured = report;
            Ok(bars)
        })?;
    pace.record(measured);
    Ok(rows)
}

/// The usage text every `<venue>_backfill` bin prints for `--help` — built from the venue name
/// because the five bins differ in nothing else. Shared here for the same reason
/// [`run_klines_backfill_cli`] is: five copies of one paragraph is five chances to drift.
///
/// It documents the arguments a caller has to get right (all five are required and POSITIONAL, in
/// order, with epoch MILLISECONDS — not seconds, the mistake that silently backfills 1970) plus
/// the two environment knobs that change where the run reads and writes.
pub(crate) fn klines_usage(venue: &str) -> String {
    format!(
        "\
usage: {venue}_backfill <ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>

Fetch {venue} klines for one symbol over a closed time window and append them to the DataFusion
hist store at ROOT_DIR, under kind=bar/venue={venue}/symbol=SYMBOL/interval=INTERVAL. Idempotent by
commit key: re-running the same window writes 0 rows. A still-forming final candle is dropped, and
an INTERVAL this store cannot measure is REFUSED before anything is fetched (see INTERVAL below).

All five arguments are REQUIRED and positional, in this order:
  ROOT_DIR    hist-store root directory (created if absent)
  SYMBOL      venue symbol, e.g. BTCUSDT
  INTERVAL    bar interval: a whole count plus one of s/m/h/d, e.g. 1m 5m 1h 1d 7d.
              `1w`, `1M` and `1mo` are REFUSED: the still-forming-candle guard cannot measure
              them, so the venue's open candle would be stored as closed and the window's
              commit key spent, which makes a corrective re-fetch a silent 0-row success.
  START_MS    window start, epoch MILLISECONDS (inclusive)
  END_MS      window end, epoch MILLISECONDS (inclusive)

  -h, --help     print this and exit 0
  -V, --version  print the version and exit 0

environment:
  VIKE_PACE_BOOK      persisted per-page pace record (default <project>/settings/state/pace.json)
  VIKE_LOG_FILE_LEVEL file-log level; this bin defaults to `warn` (see cli::log_config)"
    )
}

/// Shared CLI body for the `<venue>_backfill` bins (`binance_backfill`/`bybit_backfill`/
/// `okx_backfill`/`aster_backfill`/`deribit_backfill`): parse the
/// `<ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>` positional args, open the hist store, open
/// the persisted-pace session, run `backfill`, save the pace, and report — byte-identical across
/// venues except for `venue`, which names the log file prefix (`{venue}-backfill`), the usage-line
/// fallback program name (`{venue}_backfill`), and the "fetching {venue} ..." progress line.
///
/// `vars` is the process environment as the CALLING BIN collected it
/// (`&std::env::vars().collect()`), forwarded to [`crate::cli::pace_book_path`] — the same shape
/// every bin already uses for [`crate::cli::store_root`], and the reason neither resolver reads the
/// environment from inside this library.
///
/// ## The pace session
/// This is the layer that owns the persisted pace, because it is the layer that is a BINARY's whole
/// body and the one place every one of these venues passes through: it resolves the file
/// ([`crate::cli::pace_book_path`]: `$VIKE_PACE_BOOK` → `<project>/settings/state/pace.json` →
/// `<ROOT_DIR>/pace.json`), loads the previous run's record BEFORE the fetch, and saves whatever this
/// run measured AFTER it — including on the error path, since a backfill that measured its pace and
/// then failed to fetch page 900 still measured its pace.
///
/// `market_of` decides whether this venue takes part at all, and says so at the bin rather than
/// here:
/// * `Some(f)` — the venue MEASURES, and `f(symbol)` names the market half of the record's key.
///   Four of the five today: binance splits `"spot"`/`"perp"` (see
///   [`vike_binance::data::kline_market`], which derives it from the same routing the fetcher uses)
///   because its two HOSTS publish different budgets; bybit/okx/deribit take one `"klines"` row each
///   (`crate::bybit::kline_market` and its siblings), because each pages a single endpoint under a
///   single IP limit.
/// * `None` — the venue measures nothing. Only aster today, whose host is env-resolved and whose
///   bridge therefore exposes no `_paced` fetch to plumb one through. The session is
///   [`crate::pace_book::PaceSession::disabled`]: no file is read, none is written.
///
/// ⚠ **A record does not mean a faster backfill.** What it steers depends on whether the venue
/// publishes a budget: binance's genuinely re-paces the first page, while bybit/okx/deribit's pagers
/// answer their hardcoded `PAGE_DELAY` whatever is stored, and the record buys an ETA from page zero
/// plus an operator-readable per-page cost. Both are worth persisting; only one of them is a pace.
pub fn run_klines_backfill_cli(
    venue: &str,
    market_of: Option<fn(&str) -> &'static str>,
    vars: &std::collections::HashMap<String, String>,
    backfill: impl FnOnce(
        &DataFusionHist,
        &str,
        &str,
        i64,
        i64,
        &mut crate::pace_book::PaceSession,
    ) -> Result<usize, CollectError>,
) -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let bin = format!("{venue}_backfill");
    let usage = klines_usage(venue);
    // ⚠ BEFORE `vike_log::init`: answering a question about the command line must not create a log
    // file. All FIVE venue bins get `--help`/`--version`/unknown-argument rejection from this one
    // call — the seam is why this family cost five lines rather than five copies.
    let spec = crate::cli::CliSpec {
        bin: &bin,
        usage: &usage,
        valued: &[],
        toggles: &[],
        // `<ROOT_DIR> <SYMBOL> <INTERVAL> <START_MS> <END_MS>` — purely positional, no flags.
        positionals: 5,
    };
    if let Some(code) = spec.short_circuit(&args) {
        return code;
    }

    let _log_guards = vike_log::init(log_config(format!("{venue}-backfill")));
    tracing::info!("{venue}-backfill starting");
    if args.len() < 6 {
        // Reached only by a SHORT invocation — the triage above already rejected the long and the
        // misspelled ones. `CliSpec` caps the positional count; it cannot express "and all five are
        // required", which is this bin's own rule. Exit 2 (a usage error) rather than the old 1,
        // matching every other bin in the crate.
        eprintln!("{bin}: all five arguments are required\n\n{usage}");
        return ExitCode::from(2);
    }
    let root = &args[1];
    let symbol = &args[2];
    let interval = &args[3];
    let start_ms: i64 = match args[4].parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("bad START_MS {:?}: {e}", args[4]);
            return ExitCode::FAILURE;
        }
    };
    let end_ms: i64 = match args[5].parse() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("bad END_MS {:?}: {e}", args[5]);
            return ExitCode::FAILURE;
        }
    };

    let hist = match DataFusionHist::open(root) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("open hist store at {root}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Opened BEFORE the fetch (its whole value is seeding the first page) and saved after, on both
    // outcomes. A venue with no `market_of` gets an inert session: no file touched either way.
    let mut pace = match market_of {
        Some(market_of) => crate::pace_book::PaceSession::open(
            crate::cli::pace_book_path(None, vars, std::path::Path::new(root)),
            venue,
            market_of(symbol),
        ),
        None => crate::pace_book::PaceSession::disabled(venue, ""),
    };

    tracing::info!("fetching {venue} {symbol} {interval} [{start_ms}, {end_ms}] -> {root} ...");
    let result = backfill(&hist, symbol, interval, start_ms, end_ms, &mut pace);
    // Best effort, and deliberately OUTSIDE the match: a run that paced hundreds of pages and then
    // failed on one has still learned what a request costs, and that is exactly the run whose
    // successor most wants the answer. `save` is a no-op unless something was measured.
    pace.save();
    match result {
        Ok(n) => {
            tracing::info!("appended {n} kline bars (0 = window already ingested)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            tracing::error!("backfill failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture bar at `ts` — only `ts` matters for [`drop_forming_tail`].
    fn bar(ts: i64) -> Bar {
        Bar {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn drops_a_still_forming_final_candle() {
        // 1m interval; a candle open at ts=now-30s has not closed yet (close = ts+60s > now).
        let now = 1_700_000_090_000_i64; // 90s past an arbitrary epoch
        let mut bars = vec![
            bar(1_700_000_000_000 - 3 * 60_000),
            bar(1_700_000_000_000),
            bar(1_700_000_060_000),
        ];
        // last bar opened at ts=1_700_000_060_000; close = +60_000 = 1_700_000_120_000 > now (90_000 mark)
        drop_forming_tail(&mut bars, "1m", now);
        assert_eq!(bars.len(), 2, "the still-forming last candle is dropped");
        assert_eq!(bars.last().unwrap().ts, 1_700_000_000_000);
    }

    #[test]
    fn keeps_a_fully_closed_final_candle() {
        // close time (ts + 60_000) exactly equals now: NOT in the future, so it's closed and kept
        // (matches tif_expired-style inclusive-boundary conventions: at the boundary it's done, not
        // still forming).
        let ts = 1_700_000_000_000_i64;
        let now = ts + 60_000;
        let mut bars = vec![bar(ts)];
        drop_forming_tail(&mut bars, "1m", now);
        assert_eq!(bars.len(), 1, "a candle exactly at its close time is closed, not forming");
    }

    #[test]
    fn only_the_final_candle_is_ever_checked() {
        // An interior "gap" candle whose own close would be in the future relative to some OTHER
        // clock is irrelevant — only the LAST row is examined (a venue never serves bars past now,
        // so no earlier row can be forming).
        let now = 1_700_000_090_000_i64;
        let mut bars = vec![bar(1_700_000_000_000)]; // close = +60_000 = 1_700_000_060_000 <= now
        drop_forming_tail(&mut bars, "1m", now);
        assert_eq!(bars.len(), 1, "the sole closed candle is untouched");
    }

    #[test]
    fn empty_bars_is_a_no_op() {
        let mut bars: Vec<Bar> = Vec::new();
        drop_forming_tail(&mut bars, "1m", 1_700_000_000_000);
        assert!(bars.is_empty());
    }

    /// **THE TEST `docs/decisions/0059-…`'s Phase 1 said had to be argued against, kept — with the
    /// argument.**
    ///
    /// 0059 recorded this pin as the third reason bug B's obvious fix was refused: the behaviour it
    /// asserts *was* written down as intentional, and "that reason is right for a garbage string and
    /// wrong for a real interval three dispatched venues serve". Both halves of that sentence are
    /// true, and they are about DIFFERENT layers, which is why the fix is a refusal one layer up
    /// rather than a change here:
    ///
    /// * For a string nobody understands (`"not-an-interval"`), declining is still right. A
    ///   defensive filter that panicked would take down a collector over a typo, and one that
    ///   guessed a step would drop a row it cannot prove is forming. Neither is an improvement on
    ///   doing nothing.
    /// * For `1w`/`1M`/`1mo` the decline was never the whole answer, because those are REAL steps a
    ///   venue serves — so the outcome was a still-open candle stored as closed under a spent commit
    ///   key. [`ingest_klines`]'s refusal is where that is now decided, above this function and
    ///   before the fetch, so no real request reaches this filter with a step it cannot measure.
    ///
    /// So this pin now covers exactly the case it was always right about, and
    /// `an_unmeasurable_interval_is_refused_before_anything_is_fetched` covers the case it was
    /// wrongly read as covering. Do not "fix" this one by making it drop or panic: the two tests
    /// are a pair, and deleting this one would make the seam's refusal the only thing standing
    /// between a garbage string and a popped row.
    #[test]
    fn unparseable_interval_leaves_bars_untouched() {
        let mut bars = vec![bar(1_700_000_000_000)];
        drop_forming_tail(&mut bars, "not-an-interval", i64::MAX);
        assert_eq!(bars.len(), 1);

        // ...and the three REAL steps, at this layer, still decline — the property the refusal one
        // layer up now makes unreachable from a collector, pinned here so that moving or weakening
        // that refusal changes an observable rather than nothing.
        for interval in ["1w", "1M", "1mo"] {
            let mut bars = vec![bar(1_700_000_000_000)];
            drop_forming_tail(&mut bars, interval, i64::MAX);
            assert_eq!(bars.len(), 1, "{interval} must still be DECLINED here, not acted on");
        }
    }

    #[test]
    fn okx_style_bounded_historical_window_is_unaffected() {
        // OKX's history-candles endpoint never serves an in-progress candle — a bounded historical
        // window (end_ms far in the past relative to "now") has no forming tail to drop; this proves
        // the shared guard is a harmless no-op in that case, not that OKX is special-cased.
        let long_ago_close = 1_600_000_060_000_i64;
        let mut bars = vec![bar(1_600_000_000_000)]; // close = 1_600_000_060_000
        drop_forming_tail(&mut bars, "1m", long_ago_close + 10 * 60_000_000); // "now" far later
        assert_eq!(bars.len(), 1, "a long-closed candle is never dropped");
    }

    // ── THE INGEST SPLIT IS BYTE-IDENTICAL (0059 Phase 3) ───────────────────────────────────────
    //
    // Phase 3 moved [`backfill_klines`]'s body into [`ingest_klines`] and left the old entry point
    // as a wrapper that re-applies the `String` -> `CollectError::Fetch` mapping. THE CLAIM IS THAT
    // NOTHING ELSE MOVED, and these prove it on the observable surface the way
    // `crates/vike-config/tests/mirror.rs`'s `the_mirror_changes_no_effective_value` does: drive the
    // SAME fixtures through BOTH paths and demand equality, including the rendered error.
    //
    // Two separate temp stores, never one — a shared store would make the second call a
    // commit-key no-op and the comparison would pass for the wrong reason.

    /// A store over its own temp dir, returned with the dir so the dir outlives it.
    fn store() -> (tempfile::TempDir, DataFusionHist) {
        let dir = tempfile::tempdir().expect("temp dir");
        let hist = DataFusionHist::open(dir.path()).expect("open temp store");
        (dir, hist)
    }

    /// Everything observable about one ingest call: what it returned, and what landed in the store.
    fn observe(
        hist: &DataFusionHist,
        outcome: Result<usize, CollectError>,
    ) -> (Result<usize, String>, Vec<Bar>) {
        let bars = hist
            .load_bars("testvenue", "TESTSYM", "1m", vike_data::TsRange::all())
            .expect("read the series back");
        (outcome.map_err(|e| e.to_string()), bars)
    }

    /// Three 1-minute bars, the last of which is STILL FORMING — so a path that kept the guard
    /// writes two and a path that lost it writes three.
    ///
    /// The last bar OPENS at "now", so its close is `now + 60_000` and
    /// [`drop_forming_tail`]'s `ts + step > now` holds for any clock the ingest reads afterwards
    /// (it re-reads `now_ms()` itself, a few microseconds later, and the margin is a whole minute).
    fn three_bars_last_forming() -> Vec<Bar> {
        let t0 = vike_model::now_ms() - 2 * 60_000;
        vec![bar(t0), bar(t0 + 60_000), bar(t0 + 120_000)]
    }

    #[test]
    fn the_wrapper_and_the_ingest_write_the_same_rows_and_spend_the_same_key() {
        let bars = three_bars_last_forming();

        let (_d1, old_way) = store();
        let a = observe(
            &old_way,
            backfill_klines(&old_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Ok(bars.clone())
            }),
        );

        let (_d2, new_way) = store();
        let b = observe(
            &new_way,
            ingest_klines(&new_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Ok(bars.clone())
            }),
        );

        assert_eq!(a, b, "the wrapper and the shared ingest disagree about what to write");
        assert_eq!(a.0, Ok(2), "the still-forming tail must still be dropped by BOTH");
        assert_eq!(a.1.len(), 2);

        // ...and the commit key is still spent by both: the SAME window re-runs to zero rows.
        let again_old =
            backfill_klines(&old_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Ok(bars.clone())
            });
        let again_new =
            ingest_klines(&new_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Ok(bars.clone())
            });
        assert_eq!(again_old.unwrap(), 0, "the wrapper's window key was not spent");
        assert_eq!(again_new.unwrap(), 0, "the ingest's window key was not spent");
    }

    /// A fetch failure renders identically through both paths — the wrapper's `String` becomes
    /// `CollectError::Fetch`, which is what it always did, and nothing is written.
    #[test]
    fn a_fetch_failure_renders_identically_through_both_paths() {
        let (_d1, old_way) = store();
        let a = observe(
            &old_way,
            backfill_klines(&old_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Err("boom".to_string())
            }),
        );

        let (_d2, new_way) = store();
        let b = observe(
            &new_way,
            ingest_klines(&new_way, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Err(CollectError::Fetch("boom".to_string()))
            }),
        );

        assert_eq!(a, b);
        assert_eq!(a.0, Err("venue fetch: boom".to_string()));
        assert!(a.1.is_empty(), "a failed fetch writes nothing");
    }

    /// **The one thing the split BUYS**, and the reason it exists rather than being cosmetic: a
    /// `CollectError::Refused` reaches the caller as a refusal instead of being re-rendered as a
    /// venue fetch failure. The wrapper's `String` channel cannot express it at all — a refusal
    /// pushed through it comes back as `venue fetch: …`, which tells an operator to retry a request
    /// that never left the box (`CollectError::Refused`'s own doc argues why that matters).
    #[test]
    fn only_the_ingest_can_carry_a_refusal_as_a_refusal() {
        let (_d1, hist) = store();
        let refused = ingest_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
            Err(CollectError::Refused("cannot express this symbol".to_string()))
        });
        assert!(matches!(refused, Err(CollectError::Refused(_))));
        assert_eq!(refused.unwrap_err().to_string(), "refused: cannot express this symbol");

        let as_a_fetch =
            backfill_klines(&hist, "testvenue", "TESTSYM", "1m", 10, 20, |_, _, _, _| {
                Err("cannot express this symbol".to_string())
            });
        assert_eq!(
            as_a_fetch.unwrap_err().to_string(),
            "venue fetch: cannot express this symbol",
            "the String channel still maps to Fetch — which is exactly why the seam needed the \
             other one"
        );
    }

    // ── BUG B: THE FORMING-BAR REFUSAL REACHES THE ONE-SHOT BINS (0059 Phase 1) ─────────────────
    //
    // The seam refuses an interval the step vocabulary cannot measure. These prove the three
    // things a refusal has to be: it happens BEFORE the fetch, it writes nothing, and it leaves the
    // window's commit key UNSPENT — which is the half that made the old behaviour permanent rather
    // than merely wrong.

    /// A [`crate::kline_source::KlineSource`] whose `fetch` PANICS. The registry path's half of the
    /// proof: if the refusal ever moved below the dispatch, this stops being a red assertion and
    /// becomes a red panic — either way the test fails loudly rather than passing for a new reason.
    struct NeverFetches;

    impl crate::kline_source::KlineSource for NeverFetches {
        fn collector_name(&self) -> &str {
            "testvenue_klines"
        }
        fn venue(&self) -> &str {
            "testvenue"
        }
        fn fetch(
            &self,
            _symbol: &str,
            interval: &str,
            _start_ms: i64,
            _end_ms: i64,
        ) -> Result<Vec<Bar>, CollectError> {
            panic!("the refusal must answer {interval:?} before any venue is asked")
        }
    }

    /// **The bug B fix, on all THREE paths into the store.** `1w`, `1M` and `1mo` are refused by
    /// [`ingest_klines`], by the [`backfill_klines`] wrapper the five one-shot bins reach through
    /// their venue's `backfill_<venue>_klines`, and by
    /// [`crate::kline_source::backfill_kline_source`], which is what the supervisor and the wire
    /// verb dispatch through.
    ///
    /// Named one path at a time rather than folded: the paths are the claim, and a fold over a list
    /// that shrank to one would still pass.
    ///
    /// The fetch closures PANIC. A refusal that happened after the fetch would still write nothing
    /// (the `?` propagates), so "nothing was written" alone cannot tell the two apart — and the
    /// difference is a 24-month paging run against a venue, plus a rate-limit budget, spent to learn
    /// something the process knew from its own argv.
    #[test]
    fn an_unmeasurable_interval_is_refused_before_anything_is_fetched() {
        let (_d, hist) = store();
        for interval in ["1w", "1M", "1mo"] {
            let ingest =
                ingest_klines(&hist, "testvenue", "TESTSYM", interval, 10, 20, |_, _, _, _| {
                    panic!("ingest_klines fetched {interval:?}")
                });
            assert!(
                matches!(ingest, Err(CollectError::Refused(_))),
                "ingest_klines answered {interval:?} with {ingest:?}"
            );

            let wrapper =
                backfill_klines(&hist, "testvenue", "TESTSYM", interval, 10, 20, |_, _, _, _| {
                    panic!("backfill_klines fetched {interval:?}")
                });
            assert!(
                matches!(wrapper, Err(CollectError::Refused(_))),
                "the bins' entry point answered {interval:?} with {wrapper:?}"
            );

            let dispatched = crate::kline_source::backfill_kline_source(
                &hist,
                &NeverFetches,
                "TESTSYM",
                interval,
                10,
                20,
            );
            assert!(
                matches!(dispatched, Err(CollectError::Refused(_))),
                "the registry path answered {interval:?} with {dispatched:?}"
            );

            let bars = hist
                .load_bars("testvenue", "TESTSYM", interval, vike_data::TsRange::all())
                .expect("read the series back");
            assert!(bars.is_empty(), "a refused {interval:?} request wrote rows");
        }
    }

    /// **The half that made bug B PERMANENT: the window's commit key must be left unspent.**
    ///
    /// Observed directly rather than argued from the code path — after the refusal, the SAME key is
    /// offered to `append_bars` and must be accepted. Had the refusal run after the append (or had
    /// the old silent-decline behaviour written the forming bar), this call would return 0 rows and
    /// the series would be uncorrectable for its whole life: `commit_rows` checks `has_commit`
    /// first, and nothing in this workspace retires a single key.
    #[test]
    fn a_refused_window_leaves_its_commit_key_unspent() {
        let (_d, hist) = store();
        let refused = ingest_klines(&hist, "testvenue", "TESTSYM", "1w", 10, 20, |_, _, _, _| {
            panic!("fetched a refused window")
        });
        assert!(matches!(refused, Err(CollectError::Refused(_))));

        let key = commit_key("testvenue", "TESTSYM", "1w", 10, 20);
        let written = hist
            .append_bars("testvenue", "TESTSYM", "1w", &[bar(1_700_000_000_000)], Some(&key))
            .expect("the store takes the window");
        assert_eq!(written, 1, "the refused window had already spent its commit key");
    }

    /// The refusal SAYS what is wrong and what it prevents — the two things an operator needs in
    /// order to act, and the shape `crates/vike-datahub/src/server.rs`'s `backfill_verb` refusal
    /// already had. A refusal that only says "no" sends them to re-run with the same argument.
    #[test]
    fn the_refusal_names_the_step_the_guard_and_the_way_out() {
        let (_d, hist) = store();
        let err = ingest_klines(&hist, "binance", "BTCUSDT", "1M", 10, 20, |_, _, _, _| {
            unreachable!("refused before the fetch")
        })
        .expect_err("1M is refused")
        .to_string();
        assert!(err.contains("refused:"), "it renders as a refusal, not a fetch failure: {err}");
        assert!(err.contains("\"1M\""), "it names the offending step: {err}");
        assert!(err.contains("binance") && err.contains("BTCUSDT"), "it names the request: {err}");
        assert!(err.contains("commit key"), "it names what it prevents: {err}");
        assert!(err.contains("7d"), "it names a step that would work: {err}");
    }

    /// A MEASURABLE step is untouched by the refusal — the anti-vacuity half, and the guarantee that
    /// this change narrowed exactly the three spellings it argued about.
    ///
    /// `7d` is the interesting one: a whole week, outside nothing, and the answer for an operator
    /// who was reaching for `1w`.
    #[test]
    fn a_measurable_step_still_fetches_and_writes() {
        let (_d, hist) = store();
        for interval in ["1m", "4h", "1d", "7d", "30s"] {
            let n = ingest_klines(&hist, "testvenue", "TESTSYM", interval, 10, 20, |_, _, _, _| {
                Ok(vec![bar(1_600_000_000_000)])
            })
            .unwrap_or_else(|e| panic!("{interval} must still ingest: {e}"));
            assert_eq!(n, 1, "{interval} wrote nothing");
        }
    }
}
