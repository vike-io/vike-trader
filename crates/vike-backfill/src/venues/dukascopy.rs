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
//! day-sized chunk of its window: a settled chunk under the keys its own bounds give it, and a
//! chunk the feed may not have finished publishing under their PROVISIONAL twins, which the
//! chunk's settled commit later supersedes. ⚠ The composed flow does NOT go through
//! [`ingest_quotes`] any more: a SETTLED chunk's store step (`store_then_resample`'s
//! `StoreMode::Settled` arm) calls `vike_data::DataFusionHist::append_quotes_superseding` directly,
//! so it can atomically supersede a stale provisional entry in the same locked manifest publish.
//! `ingest_quotes` remains a plain, non-superseding `append_quotes` wrapper — kept as a standalone
//! helper (tests use it to simulate a settled fetch directly) rather than as a step this function's
//! own flow calls.
//!
//! ## The archive half — the import lane's store step (docs/decisions/0100)
//!
//! The datahub's archive import reads Dukascopy's DAILY `.bi5` files from a folder the operator
//! filled and stores each UTC day here, in the same `(venue=dukascopy, symbol)` series the HTTP lane
//! above writes (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`). The decode is
//! the bridge's (`vike_dukascopy::decode_daily_file`) and the walk is the datahub's; this module
//! owns what the two LANES must agree on, because the store dedups by commit key and never by row —
//! a tick both lanes stored is stored twice, and inflates `volume` in every bar resampled over it:
//!
//! - **One key per day, in a namespace of its own** — [`archive_quote_commit_key`] over the day's
//!   bounds, whose `crates/vike-data/src/store/store_kind.rs` row names a `source` of its own, so
//!   `source_for_key` keeps archive days apart from fetched ones.
//! - **One owner per UTC day.** [`DayOwners::classify`] is the design's §4.2 table for the import:
//!   it skips a day the HTTP lane already holds, supersedes an exact-day provisional key, and
//!   REFUSES a day any other Dukascopy quote key meets. [`backfill_quotes_then_bars`] is the other
//!   direction: it never downloads a day the archive holds, and fetches only the rest of a chunk
//!   that touches one (its ARCHIVE bullet).
//! - **One lock per `(store root, symbol)`** — [`ArchiveImport`] holds it for its whole request,
//!   the HTTP lane for each chunk's store step and never across a download, so neither lane's
//!   "is this day free?" can be answered stale by the time it writes.

use std::collections::BTreeSet;

use vike_data::StoreKindId;
use vike_data::{DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick};

use crate::error::CollectError;

#[cfg(doc)]
use archive::ArchiveImport;
use chunks::{
    archive_days_touched, chunk_len, chunk_windows, clip_to, settled, unowned_pieces, whole_buckets,
};
use day_owner::DayOwnerLock;
#[cfg(doc)]
use day_owner::DayOwners;
use report::{refusal_report, skip_if_refused, stopped_report};

pub mod archive;
mod chunks;
pub mod day_owner;
mod report;

/// Venue tag under which Dukascopy series live in the hist store (`venue=dukascopy` in the tree).
pub const VENUE: &str = "dukascopy";

/// How long ago a chunk's last millisecond must be for the chunk to take the SHARED keys its own
/// bounds give it — the publication margin. Inside it, [`backfill_quotes_then_bars`] stores the
/// chunk under those keys' PROVISIONAL twins instead, and its doc says why.
///
/// ⚠ **A DEFAULT, not a measurement.** The feed's only stated lag is the "T+1" in
/// `crates/bridges/dukascopy/src/data.rs`'s module doc — one day, stated rather than measured — and
/// this is that day plus one more of slack, because the two ways of being wrong are lopsided: too
/// short, and a chunk still being published spends the key every later request would use, for
/// good; too long, and a chunk only waits an extra day under its provisional keys before its
/// settled commit replaces it.
const PUBLICATION_MARGIN_MS: i64 = 2 * MS_PER_DAY;

/// The idempotency guard for a `[start_ms, end_ms]` backfill window: a re-run with the same window
/// is a no-op in the store (batch-level dedup — never per-row value dedup, per the store contract).
/// [`backfill_quotes_then_bars`] spends one per SETTLED chunk of its window, over that chunk's
/// sub-window. ⚠ A chunk still inside the publication margin does NOT spend this key: it spends
/// [`provisional_quote_commit_key`] over the same sub-window instead, superseded once the chunk
/// settles — see [`backfill_quotes_then_bars`]'s own doc.
pub fn quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy:{symbol}:{start_ms}-{end_ms}")
}

