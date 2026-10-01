//! Dukascopy tick+bar backfill: ingest already-fetched quotes into the DataFusion hist store
//! (`vike_data::DataFusionHist`) and resample them into bars. No venue code here — the fetch half
//! (docs/decisions/0094) moved into the bridge, `vike_dukascopy::fetch_quotes_range`
//! (itself a port of the Python `dukascopy_source`'s tick pull); this module names no bridge and
//! takes quotes, or a fetch closure that produces them, as a parameter. The ingest/derive half is
//! the `vike_data::HistStore` seam — no store code in vike-dukascopy.
//!
//! Flow: a caller's `fetch` (typically `vike_dukascopy::fetch_quotes_range`) → store as quotes →
//! resample into bars (the parity-tested `vike_model::consolidate_quotes`), composed end to end by
//! [`backfill_quotes_then_bars`] — the datahub's `TickBars` lane, which runs that flow once per
//! settled day-sized chunk of its window and once more for the recent tail the feed may not have
//! finished publishing. ⚠ The composed flow does NOT go through [`ingest_quotes`] any more: a
//! SETTLED chunk's store step (`store_then_resample`'s `StoreMode::Settled` arm) calls
//! `vike_data::DataFusionHist::append_quotes_superseding` directly, so it can atomically supersede
//! a stale provisional entry in the same locked manifest publish. `ingest_quotes` remains a plain,
//! non-superseding `append_quotes` wrapper — kept as a standalone helper (tests use it to simulate
//! a settled fetch directly) rather than as a step this function's own flow calls.

use vike_data::{DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick};

use crate::error::CollectError;

/// Venue tag under which Dukascopy series live in the hist store (`venue=dukascopy` in the tree).
pub const VENUE: &str = "dukascopy";

/// How long ago a chunk's last millisecond must be for the chunk to take the SHARED keys its own
/// bounds give it — the publication margin. Inside it, [`backfill_quotes_then_bars`] keys the chunk
/// by the request instead, and its doc says why.
///
/// ⚠ **A DEFAULT, not a measurement.** The feed's only stated lag is the "T+1" in
/// `crates/bridges/dukascopy/src/data.rs`'s module doc — one day, stated rather than measured — and
/// this is that day plus one more of slack, because the two ways of being wrong are lopsided: too
/// short, and a chunk still being published spends the key every later request would use, for
/// good; too long, and requests only forgo deduplicating against each other for an extra day.
const PUBLICATION_MARGIN_MS: i64 = 2 * MS_PER_DAY;

/// The idempotency guard for a `[start_ms, end_ms]` backfill window: a re-run with the same window
/// is a no-op in the store (batch-level dedup — never per-row value dedup, per the store contract).
/// [`backfill_quotes_then_bars`] spends one per settled chunk of its window, over that chunk's
/// sub-window, and one for a MULTI-chunk window's recent tail, over the whole request. ⚠ A
/// one-chunk window's recent tail is the one case that does NOT spend this key: it spends
/// [`provisional_quote_commit_key`] instead, superseded once the window settles — see
/// [`backfill_quotes_then_bars`]'s own doc.
pub fn quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy:{symbol}:{start_ms}-{end_ms}")
}

/// The PROVISIONAL twin of [`quote_commit_key`]: spent by a recent one-chunk window's early fetch
/// — a namespace `quote_commit_key` never produces, so the two can never collide. Superseded
/// (removed) the moment the same window is committed under its canonical key once settled — and
/// spent by that commit even when no early fetch ever wrote it, so one that arrives after the
/// settled commit writes nothing — see [`vike_data::DataFusionHist::append_quotes_superseding`].
///
/// `pub`, matching [`quote_commit_key`]'s own visibility: integration tests construct this
/// directly to simulate "the window has now settled," the same way they already do for the
/// canonical key — wall-clock settledness cannot be faked forward inside a test.
pub fn provisional_quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy-provisional:{symbol}:{start_ms}-{end_ms}")
}

/// Append already-fetched quotes under `(venue=dukascopy, symbol)`, idempotent by
/// [`quote_commit_key`]. Returns rows written — 0 if the window was already ingested, or if
/// `quotes` is empty, which spends no key (the store records nothing for an empty batch).
pub fn ingest_quotes(
    hist: &DataFusionHist,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    quotes: &[QuoteTick],
) -> Result<usize, CollectError> {
    let key = quote_commit_key(symbol, start_ms, end_ms);
    Ok(hist.append_quotes(VENUE, symbol, quotes, Some(&key))?)
}

