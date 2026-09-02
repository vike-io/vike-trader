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
use vike_model::rate_limits::PaceSample;
use vike_model::Bar;

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
/// `end_ms`) — centrally, so binance/bybit/okx all get the still-forming-candle guard from this one
/// pager rather than each venue needing its own.
pub(crate) fn backfill_klines(
    hist: &DataFusionHist,
    venue: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    fetch: impl FnOnce(&str, &str, i64, i64) -> Result<Vec<Bar>, String>,
) -> Result<usize, CollectError> {
    let mut bars = fetch(symbol, interval, start_ms, end_ms).map_err(CollectError::Fetch)?;
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
commit key: re-running the same window writes 0 rows. A still-forming final candle is dropped.

All five arguments are REQUIRED and positional, in this order:
  ROOT_DIR    hist-store root directory (created if absent)
  SYMBOL      venue symbol, e.g. BTCUSDT
  INTERVAL    bar interval, e.g. 1m 5m 1h 1d
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

    #[test]
    fn unparseable_interval_leaves_bars_untouched() {
        // Defensive: an interval the shared vocabulary doesn't recognize must never panic or drop
        // data — the guard just declines to act.
        let mut bars = vec![bar(1_700_000_000_000)];
        drop_forming_tail(&mut bars, "not-an-interval", i64::MAX);
        assert_eq!(bars.len(), 1);
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
}