/// The PROVISIONAL twin of [`quote_commit_key`]: spent by a recent chunk's early fetch, over that
/// chunk's own sub-window — a namespace `quote_commit_key` never produces, so the two can never
/// collide. Superseded (removed) the moment the same sub-window is committed under its canonical
/// key once settled — and spent by that commit even when no early fetch ever wrote it, so one that
/// arrives after the settled commit writes nothing — see
/// [`vike_data::DataFusionHist::append_quotes_superseding`].
///
/// `pub`, matching [`quote_commit_key`]'s own visibility: integration tests construct this
/// directly to simulate "the window has now settled," the same way they already do for the
/// canonical key — wall-clock settledness cannot be faked forward inside a test.
pub fn provisional_quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy-provisional:{symbol}:{start_ms}-{end_ms}")
}

/// The ARCHIVE lane's day key: `[start_ms, end_ms]` is one UTC day, `[D, D + 86_399_999]`, and an
/// imported day is stored under exactly one — see the module doc's archive half and
/// docs/decisions/0100's third verdict. A namespace neither formatter above produces, so the two
/// lanes' keys can never collide, and `crates/vike-data/src/store/store_kind.rs`'s `source_for_key`
/// answers `dukascopy-archive` for it where it answers `dukascopy` for theirs.
///
/// It is [`quote_commit_key`]'s exact shape on purpose: [`DayOwners`] reads the `{start}-{end}`
/// suffix of all three Dukascopy quote templates as ONE grammar, deriving each one's head from its
/// formatter rather than spelling it again. ⚠ `crates/vike-data/src/store/store_kind.rs`'s `quote`-kind
/// `dukascopy-archive:` row checks this literal verbatim, so its characters must stay exactly this.
pub fn archive_quote_commit_key(symbol: &str, start_ms: i64, end_ms: i64) -> String {
    format!("dukascopy-archive:{symbol}:{start_ms}-{end_ms}")
}