/// The datahub's `TickBars` lane: fetch the window's ticks through `fetch`, store them as quotes,
/// then resample the stored quotes into `interval` bars — one CHUNK of the window at a time.
/// Returns BARS written, summed over the chunks, which is what the `Backfill` reply reports and
/// reads back; the quote count is logged per chunk and, for a window of more than one, in total.
/// ⚠ It returns that count only when every chunk was written: a request that had to SKIP a chunk
/// the store refused to supersede returns `Err` instead, naming it (the REFUSED bullet below).
///
/// ⚠ **Chunked, because this runs inside the data daemon.** Fetched whole, a long window held all
/// of its ticks in memory at once — the fetch's answer, then the resample's read-back of the same
/// range — and a long enough window on a major FX pair can take the daemon past its unit's memory
/// ceiling (`deploy/vike-datahub.service`'s `MemoryMax`). So the window is cut on a fixed grid:
/// chunks of `chunk_len` milliseconds — the most whole bars that fit in a UTC day, or one bar when
/// a bar is longer — starting at every multiple of that length from epoch 0. The chunk starting at
/// `c0` covers the sub-window `[max(start_ms, c0), min(end_ms, c0 + chunk - 1)]`, which is fetched,
/// stored under its own [`quote_commit_key`], dropped, and resampled over its own whole buckets
/// before the next chunk is fetched — so the daemon holds one chunk's ticks, never the window's
/// (the recent tail, below, is the one exception, and the margin bounds it). A chunk is a whole
/// number of bars and both grids start at 0, so no bar straddles two chunks: the chunks' whole
/// buckets are exactly the window's.
///
/// - **A window inside one chunk is untouched** by the chunking split — one fetch over its own
///   bounds, never several. A SETTLED one with no prior provisional entry stores the same rows,
///   under the quote key and resample range it always had, as before provisional commits — its
///   parts additionally carry its provisional twins' keys, which a settled commit always spends
///   (the pre-spend, in the RECENT TAIL bullet below); a RECENT one now stores under distinct
///   PROVISIONAL keys instead — see that bullet.
/// - **A longer window spends one quote key per SETTLED chunk** — one whose last millisecond is
///   more than `PUBLICATION_MARGIN_MS` old. A FULL settled chunk's keys are its own bounds
///   whichever request covered it, so overlapping requests store each full settled chunk they share
///   once. A PARTIAL one — a window's ragged first or last chunk — is keyed by its own cut, so two
///   requests that cut one chunk differently still store the ticks they share twice, as any two
///   different windows always did.
/// - **A settled chunk whose ticks are already stored skips the fetch.** Immediately before calling
///   `fetch`, a settled chunk checks its own [`quote_commit_key`] against the store
///   (`vike_data::DataFusionHist::series_has_commit`, one lock-free manifest read, cheaper than the
///   fetch it guards) and moves on without fetching if it is already spent — the same
///   `StoreMode::Settled` chunk whose store step this skip pre-empts.
///   ⚠ **The quote key carries no `interval`, so "the ticks are stored" is not "the bars are."** A
///   chunk ingested at 1m and then requested at 5m has its ticks already and none of its 5m bars.
///   So the skip stops at the fetch: `resample_stored_chunk` then derives THIS interval's bars
///   from the stored ticks, under the chunk's own resample key
///   (`dukascopy-resample:{symbol}:{interval}:…`, which does carry it), with no network call — and
///   writes nothing when that key is spent too, which is the ordinary repeat of an interval. It also
///   gives a retry the bars a failed request never wrote (its ticks stored, its resample not). The
///   tail below is never checked this way: its chunks carry no key of their own, only the
///   request's, so every tail chunk is still fetched.
/// - ⚠ **The RECENT TAIL — every chunk still inside the margin — is keyed by the request instead.**
///   The feed answers an hour it has not published yet exactly as it answers an hour nothing traded
///   in (`crates/bridges/dukascopy/src/data.rs`'s `fetch_hour` turns both 404s into `Ok(None)`), so
///   a chunk fetched too soon comes back short with nothing to say so — and under a shared key that
///   short answer would be final, because every later request covering the chunk would find its key
///   spent. So the tail's chunks are fetched one at a time like any other, then stored as ONE batch
///   under the request's own [`quote_commit_key`] and resampled once, BOTH keyed by the request's
///   own RAW bounds. A later request with other bounds stores the tail again (the ticks the two
///   share, twice), and once the tail's chunks settle they take the shared keys like any other. It
///   is ONE batch because two appends under one key are one append — the store would keep the first
///   recent chunk and drop the rest — and the tail is at most the margin plus one chunk, which
///   bounds what it holds at once.
///   ⚠ RAW bounds, not the whole bars they round to, because that rounding can collapse onto one
///   chunk's own bounds. One day named with date labels is `[D, D + 1 day]` — `--from D --to D` is
///   refused, so `crates/vike-cli/src/cmd/data.rs`'s `fetch_window_ms` sends day D plus the next
///   day's first millisecond — and that 1 ms chunk holds no bar, so the window's whole bars are
///   exactly day D's: a resample keyed by them would spend the very key every later request
///   covering D resamples under. A window of more than one chunk has its two bounds in two
///   different chunks, so its raw bounds are never any one chunk's key.
///   ⚠ A window inside ONE chunk, fetched while still recent, commits under a distinct PROVISIONAL
///   key instead ([`provisional_quote_commit_key`] for the quotes, [`provisional_resample_key`] for
///   the resample) — a namespace the canonical keys never produce, so an early, incomplete fetch can
///   never spend the key a later, complete fetch needs. Once the same window is fetched again after
///   settling, the settled commit atomically supersedes the provisional entry, in the SAME locked
///   manifest publish that seals its own rows (`vike_data::DataFusionHist::
///   append_quotes_superseding` / `resample_quotes_to_bars_superseding`) — so the window's data
///   converges to the complete, correct answer with no permanent loss and no double-counted
///   `volume`, for an early fetch and a later settled fetch of the SAME bounds at the SAME
///   interval. See `docs/superpowers/specs/2026-09-29-provisional-commits-design.md` for the full
///   mechanism and safety argument.
///   ⚠ The ORDER can also be the other way round, and is covered too: a recent request decides
///   "recent" from its one clock read and then spends its fetch time, so a request started after
///   the window crossed the margin can settle it FIRST. The settled commit spends its provisional
///   twin even when there is nothing to supersede — the store's PRE-SPEND
///   (`vike_data::DataFusionHist::append_quotes_superseding`'s stamping paragraph) — so the late
///   recent write finds its provisional key spent and writes nothing, rather than sealing its
///   early ticks and bars beside the settled ones where no later settled pass could remove them.
///   A window settled before that rule shipped carries its canonical key alone, and keeps the race.
///   ⚠ One consequence: a SETTLED one-chunk (or per-chunk) commit can now be REFUSED, in the rare
///   case its provisional entry was folded into a multi-key part by background compaction before
///   the settled fetch arrived — the store refuses a supersede it can no longer perform exactly,
///   instead of risking a double-counted `volume`. `vike_data::DataError::is_supersede_refusal` is
///   what tells that one failure from every other store error, and what happens next is the bullet
///   below: the refusal is one chunk's problem and is handled as one.
/// - ⚠ **A REFUSED supersede skips ITS OWN chunk, and the rest of the request goes on.** The
///   refusal is a fact about one chunk's keys and says nothing about the chunks after it, so that
///   chunk is logged at error level (its bounds, the store's reason, the remedy below), remembered
///   and SKIPPED — every later settled chunk and the recent tail are still fetched, stored and
///   resampled. The request then ends in [`CollectError::SupersedeRefused`], naming EVERY skipped
///   chunk and the bars the others wrote: it never returns `Ok` over a window with a hole in it.
///   Only a refusal is stepped over — a fetch error or any other store error still aborts the
///   request at once (the "A failed chunk" bullet below), and a refusal skipped earlier in a
///   request that then aborts is in the log, not in that error.
///   ⚠ **The refusal is PERMANENT for that exact window.** No key is spent when it fires — the
///   store writes nothing and spends neither the canonical nor the provisional key — so every
///   retry of the identical `(from, to)` refuses identically, with no remedy short of operator
///   intervention. A retry of the WINDOW skips the chunks now written (the skip-before-fetch bullet
///   above) and refuses the refused one again — after fetching it again, when it was the QUOTE side
///   that refused, since no ticks were stored — so it ends in the same error.
///   ⚠ **If the BAR-side supersede call is the one that refuses while the QUOTE-side call already
///   succeeded**, the chunk's ticks are stored — its quote key is spent — and its bars are not, so
///   the skip-before-fetch check (above) skips the fetch on every later retry but stops only there:
///   it re-attempts the bar side (`resample_stored_chunk`), which refuses again and is skipped
///   again, rather than reporting success over bars stuck on the stale provisional data.
///   **The remedy** is to delete the symbol's affected quote and bar series and re-backfill from
///   scratch — `refusal_remedy` spells the command, once, for the error log and the final error
///   alike. ⚠ Once a provisional or stamped-canonical key exists
///   for a series — which is EVERY series a settled pass has written, because a settled commit
///   stamps its newly-sealed parts with BOTH its own key and its provisional twin's, whether or not
///   it superseded anything (see `vike_data::DataFusionHist::append_quotes_superseding`'s doc) —
///   `--produced-by dukascopy:` (WITH the trailing colon) as a deletion filter is itself refused:
///   `dukascopy-provisional:…`/`dukascopy-resample-provisional:…` do not start with the literal
///   `dukascopy:` prefix, so such a part fails `--produced-by`'s all-keys-must-match assertion. The
///   bare `dukascopy` literal (no colon) is what is needed instead — it matches every key this
///   venue mints, canonical and provisional alike.
/// - **A failed chunk** (fetch, ingest or resample — a supersede REFUSAL is the one exception, the
///   bullet above) returns its error at once — a fetch error, in a window of more than one chunk,
///   naming the chunk and the bars already written — and the chunks before it stay written: their
///   keys are spent, so a retry of the same window skips fetching each one that already settled
///   (the bullet above) and re-fetches only from the chunk that failed onward. The recent tail is
///   stored only after its last fetch and is never skipped this way, so a failure inside it
///   re-fetches the whole tail on retry and writes none of it until every chunk in it succeeds.
///
/// `fetch(symbol, from, to)` must answer the ticks inside `[from, to]`, as
/// `vike_dukascopy::fetch_quotes_range` does: whatever each chunk's call returns is stored, so a
/// fetch that ignored its bounds would store its answer once per chunk.
///
/// ⚠ **Every fetched tick is stored, but only WHOLE bars are resampled.** A bar is keyed at its
/// bucket's START, so a window beginning inside a bucket would store a bar built from only the tail
/// of that bucket's ticks under the whole bar's key — and `end_ms` usually cuts the last bucket the
/// same way. The store dedups by commit key, never by row, so such a bar could not be corrected:
/// re-running the window is a no-op, and a wider window's resample writes its whole bar BESIDE the
/// partial one. So each resample — a settled chunk's, or the recent tail's — covers only the
/// buckets lying wholly inside what it spans (`whole_buckets`), which, chunk edges being bar edges,
/// trims only at the window's own two ends; a span holding none writes no bar and spends no
/// resample key. The quotes are stored untrimmed, so the edge ticks are there for a wider window's
/// resample to find.
///
/// An `interval` with no positive width — `0m`, or anything `vike_model::time::interval_ms` cannot
/// read — is REFUSED before the fetch: a zero-width bucket is no bar, and `consolidate_quotes`'
/// `rem_euclid` would divide by it.
pub fn backfill_quotes_then_bars(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    mut fetch: impl FnMut(&str, i64, i64) -> Result<Vec<QuoteTick>, String>,
) -> Result<usize, CollectError> {
    let Some(step) = vike_model::time::interval_ms(interval).filter(|&step| step > 0) else {
        return Err(CollectError::Refused(format!(
            "dukascopy {symbol}: interval {interval:?} has no positive bar width \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, and a zero count is \
             a zero-width bar). Nothing was fetched and no commit key was spent."
        )));
    };
    let chunk = chunk_len(step);
    let multi = chunk_windows(start_ms, end_ms, chunk).nth(1).is_some();
    // ONE clock read per request, so every chunk of it is judged against the same instant — the
    // wall clock, read the way `crate::klines::ingest_klines` reads it for its forming-bar guard.
    let now_ms = vike_model::now_ms();
    let (mut chunks, mut quote_rows_total, mut bars_total) = (0usize, 0usize, 0usize);
    // The recent tail, once one starts: its first millisecond, and every tick fetched for it.
    let mut tail: Option<(i64, Vec<QuoteTick>)> = None;
    // The settled chunks the store REFUSED to supersede, as `(from, to)`: skipped, so the rest of
    // the window still runs, and named in the error the request ends in.
    let mut refused: Vec<(i64, i64)> = Vec::new();
    for (from, to) in chunk_windows(start_ms, end_ms, chunk) {
        chunks += 1;
        let is_settled = settled(to, now_ms);
        if is_settled {
            let key = quote_commit_key(symbol, from, to);
            let id = SeriesId::per_symbol("quote", VENUE, symbol, None);
            if hist.series_has_commit(&id, &key)? {
                // The chunk's TICKS are already stored, so they are never fetched again. What this
                // INTERVAL may still lack is its bars, and those derive from the stored ticks with no
                // network call: a chunk already resampled at this interval writes nothing. This is
                // the bar-side supersede too, so it can be the refusal — skipped like any other.
                match resample_stored_chunk(hist, symbol, interval, step, (from, to)) {
                    Ok(bars) => bars_total += bars,
                    Err(e) => skip_if_refused(e, symbol, interval, (from, to), &mut refused)?,
                }
                continue;
            }
        }
        let quotes = fetch(symbol, from, to).map_err(|e| {
            // One chunk of several names itself and what is already written, which stays written.
            // A one-chunk window's error is the fetch's own text, as it always was.
            CollectError::Fetch(if multi {
                format!(
                    "chunk [{from}, {to}] of [{start_ms}, {end_ms}] failed after {bars_total} \
                     {interval} bars were written: {e}"
                )
            } else {
                e
            })
        })?;
        if !is_settled {
            // This chunk and every later one, for ONE store under the request's own bounds.
            let (_, gathered) = tail.get_or_insert_with(|| (from, Vec::new()));
            gathered.extend(quotes);
            continue;
        }
        // The one store step that can be REFUSED (it supersedes): the refusal is this chunk's alone,
        // so it is skipped and the next chunk — and the tail — still run. Anything else aborts.
        match store_then_resample(
            hist,
            symbol,
            interval,
            step,
            (from, to),
            StoreMode::Settled,
            quotes,
        ) {
            Ok((quote_rows, bars)) => {
                quote_rows_total += quote_rows;
                bars_total += bars;
            }
            Err(e) => skip_if_refused(e, symbol, interval, (from, to), &mut refused)?,
        }
    }
    if let Some((tail_start, quotes)) = tail {
        if multi {
            tracing::info!(
                "dukascopy {symbol} [{tail_start}, {end_ms}]: within the publication margin — \
                 stored as one batch under this request's own bounds [{start_ms}, {end_ms}]"
            );
        }
        // A one-chunk window's tail IS the window: still recent, it now keys under the PROVISIONAL
        // formatters instead of the canonical ones (see the doc). A longer window's tail is keyed
        // by the request's raw bounds, for both the quotes and the resample, exactly as before.
        let mode = if multi {
            StoreMode::RecentTail { request_start: start_ms, request_end: end_ms }
        } else {
            StoreMode::RecentOneChunk
        };
        let (quote_rows, bars) =
            store_then_resample(hist, symbol, interval, step, (tail_start, end_ms), mode, quotes)?;
        quote_rows_total += quote_rows;
        bars_total += bars;
    }
    // A one-chunk window's line already IS its total — the line such a request has always logged —
    // so the sum is logged only when there is more than one chunk to sum.
    if multi {
        let skipped = if refused.is_empty() {
            String::new()
        } else {
            format!(" — {} SKIPPED (refused), not counted above", refused.len())
        };
        tracing::info!(
            "dukascopy {symbol} [{start_ms}, {end_ms}]: {quote_rows_total} quote rows, \
             {bars_total} {interval} bars over {chunks} chunks{skipped}"
        );
    }
    // Never `Ok` over a window with a hole in it: the count above is what the caller reads as "this
    // window is in the store", and a skipped chunk is not.
    if !refused.is_empty() {
        return Err(CollectError::SupersedeRefused(refusal_report(
            symbol,
            interval,
            (start_ms, end_ms),
            (chunks, bars_total),
            &refused,
        )));
    }
    Ok(bars_total)
}

/// A settled chunk's store step failed with `error`. A supersede REFUSAL is that chunk's own
/// permanent problem — see [`backfill_quotes_then_bars`]'s REFUSED bullet for why the rest of the
/// request need not share it — so it is logged at error level with the remedy, remembered in
/// `refused`, and stepped over (`Ok`). Anything else is not one chunk's fault (the disk, the store
/// or a decode is failing, and the next chunk would meet it too), so it is handed back to abort the
/// request exactly as it always did (`Err`).
///
/// ⚠ The refusal is told from every other error by `DataError::is_supersede_refusal` and by nothing
/// else: only a `Data` error can be one, and a `Data` error that is not one must still abort.
fn skip_if_refused(
    error: CollectError,
    symbol: &str,
    interval: &str,
    (from, to): (i64, i64),
    refused: &mut Vec<(i64, i64)>,
) -> Result<(), CollectError> {
    match error {
        CollectError::Data(store) if store.is_supersede_refusal() => {
            tracing::error!(
                "dukascopy {symbol} [{from}, {to}]: chunk SKIPPED — {store}. {}",
                refusal_remedy(symbol, interval)
            );
            refused.push((from, to));
            Ok(())
        }
        other => Err(other),
    }
}

/// What an operator does about a chunk the store refused to supersede, spelled ONCE so the error
/// logged as the chunk is skipped and the error the request ends in cannot drift apart.
///
/// ⚠ The command is the one `backfill_quotes_then_bars`'s doc gives in prose, and its
/// `--produced-by dukascopy` is the bare literal for the reason that doc gives: a series holding a
/// provisional or stamped-canonical key refuses `dukascopy:` with the colon.
fn refusal_remedy(symbol: &str, interval: &str) -> String {
    format!(
        "The refusal is PERMANENT for that window — retrying it refuses the same way — and only an \
         operator can clear it: delete this symbol's dukascopy quote series and its {interval} bar \
         series (`vike-cli data hist rm --kind quote --venue dukascopy --symbol {symbol}` and the \
         same with `--kind bar --interval {interval}`, each asserting `--produced-by dukascopy` — \
         the bare literal, since `dukascopy:` with the colon is itself refused on a series holding \
         a provisional key) and re-backfill."
    )
}