/// The `kind=quote` series both Dukascopy lanes write for `symbol` — spelled once, so the key reads
/// and the archive's classification can never ask about a different series than the writes land in.
fn quote_series(symbol: &str) -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Quote, VENUE, symbol, None)
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
/// stored under its own keys — canonical once it has settled, provisional while it is recent (the
/// RECENT CHUNK bullet below) — dropped, and resampled over its own whole buckets before the next
/// chunk is fetched, so the daemon holds one chunk's ticks, never the window's, recent chunks
/// included. A chunk is a whole number of bars and both grids start at 0, so no bar straddles two
/// chunks: the chunks' whole buckets are exactly the window's.
///
/// - **A window inside one chunk is untouched** by the chunking split — one fetch over its own
///   bounds, never several. A SETTLED one with no prior provisional entry stores the same rows,
///   under the quote key and resample range it always had, as before provisional commits — its
///   parts additionally carry its provisional twins' keys, which a settled commit always spends
///   (the pre-spend, in the RECENT CHUNK bullet below); a RECENT one stores under distinct
///   PROVISIONAL keys instead, like every recent chunk — see that bullet.
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
///   gives a retry the bars a failed request never wrote (its ticks stored, its resample not).
///   ⚠ **A RECENT chunk skips its fetch the same way, against its PROVISIONAL quote key.** A spent
///   one means an earlier request cut this chunk exactly the same way and stored it, and the store
///   would discard a second answer under the spent key anyway — so the download is skipped, and
///   `resample_stored_recent_chunk` derives THIS interval's bars from the stored ticks under the
///   chunk's PROVISIONAL resample key, exactly as the store step's own resample would have written
///   them (so they stay supersedable), writing nothing when that key is spent too. It changes no
///   stored byte; it removes the download alone.
/// - ⚠ **A RECENT CHUNK — one still inside the margin — is stored under PROVISIONAL keys.** The
///   feed answers an hour it has not published yet exactly as it answers an hour nothing traded
///   in (`crates/bridges/dukascopy/src/data.rs`'s `fetch_hour` turns both 404s into `Ok(None)`), so
///   a chunk fetched too soon comes back short with nothing to say so — and under the chunk's
///   canonical keys that short answer would be final, because every later request covering the
///   chunk would find them spent. So a recent chunk is stored and resampled the way a settled one
///   is, immediately after its own fetch, but under [`provisional_quote_commit_key`] of its own
///   bounds for the ticks and [`provisional_resample_key`] of its own whole buckets for the bars —
///   a namespace the canonical keys never produce, so an early, incomplete fetch can never spend
///   the key a later, complete fetch needs. Once the chunk has settled and is fetched again, its
///   settled commit atomically supersedes the provisional entry, in the SAME locked manifest
///   publish that seals its own rows (`vike_data::DataFusionHist::append_quotes_superseding` /
///   `resample_quotes_to_bars_superseding`) — so the chunk's data converges to the complete,
///   correct answer with no permanent loss and no double-counted `volume`. The mechanism and its
///   safety argument are `docs/superpowers/specs/2026-09-29-provisional-commits-design.md`'s; why
///   EVERY recent chunk takes it, and not a one-chunk window's alone, is #9 of
///   `docs/superpowers/specs/2026-09-30-provisional-commits-followups-design.md`: a longer window's
///   recent chunks used to be stored as ONE batch keyed by the request's own bounds, which no
///   settled commit ever superseded, so once their days settled and were fetched again their ticks
///   were stored twice and the settled bars' `volume` counted them twice.
///   ⚠ **The supersede is EXACT, so it closes a chunk only when the settled pass cuts it the same
///   way.** The grid is anchored at epoch 0, so a FULL chunk is the same `(from, to)` whichever
///   request covers it — for an interval that divides a day (1m, 5m, 15m, 1h, 4h, 1d) that is the
///   UTC day, and a window given as date labels holds only full chunks plus a one-millisecond last
///   one that holds no bar. A PARTIAL chunk — a ragged first or last one, which the `[today, now]`
///   end of a `--days N` or epoch-ms window always is — keeps a provisional key cut to its own
///   bounds, which no settled commit matches: once its day settles and is fetched again, its ticks
///   and bars still sit beside the full day's. That residual is DECLARED: closing it needs a
///   supersede by CONTAINMENT, a different primitive with its own design (the follow-ups design's
///   owner question 4). Keying a partial chunk by its whole grid cell instead was weighed and
///   rejected: the first fetch of a day would freeze all of it, and a later request asking for more
///   of that day would write nothing.
///   ⚠ **A full recent chunk is written ONCE, at its first fetch.** Its provisional key is spent
///   from then on, so every later request covering it skips it (the RECENT skip above) and it stays
///   at that first snapshot until its settled commit replaces it — the freshness the old
///   request-keyed batch bought by storing the same ticks again under every new pair of bounds.
///   ⚠ The ORDER can also be the other way round, and is covered too: a recent request decides
///   "recent" from its one clock read and then spends its fetch time, so a request started after
///   the chunk crossed the margin can settle it FIRST. The settled commit spends its provisional
///   twin even when there is nothing to supersede — the store's PRE-SPEND
///   (`vike_data::DataFusionHist::append_quotes_superseding`'s stamping paragraph) — so the late
///   recent request finds its provisional key spent, at its skip or at its write, and writes
///   nothing, rather than sealing its early ticks and bars beside the settled ones where no later
///   settled pass could remove them. A chunk settled before that rule shipped carries its canonical
///   key alone, and keeps the race.
///   ⚠ One consequence: a SETTLED commit can now be REFUSED, in the rare case its provisional entry
///   was folded into a multi-key part by background compaction before the settled fetch arrived —
///   the store refuses a supersede it can no longer perform exactly, instead of risking a
///   double-counted `volume`. Every recent chunk leaves a provisional part, but a repeat of the
///   same cut adds none (its key is spent), so a fold takes several DIFFERENT cuts of one day
///   before it settles (`vike_data::CompactionConfig`'s default `min_parts`).
///   `vike_data::DataError::is_supersede_refusal` is what tells that one failure from every other
///   store error, and what happens next is the bullet below: the refusal is one chunk's problem and
///   is handled as one.
/// - ⚠ **A REFUSED supersede skips ITS OWN chunk, and the rest of the request goes on.** The
///   refusal is a fact about one chunk's keys and says nothing about the chunks after it, so that
///   chunk is logged at error level (its bounds, the store's reason, the remedy below), remembered
///   and SKIPPED — every later chunk, settled or recent, is still fetched, stored and resampled.
///   The request then ends in [`CollectError::SupersedeRefused`], naming EVERY skipped
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
///   keys are spent, so a retry of the same window skips fetching each one already written (the
///   skips above) and re-fetches only from the chunk that failed onward. A recent chunk is no
///   exception: it is stored as soon as it is fetched, so the recent chunks before a failure stay
///   written too — provisionally, which is harmless — and a retry that cuts them the same way skips
///   them.
/// - ⚠ **THE ARCHIVE'S DAYS ARE NEVER FETCHED** (docs/decisions/0100: every UTC day of a series has
///   ONE owner). Before its own skip check, every chunk asks which UTC days it touches already hold
///   their [`archive_quote_commit_key`] — at most two `series_has_commit` reads for any interval up
///   to a day, since a chunk is then at most a day long, and one `series_commits` read for a longer
///   one. Those days are cut out of the chunk, and what is left is the chunk's PIECES:
///   - **All of it held** (the design's change A): no download at all — the chunk's bars derive from
///     the stored ticks exactly as the skip above derives them, except that a RAGGED chunk whose whole
///     grid cell the archive holds resamples that whole cell, so it lands on the import's own bar
///     key instead of writing part of the day's bars twice (`held_span` carries the argument). That
///     is also what gives a request over imported days its bars at ANY interval, offline.
///   - **Part of it held** (change B — a chunk of an interval that does not divide a day, such as
///     `7m`, which crosses midnight; or one of an interval longer than a day): each remaining piece
///     is fetched alone and stored under its OWN bounds' keys, a partial key like a ragged request
///     edge, and the chunk's whole buckets are then resampled from the store. A repeat cuts the same
///     pieces, so the skip check above runs per piece.
///   - **None of it** — every day of a lane that never imported anything: the one piece IS the
///     chunk, and everything above is byte-identical to before.
///
///   ⚠ This applies to a RECENT chunk too, which the design's §4.3 said it need not: "an archive day
///   is always settled, so it can never sit inside them" is true of a chunk that is one UTC day, and
///   false of a `7m` chunk whose second day is still inside the margin while its first was imported.
///   Its pieces take provisional keys like any recent chunk, and its settled pass later cuts the
///   same pieces and supersedes them exactly.
///   **The store step runs under the day-owner lock (change C)** — the one per `(store root,
///   symbol)` that [`ArchiveImport`] holds for its whole request — and never across a download.
///   Once the fetch returns, the archive check is asked AGAIN under the lock: an import may have
///   stored one of the chunk's days while it downloaded, so whatever the archive now holds is cut
///   out of what was fetched before anything is stored, and a chunk the archive now holds whole
///   stores nothing and resamples from the store. A resample-only chunk takes the lock as well, so
///   no write of this lane ever interleaves with an import's.
/// - **`should_stop` is asked at the TOP of every chunk** — before its settled test, its key read
///   and its fetch, so between the last chunk's store step and this chunk's first I/O, and never
///   inside a chunk. `true` stops the request at that boundary in a [`CollectError::Stopped`] —
///   never `Ok`, since a window with chunks it never reached is not in the store — naming the
///   boundary, the bars the chunks before it wrote and EVERY chunk a supersede refusal skipped
///   earlier (each already logged at error level when it was skipped), so a stop never hides a hole.
///   Like a failed chunk, a stop leaves every chunk before it written — a recent one under its
///   provisional keys, which is harmless — and repeating the request resumes at the boundary. Why
///   never mid-chunk is `crate::klines::ingest_klines_chunked`'s argument, unchanged: a chunk is the
///   unit of atomicity. A caller that cannot be cancelled passes a probe that never fires.
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
/// partial one. So each resample — a settled chunk's or a recent one's — covers only the
/// buckets lying wholly inside what it spans (`whole_buckets`), which, chunk edges being bar edges,
/// trims only at the window's own two ends; a span holding none writes no bar and spends no
/// resample key. The quotes are stored untrimmed, so the edge ticks are there for a wider window's
/// resample to find.
///
/// An `interval` with no positive width — `0m`, or anything `vike_marketdata::interval::interval_ms` cannot
/// read — is REFUSED before the fetch: a zero-width bucket is no bar, and `consolidate_quotes`'
/// `rem_euclid` would divide by it.
pub fn backfill_quotes_then_bars(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    should_stop: &dyn Fn() -> bool,
    mut fetch: impl FnMut(&str, i64, i64) -> Result<Vec<QuoteTick>, String>,
) -> Result<usize, CollectError> {
    let Some(step) = vike_marketdata::interval::interval_ms(interval).filter(|&step| step > 0)
    else {
        return Err(CollectError::Refused(format!(
            "dukascopy {symbol}: interval {interval:?} has no positive bar width \
             (`vike_marketdata::interval::interval_ms` reads a count plus one of s/m/h/d, and a zero count is \
             a zero-width bar). Nothing was fetched and no commit key was spent."
        )));
    };
    let chunk = chunk_len(step);
    let multi = chunk_windows(start_ms, end_ms, chunk).nth(1).is_some();
    // ONE clock read per request, so every chunk of it is judged against the same instant — the
    // wall clock, read the way `crate::klines::ingest_klines` reads it for its forming-bar guard.
    let now_ms = vike_model::now_ms();
    let (mut chunks, mut quote_rows_total, mut bars_total) = (0usize, 0usize, 0usize);
    // The settled chunks the store REFUSED to supersede, as `(from, to)`: skipped, so the rest of
    // the window still runs, and named in the error the request ends in.
    let mut refused: Vec<(i64, i64)> = Vec::new();
    // The chunk the request was asked to stop BEFORE — `None` while it runs.
    let mut stopped_before: Option<(i64, i64)> = None;
    for (from, to) in chunk_windows(start_ms, end_ms, chunk) {
        // THE STOP PROBE, first — before anything this chunk would read, fetch or write — so the
        // boundary is always between the last chunk's store step and this chunk's first I/O.
        if should_stop() {
            stopped_before = Some((from, to));
            break;
        }
        chunks += 1;
        let mode = if settled(to, now_ms) { StoreMode::Settled } else { StoreMode::Recent };
        // THE ARCHIVE'S DAYS (the ARCHIVE bullet above): the UTC days of this chunk the import lane
        // already stored are never fetched, so the PIECES are what is left of the chunk once they
        // are cut out — the whole chunk, untouched, when the archive holds none of its days.
        let owned = archive_owned_days(hist, symbol, (from, to))?;
        let mut pieces = unowned_pieces((from, to), &owned);
        if !owned.is_empty() {
            tracing::info!(
                "dukascopy {symbol} [{from}, {to}]: {} UTC day(s) held by the archive lane — never \
                 fetched; {} piece(s) of the chunk left: {pieces:?}",
                owned.len(),
                pieces.len()
            );
        }
        // The skip-before-fetch, per piece, on the piece's own quote key — canonical once the chunk
        // has settled, PROVISIONAL while it is recent. A spent one means an earlier request cut that
        // piece the same way and stored it, so a fetch would only be discarded by the store.
        let mut wanted = Vec::with_capacity(pieces.len());
        for &(piece_from, piece_to) in &pieces {
            let key = mode.quote_key(symbol, piece_from, piece_to);
            if !hist.series_has_commit(&quote_series(symbol), &key)? {
                wanted.push((piece_from, piece_to));
            }
        }
        if wanted.is_empty() {
            // The chunk's TICKS are all stored already — under its own keys, the archive's, or both —
            // so they are never fetched again. What this INTERVAL may still lack is its bars, and
            // those derive from the stored ticks with no network call: a chunk already resampled at
            // this interval writes nothing. A settled chunk's resample is the bar-side supersede, so
            // it can be the refusal — skipped like any other; a recent one supersedes nothing.
            let _owner = DayOwnerLock::acquire(hist, symbol);
            let span = held_span(hist, symbol, (from, to), chunk, &pieces)?;
            bars_total +=
                resample_held_chunk(hist, symbol, interval, step, span, mode, &mut refused)?;
            continue;
        }
        // The download, OUTSIDE the day-owner lock: an import waits for a store step, never for a
        // network round trip.
        let mut fetched = Vec::with_capacity(wanted.len());
        for (piece_from, piece_to) in wanted {
            let quotes = fetch(symbol, piece_from, piece_to).map_err(|e| {
                // One chunk of several names itself and what is already written, which stays
                // written. A one-chunk window's error is the fetch's own text, as it always was.
                CollectError::Fetch(if multi {
                    format!(
                        "chunk [{from}, {to}] of [{start_ms}, {end_ms}] failed after {bars_total} \
                         {interval} bars were written: {e}"
                    )
                } else {
                    e
                })
            })?;
            fetched.push(((piece_from, piece_to), quotes));
        }
        // THE STORE STEP, under the day-owner lock (change C). An import may have stored one of this
        // chunk's days while the download ran, so the archive is asked AGAIN, under the lock, and
        // whatever it now holds is cut out of what was fetched before a single tick is stored.
        let _owner = DayOwnerLock::acquire(hist, symbol);
        let owned_now = archive_owned_days(hist, symbol, (from, to))?;
        if owned_now != owned {
            // The archive only ever GAINS a day — unless something deleted the series under the
            // request, and then the pieces fetched no longer cover what is unowned: resampling
            // over them would seal bars from a partial day, for good. So that case aborts.
            if !owned.iter().all(|day| owned_now.contains(day)) {
                return Err(CollectError::Data(vike_data::DataError::Io(format!(
                    "dukascopy {symbol} [{from}, {to}]: archive day keys of this chunk were \
                     REMOVED from the store while it was being fetched ({owned:?} became \
                     {owned_now:?}) — nothing of the chunk was stored; repeat the request"
                ))));
            }
            tracing::info!(
                "dukascopy {symbol} [{from}, {to}]: the archive lane stored UTC day(s) {:?} of \
                 this chunk while it was being fetched — their fetched ticks are dropped, not \
                 stored",
                owned_now.iter().filter(|day| !owned.contains(day)).collect::<Vec<_>>()
            );
            pieces = unowned_pieces((from, to), &owned_now);
            fetched = clip_to(fetched, &pieces);
        }
        if fetched.is_empty() {
            // The archive now holds every piece this request fetched: nothing is stored, and the
            // bars derive from the store exactly as a held chunk's do above.
            let span = held_span(hist, symbol, (from, to), chunk, &pieces)?;
            bars_total +=
                resample_held_chunk(hist, symbol, interval, step, span, mode, &mut refused)?;
            continue;
        }
        match (store_then_resample(hist, symbol, interval, step, (from, to), mode, fetched), mode) {
            (Ok((quote_rows, bars)), _) => {
                quote_rows_total += quote_rows;
                bars_total += bars;
            }
            // The one store step that can be REFUSED (it supersedes): the refusal is this chunk's
            // alone, so it is skipped and every later chunk still runs. Anything else aborts.
            (Err(e), StoreMode::Settled) => {
                skip_if_refused(e, symbol, interval, (from, to), &mut refused)?;
            }
            // A RECENT chunk is stored under its own PROVISIONAL keys, which its settled commit later
            // supersedes. It supersedes nothing itself, so it cannot be the refusal: any error it
            // returns aborts the request.
            (Err(e), StoreMode::Recent) => return Err(e),
        }
    }
    // A one-chunk window's line already IS its total — the line such a request has always logged —
    // so the sum is logged only when there is more than one chunk to sum.
    if multi {
        let skipped = if refused.is_empty() {
            String::new()
        } else {
            format!(" — {} SKIPPED (refused), not counted above", refused.len())
        };
        let stopped = match stopped_before {
            Some((from, to)) => format!(
                " — STOPPED before [{from}, {to}] (asked to stop; it and every chunk after it were \
                 never fetched)"
            ),
            None => String::new(),
        };
        tracing::info!(
            "dukascopy {symbol} [{start_ms}, {end_ms}]: {quote_rows_total} quote rows, \
             {bars_total} {interval} bars over {chunks} chunks{skipped}{stopped}"
        );
    }
    // A stop answers before a refusal does: a `SupersedeRefused` says the request RAN to its end,
    // and a stopped one did not — so the stop's own text names the refused chunks instead.
    if let Some(boundary) = stopped_before {
        return Err(CollectError::Stopped(stopped_report(
            symbol,
            interval,
            (start_ms, end_ms),
            chunk,
            boundary,
            (chunks, quote_rows_total, bars_total),
            &refused,
        )));
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

/// Which keys `store_then_resample` spends for one chunk, decided by [`settled`]. Both modes key
/// each stored PIECE by its own `(from, to)` bounds — the whole chunk, unless the archive holds
/// some of its days — and the chunk's bars by its own whole buckets, whether the chunk is a whole
/// window or one of several; they differ in the namespace and in whether the write supersedes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreMode {
    /// A settled chunk: keys by the canonical formatters, and ALWAYS supersedes that same chunk's
    /// provisional key — removing the provisional entry when there is one, and pre-spending the key
    /// when there is not, so a recent fetch of the chunk that lands later writes nothing.
    Settled,
    /// A chunk still inside the publication margin: keys by the PROVISIONAL formatters, which the
    /// same chunk's `Settled` commit later supersedes exactly. No supersede of its own — an early
    /// fetch has nothing to supersede — so it is never refused.
    Recent,
}

impl StoreMode {
    /// The quote key this mode spends for the piece `[from, to]` — canonical once settled,
    /// provisional while recent — and so the key the skip-before-fetch asks about.
    fn quote_key(self, symbol: &str, from: i64, to: i64) -> String {
        match self {
            StoreMode::Settled => quote_commit_key(symbol, from, to),
            StoreMode::Recent => provisional_quote_commit_key(symbol, from, to),
        }
    }
}

/// Store one fetched chunk and resample it: the unit every chunk goes through, settled or recent.
/// `pieces` are what was fetched, each with its own bounds — ONE piece, the whole `[from, to]`,
/// unless the archive holds some of the chunk's UTC days — and each is stored under its own bounds'
/// keys. The whole buckets of `[from, to]` are what gets resampled, whatever keys it. `mode` decides
/// which keys are spent, and whether each write also supersedes a stale provisional entry — see
/// [`StoreMode`]. The line a chunk has always logged is logged. Returns `(quote rows, bars)`.
///
/// ⚠ A refusal on a LATER piece leaves the earlier pieces stored: each is its own commit, so a
/// retry skips them by their spent keys and meets the refused one again, as it would a whole chunk.
fn store_then_resample(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    step: i64,
    (from, to): (i64, i64),
    mode: StoreMode,
    pieces: Vec<((i64, i64), Vec<QuoteTick>)>,
) -> Result<(usize, usize), CollectError> {
    let mut quote_rows = 0;
    for ((piece_from, piece_to), quotes) in pieces {
        let key = mode.quote_key(symbol, piece_from, piece_to);
        quote_rows += match mode {
            StoreMode::Settled => {
                let provisional = provisional_quote_commit_key(symbol, piece_from, piece_to);
                hist.append_quotes_superseding(
                    VENUE,
                    symbol,
                    &quotes,
                    Some(&key),
                    Some(&provisional),
                )?
            }
            StoreMode::Recent => hist.append_quotes(VENUE, symbol, &quotes, Some(&key))?,
        };
        // The resample re-reads the span from the store, so the fetched ticks are dead weight from
        // here: freeing them holds the data daemon to one copy of them rather than two.
        drop(quotes);
    }
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
        StoreMode::Recent => resample_recent(hist, symbol, interval, range)?,
    };
    tracing::info!(
        "dukascopy {symbol} [{from}, {to}]: {quote_rows} quote rows, {bars} {interval} bars \
         over [{first}, {last}]"
    );
    Ok((quote_rows, bars))
}