/// The error a request ends in when [`skip_if_refused`] skipped chunks: EVERY skipped chunk by its
/// bounds, what became of the rest, and the remedy — because this is all a caller that only sees
/// the reply ever learns, the log lines having been written on the daemon's box.
///
/// It says "wrote no bars for them" and nothing about their ticks, because both halves of the
/// supersede can refuse and only one leaves ticks behind: a QUOTE-side refusal writes nothing at
/// all, a BAR-side one leaves the chunk's ticks stored and its bars not. "No bars" is true of both.
fn refusal_report(
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    (chunks, bars_total): (usize, usize),
    refused: &[(i64, i64)],
) -> String {
    let skipped =
        refused.iter().map(|(from, to)| format!("[{from}, {to}]")).collect::<Vec<_>>().join(", ");
    format!(
        "dukascopy {symbol} [{start_ms}, {end_ms}] {interval}: the store REFUSED to supersede a \
         provisional entry it could no longer remove exactly (folded into a multi-key part, e.g. by \
         background compaction), so those chunks were SKIPPED and this request wrote no {interval} \
         bars for them — chunks skipped, {} of {chunks}: {skipped}. The other chunks ran to their \
         end, writing {bars_total} {interval} bars. {}",
        refused.len(),
        refusal_remedy(symbol, interval),
    )
}

/// Which keys `store_then_resample` uses, replacing the previous `request: Option<(i64, i64)>`
/// two-way split with the three real cases this function serves.
enum StoreMode {
    /// A settled chunk (single chunk of a longer window, OR a settled one-chunk window): keys by
    /// its own `(from, to)` bounds, and ALWAYS supersedes that same window's provisional key —
    /// removing the provisional entry when there is one, and pre-spending the key when there is
    /// not, so a recent fetch of the window that lands later writes nothing.
    Settled,
    /// A multi-chunk window's recent tail: keys by the whole REQUEST's raw bounds — unchanged from
    /// today, no provisional/supersede involved (already correct since #2310 Unit B).
    RecentTail { request_start: i64, request_end: i64 },
    /// A one-chunk window that is itself still recent: keys by the PROVISIONAL formatters instead
    /// of the canonical ones. No supersede — nothing to supersede on a first early fetch.
    RecentOneChunk,
}

/// Store one fetched batch and resample it: the unit a settled chunk and the recent tail share.
/// The whole buckets of `[from, to]` are what gets resampled, whatever keys it. `mode` decides
/// which keys are spent, and whether a settled write also supersedes a stale provisional entry —
/// see [`StoreMode`]. The line a chunk has always logged is logged. Returns `(quote rows, bars)`.
fn store_then_resample(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    step: i64,
    (from, to): (i64, i64),
    mode: StoreMode,
    quotes: Vec<QuoteTick>,
) -> Result<(usize, usize), CollectError> {
    let quote_rows = match mode {
        StoreMode::Settled => {
            let key = quote_commit_key(symbol, from, to);
            let provisional = provisional_quote_commit_key(symbol, from, to);
            hist.append_quotes_superseding(VENUE, symbol, &quotes, Some(&key), Some(&provisional))?
        }
        StoreMode::RecentTail { request_start, request_end } => {
            let key = quote_commit_key(symbol, request_start, request_end);
            hist.append_quotes(VENUE, symbol, &quotes, Some(&key))?
        }
        StoreMode::RecentOneChunk => {
            let key = provisional_quote_commit_key(symbol, from, to);
            hist.append_quotes(VENUE, symbol, &quotes, Some(&key))?
        }
    };
    // The resample re-reads the span from the store, so the fetched ticks are dead weight from
    // here: freeing them holds the data daemon to one copy of them rather than two.
    drop(quotes);
    let Some((first, last)) = whole_buckets(from, to, step) else {
        tracing::info!(
            "dukascopy {symbol} [{from}, {to}]: {quote_rows} quote rows, no whole \
             {interval} bar inside the window — nothing resampled"
        );
        return Ok((quote_rows, 0));
    };
    let range = TsRange::of(first, last);
    let bars = match mode {
        StoreMode::Settled => resample_settled(hist, symbol, interval, range)?,
        StoreMode::RecentTail { request_start, request_end } => {
            let key_range = TsRange::of(request_start, request_end);
            resample_keyed(hist, symbol, interval, range, key_range)?
        }
        StoreMode::RecentOneChunk => {
            let key = provisional_resample_key(symbol, interval, range);
            hist.resample_quotes_to_bars(VENUE, symbol, interval, range, Some(&key))?
        }
    };
    tracing::info!(
        "dukascopy {symbol} [{from}, {to}]: {quote_rows} quote rows, {bars} {interval} bars \
         over [{first}, {last}]"
    );
    Ok((quote_rows, bars))
}