/// The bar half of a SETTLED chunk: resample `range` — the chunk's whole buckets — under the
/// chunk's own canonical resample key ([`resample_commit_key`] of `range`), atomically superseding
/// that same window's provisional bars when there are any, and pre-spending their key when there
/// are none. Shared by the store step ([`store_then_resample`]'s `Settled` arm) and by
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

/// The bar half of a RECENT chunk: resample `range` — the chunk's whole buckets, the very range
/// [`resample_settled`] resamples once the chunk settles — under the chunk's own PROVISIONAL
/// resample key, so that settled resample supersedes these bars exactly. Shared by the store step
/// ([`store_then_resample`]'s `Recent` arm) and by [`resample_stored_recent_chunk`], so the two can
/// only ever spend one spelling of the key. A spent key writes nothing. Returns bars written.
fn resample_recent(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    let key = provisional_resample_key(symbol, interval, range);
    Ok(hist.resample_quotes_to_bars(VENUE, symbol, interval, range, Some(&key))?)
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
    let bar_series =
        SeriesId::per_symbol(StoreKindId::Bar, VENUE, symbol, Some(interval.to_string()));
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

/// The RECENT twin of [`resample_stored_chunk`]: a chunk still inside the publication margin whose
/// PROVISIONAL quote key is spent — an earlier request cut it the same way and stored it — needs no
/// fetch, because the store would discard the answer under the spent key. What THIS interval may
/// still lack is its bars, which derive from the stored ticks under the chunk's PROVISIONAL
/// resample key ([`resample_recent`]), exactly as the store step's own resample would have written
/// them — so the chunk's settled commit still supersedes them. Writes nothing when that key is
/// spent too, which is the ordinary repeat of a request. Returns bars written — `0` when the chunk
/// is already resampled at this interval or holds no whole bucket of it.
///
/// ⚠ The bar-side key is asked first, with one lock-free manifest read, as
/// [`resample_stored_chunk`] asks its own: the store resamples by reading the span's ticks back
/// BEFORE it finds a key spent, so an unasked repeat would re-read a day of ticks to write nothing.
fn resample_stored_recent_chunk(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    step: i64,
    (from, to): (i64, i64),
) -> Result<usize, CollectError> {
    let Some((first, last)) = whole_buckets(from, to, step) else {
        tracing::info!(
            "dukascopy {symbol} [{from}, {to}]: recent, already fetched (provisional key spent) — \
             skipping the fetch; no whole {interval} bar inside the chunk"
        );
        return Ok(0);
    };
    let range = TsRange::of(first, last);
    let bar_series =
        SeriesId::per_symbol(StoreKindId::Bar, VENUE, symbol, Some(interval.to_string()));
    if hist.series_has_commit(&bar_series, &provisional_resample_key(symbol, interval, range))? {
        tracing::info!(
            "dukascopy {symbol} [{from}, {to}]: recent, already fetched and resampled at \
             {interval} (both provisional keys spent) — skipping the fetch"
        );
        return Ok(0);
    }
    let bars = resample_recent(hist, symbol, interval, range)?;
    tracing::info!(
        "dukascopy {symbol} [{from}, {to}]: recent, ticks already fetched (provisional key \
         spent) — {bars} {interval} bars resampled from the store over [{first}, {last}], no fetch"
    );
    Ok(bars)
}

/// A chunk with NOTHING left to fetch — every tick already stored, under its own keys, the
/// archive's, or both — still owes THIS interval's bars, derived from the store: a settled chunk
/// through [`resample_stored_chunk`], a recent one through [`resample_stored_recent_chunk`]. Returns
/// bars written.
///
/// A settled chunk's resample is the bar-side supersede, so it can be the refusal: that is skipped
/// like any other (`skip_if_refused`, recording the chunk in `refused`) and answers `0`. A recent
/// one supersedes nothing, so it cannot be the refusal, and any error it returns aborts.
fn resample_held_chunk(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    step: i64,
    (from, to): (i64, i64),
    mode: StoreMode,
    refused: &mut Vec<(i64, i64)>,
) -> Result<usize, CollectError> {
    match mode {
        StoreMode::Settled => match resample_stored_chunk(hist, symbol, interval, step, (from, to))
        {
            Ok(bars) => Ok(bars),
            Err(e) => skip_if_refused(e, symbol, interval, (from, to), refused).map(|()| 0),
        },
        StoreMode::Recent => resample_stored_recent_chunk(hist, symbol, interval, step, (from, to)),
    }
}

/// The span a chunk with nothing left to fetch resamples. The chunk itself — whose whole buckets are
/// what the skip has always resampled — UNLESS the archive holds the chunk whole (`pieces` is empty)
/// and every UTC day of the chunk's whole grid CELL, `[c0, c0 + chunk - 1]`, as well: then the CELL.
///
/// ⚠ **This is the one place the design's change A had to be widened, and it is about BARS.** The
/// design said a held chunk "runs its existing `resample_stored_chunk`", which resamples the chunk's
/// own whole buckets. For a RAGGED chunk — the window's first or last, cut mid-day, which the start of
/// every `--days N` request is — those buckets are a TRIMMED range, so its resample key differs from
/// the day-range key [`ArchiveImport`] already spent at the same interval, and the store, which
/// dedups by key and never by row, wrote that part of the day's bars a SECOND time: double `volume`
/// from a lane that stored no tick. The archive holds the cell whole, so its whole buckets are
/// complete and correct; resampling them lands on the import's own key — a day, for every interval
/// that divides one — and writes nothing when that key is spent, or the whole cell's bars once when
/// it is not. The cost is that the bar count such a request reports can include bars just outside
/// its window. A chunk the archive holds only PART of keeps the trimmed range: its bars are the HTTP
/// lane's, and two ragged HTTP requests overlapping is that lane's own, older, baseline.
fn held_span(
    hist: &DataFusionHist,
    symbol: &str,
    (from, to): (i64, i64),
    chunk: i64,
    pieces: &[(i64, i64)],
) -> Result<(i64, i64), CollectError> {
    if !pieces.is_empty() {
        return Ok((from, to));
    }
    let cell = from
        .div_euclid(chunk)
        .checked_mul(chunk)
        .and_then(|start| Some((start, start.checked_add(chunk - 1)?)))
        .filter(|&(start, _)| start >= 0);
    let Some(cell) = cell.filter(|&cell| cell != (from, to)) else {
        return Ok((from, to));
    };
    let days = archive_days_touched(cell);
    let whole = !days.is_empty() && archive_owned_days(hist, symbol, cell)? == days;
    Ok(if whole { cell } else { (from, to) })
}

/// The UTC days `[from, to]` touches whose ARCHIVE day key ([`archive_quote_commit_key`]) is spent
/// — the days the import lane owns (docs/decisions/0100) — as day starts, ascending.
///
/// A chunk of any interval up to a day is at most a day long and touches at most two UTC days, so
/// it costs at most two `series_has_commit` reads, the design's §4.3 figure. A chunk of a LONGER
/// interval is one bar and touches one day per day of it, which the design did not foresee: asking
/// each day would re-parse the manifest once per day, so such a chunk reads the commit log ONCE
/// instead (`series_commits`) and tests each day against it.
fn archive_owned_days(
    hist: &DataFusionHist,
    symbol: &str,
    window: (i64, i64),
) -> Result<Vec<i64>, CollectError> {
    let days = archive_days_touched(window);
    let key = |day: i64| archive_quote_commit_key(symbol, day, day + MS_PER_DAY - 1);
    let series = quote_series(symbol);
    let mut owned = Vec::new();
    if days.len() <= 2 {
        for day in days {
            if hist.series_has_commit(&series, &key(day))? {
                owned.push(day);
            }
        }
    } else {
        let spent: BTreeSet<String> = hist.series_commits(&series)?.into_iter().collect();
        owned.extend(days.into_iter().filter(|&day| spent.contains(&key(day))));
    }
    Ok(owned)
}

/// Resample stored Dukascopy quotes → OHLCV bars at `interval` for `symbol` over `range`, writing
/// them back into the store. Thin wrapper over `HistStore::resample_quotes_to_bars`, which reads the
/// stored quote slice and folds it with the parity-tested `vike_model::consolidate_quotes`.
/// Idempotent: the derived batch is keyed by `(symbol, interval, range)` — [`resample_commit_key`]
/// of `range` — so re-running is a no-op. Returns bars written.
pub fn resample_and_store_bars(
    hist: &DataFusionHist,
    symbol: &str,
    interval: &str,
    range: TsRange,
) -> Result<usize, CollectError> {
    let key = resample_commit_key(symbol, interval, range);
    Ok(hist.resample_quotes_to_bars(VENUE, symbol, interval, range, Some(&key))?)
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
/// spelled once: [`resample_and_store_bars`], and `resample_settled` with
/// `resample_stored_chunk`'s spent-key check — every one keying by the very `range` it resamples.
/// (A multi-chunk window's recent tail used to key its resample by the request's raw bounds
/// instead; it is gone, every recent chunk now resampling under [`provisional_resample_key`] of its
/// own range.) ⚠ `crates/vike-data/src/store/store_kind.rs`'s `bar`-kind `dukascopy-resample:` row
/// checks this literal verbatim, so its characters must stay exactly this.
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

#[path = "dukascopy_tests.rs"]
#[cfg(test)]
mod dukascopy_tests;