/// The bar half of a SETTLED chunk: resample `range` — the chunk's whole buckets — under the
/// chunk's own canonical resample key (`resample_commit_key`'s `key_range == range` case: a settled
/// chunk has no separate request), atomically superseding that same window's provisional bars when
/// there are any, and pre-spending their key when there are none. Shared by the store step ([`store_then_resample`]'s `Settled` arm) and by
/// [`resample_stored_chunk`], so the two can only ever spend one spelling of the key. Returns bars
/// written — or the store's REFUSAL to supersede, which both callers hand back unchanged for
/// [`backfill_quotes_then_bars`]'s loop to skip (`skip_if_refused`) rather than abort on.
fn resample_settled(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    let key = resample_commit_key(symbol, interval, range);
    let provisional = provisional_resample_key(symbol, interval, range);
    Ok(hist.resample_quotes_to_bars_superseding(
        VENUE,
        symbol,
        interval,
        range,
        Some(&key),
        Some(&provisional),
    )?)
}

/// A SETTLED chunk whose ticks are already stored — its quote key is spent — needs no fetch, but
/// may still lack THIS interval's bars: the quote key carries no `interval`, so a chunk ingested at
/// 1m looks spent to a request for 5m. The bars derive from the stored ticks, so this resamples them
/// under the chunk's resample key unless that key is spent too, which is the ordinary repeat of an
/// interval and writes nothing. Returns bars written — `0` when the chunk is already resampled at
/// this interval or holds no whole bucket of it.
///
/// It also gives a retry the bars a failed request never wrote: a request that stored a chunk's ticks
/// and then failed before (or inside) its resample left the quote key spent and the resample key
/// not, and a check on the quote key alone skipped that chunk for good.
fn resample_stored_chunk(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    step: i64,
    (from, to): (i64, i64),
) -> Result<usize, CollectError> {
    let Some((first, last)) = whole_buckets(from, to, step) else {
        tracing::info!(
            "dukascopy {symbol} [{from}, {to}]: already ingested (commit key spent) — skipping the \
             fetch; no whole {interval} bar inside the chunk"
        );
        return Ok(0);
    };
    let range = TsRange::of(first, last);
    let bar_series = SeriesId::per_symbol("bar", VENUE, symbol, Some(interval.to_string()));
    if hist.series_has_commit(&bar_series, &resample_commit_key(symbol, interval, range))? {
        tracing::info!(
            "dukascopy {symbol} [{from}, {to}]: already ingested and resampled at {interval} \
             (both commit keys spent) — skipping the fetch"
        );
        return Ok(0);
    }
    let bars = resample_settled(hist, symbol, interval, range)?;
    tracing::info!(
        "dukascopy {symbol} [{from}, {to}]: ticks already ingested (commit key spent) — {bars} \
         {interval} bars resampled from the store over [{first}, {last}], no fetch"
    );
    Ok(bars)
}

/// Whether a chunk whose fetched cut ends at `to` is SETTLED at `now_ms` — its last millisecond
/// more than [`PUBLICATION_MARGIN_MS`] old — and so takes the shared keys its own bounds give it.
fn settled(to: i64, now_ms: i64) -> bool {
    to < now_ms.saturating_sub(PUBLICATION_MARGIN_MS)
}

/// The chunk [`backfill_quotes_then_bars`] cuts a window into, in milliseconds: the most whole
/// `step` bars that fit in a UTC day ([`MS_PER_DAY`]), or one bar when a bar is longer than a day.
/// Always a whole number of bars, which is what puts every chunk edge on a bar edge. `step > 0`, so
/// the product is at most a day and cannot overflow.
fn chunk_len(step: i64) -> i64 {
    step.max((MS_PER_DAY / step) * step)
}

/// `[start_ms, end_ms]` cut at every multiple of `chunk` inside it (the grid anchored at epoch 0),
/// as inclusive sub-windows in order — lazily, so a window of any length costs one pair at a time.
/// A window inside one chunk comes back whole, as the one pair `(start_ms, end_ms)` — an inverted
/// window too. The chunk's END is computed rather than its start, so a window at `i64::MIN` never
/// needs the chunk start below it; `rem_euclid` rather than `%`, so a pre-1970 stamp finds its
/// chunk like any other; saturating, so the last chunk below `i64::MAX` ends at `end_ms` instead of
/// overflowing. `chunk > 0`.
fn chunk_windows(start_ms: i64, end_ms: i64, chunk: i64) -> impl Iterator<Item = (i64, i64)> {
    let mut next = Some(start_ms);
    std::iter::from_fn(move || {
        let from = next?;
        let to = from.saturating_add(chunk - 1 - from.rem_euclid(chunk)).min(end_ms);
        // `to < end_ms` is what keeps `to + 1` from overflowing: a window reaching `i64::MAX`
        // stops here instead.
        next = if to < end_ms { Some(to + 1) } else { None };
        Some((from, to))
    })
}

/// The widest run of WHOLE `step` buckets inside the inclusive window `[start_ms, end_ms]`, as an
/// inclusive `(first, last)` — `None` when not one bucket fits. The start rounds UP onto the `step`
/// grid and `end_ms + 1` rounds DOWN onto it (the exclusive end of the last whole bucket).
/// `rem_euclid` rather than `%`, so a pre-1970 stamp rounds the same way as any other; checked
/// arithmetic, so a window at the edge of `i64` answers `None` instead of overflowing. `step > 0`.
fn whole_buckets(start_ms: i64, end_ms: i64, step: i64) -> Option<(i64, i64)> {
    let first = match start_ms.rem_euclid(step) {
        0 => start_ms,
        rem => start_ms.checked_add(step - rem)?,
    };
    let end_excl = end_ms.saturating_add(1);
    let end_excl = end_excl.checked_sub(end_excl.rem_euclid(step))?;
    if end_excl > first { Some((first, end_excl - 1)) } else { None }
}

/// Resample stored Dukascopy quotes → OHLCV bars at `interval` for `symbol` over `range`, writing
/// them back into the store. Thin wrapper over `HistStore::resample_quotes_to_bars`, which reads the
/// stored quote slice and folds it with the parity-tested `vike_model::consolidate_quotes`.
/// Idempotent: the derived batch is keyed by `(symbol, interval, range)` so re-running is a no-op.
/// Returns bars written.
pub fn resample_and_store_bars(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    resample_keyed(hist, symbol, interval, range, range)
}

/// `pub` for the same reason [`provisional_quote_commit_key`] is.
pub fn provisional_resample_key(symbol: &str, interval: &str, key_range: TsRange) -> String {
    let bound = |b: Option<i64>| b.map(|v| v.to_string()).unwrap_or_else(|| "*".to_string());
    format!(
        "dukascopy-resample-provisional:{symbol}:{interval}:{}-{}",
        bound(key_range.start),
        bound(key_range.end),
    )
}

/// The canonical resample commit key, shared by every caller so the template can only ever be
/// spelled once: `resample_keyed` (whose `key_range` may differ from the `range` it resamples — a
/// multi-chunk tail's raw request bounds) and `store_then_resample`'s `Settled` arm (whose
/// `key_range` is always its own `range`, there being no separate request there). ⚠
/// `crates/vike-data/src/store_kind.rs`'s `bar`-kind `dukascopy-resample:` row checks this literal
/// verbatim, so its characters must stay exactly this.
///
/// `pub` for the same reason [`quote_commit_key`] is: integration tests construct this directly to
/// simulate a settled pass's bar-side supersede call, the same way they already do for the quote
/// side — wall-clock settledness cannot be faked forward inside a test.
pub fn resample_commit_key(symbol: &str, interval: &str, key_range: TsRange) -> String {
    let bound = |b: Option<i64>| b.map(|v| v.to_string()).unwrap_or_else(|| "*".to_string());
    format!(
        "dukascopy-resample:{symbol}:{interval}:{}-{}",
        bound(key_range.start),
        bound(key_range.end),
    )
}

/// [`resample_and_store_bars`], keyed by `key_range` instead of by the `range` it resamples. A
/// multi-chunk window's recent tail is the one caller that needs the two apart: it resamples its
/// own whole buckets under the request's raw bounds, which lie in two different chunks and so can
/// never be the key a later request's chunk resamples under.
fn resample_keyed(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
    key_range: TsRange,
) -> Result<usize, CollectError> {
    let key = resample_commit_key(symbol, interval, key_range);
    Ok(hist.resample_quotes_to_bars(VENUE, symbol, interval, range, Some(&key))?)
}

#[path = "dukascopy_tests.rs"]
#[cfg(test)]
mod dukascopy_tests;
