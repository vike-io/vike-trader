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
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, PoisonError};

use vike_data::{DataFusionHist, HistStore, SeriesCoverage, SeriesId, TsRange};
use vike_model::{MS_PER_DAY, QuoteTick};

use crate::error::CollectError;

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
    SeriesId::per_symbol("quote", VENUE, symbol, None)
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
/// An `interval` with no positive width — `0m`, or anything `vike_model::time::interval_ms` cannot
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

/// [`CollectError::Stopped`]'s text for a request asked to stop before the chunk `(from, to)` of a
/// window cut into `chunk`-long pieces: the boundary, what the `chunks` before it wrote, that
/// repeating the request resumes there — and EVERY chunk a supersede refusal skipped earlier, with
/// the remedy, because this is all a caller that reads only the reply ever learns, and a stop must
/// never hide a hole the request had already made.
///
/// The total is counted from the grid rather than by walking [`chunk_windows`], so naming it costs
/// nothing however long the window is; `i128`, so a window at the edge of `i64` cannot overflow it.
fn stopped_report(
    symbol: &str,
    interval: &str,
    (start_ms, end_ms): (i64, i64),
    chunk: i64,
    (from, to): (i64, i64),
    (chunks, quote_rows, bars): (usize, usize, usize),
    refused: &[(i64, i64)],
) -> String {
    let cell = |ms: i64| i128::from(ms.div_euclid(chunk));
    let total = (cell(end_ms) - cell(start_ms) + 1).max(1);
    let holes = if refused.is_empty() {
        String::new()
    } else {
        let skipped = refused
            .iter()
            .map(|(from, to)| format!("[{from}, {to}]"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            " But {} of those {chunks} chunk(s) were SKIPPED before the stop, because the store \
             REFUSED to supersede them, and hold no {interval} bars: {skipped}. {}",
            refused.len(),
            refusal_remedy(symbol, interval),
        )
    };
    format!(
        "dukascopy {symbol} [{start_ms}, {end_ms}] {interval}: asked to stop, and stopped before \
         chunk {} of {total} [{from}, {to}] — nothing failed. The {chunks} chunk(s) before it ran, \
         writing {bars} {interval} bars and {quote_rows} quote rows; what they wrote stays stored. \
         This chunk and every one after it were never fetched: repeating the request resumes \
         here.{holes}",
        chunks + 1,
    )
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
    let bar_series = SeriesId::per_symbol("bar", VENUE, symbol, Some(interval.to_string()));
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

/// The start of every UTC day `[from, to]` touches that COULD carry an archive day key — one starting
/// at or after the epoch (an archive day is a calendar date from 1970 on) whose last millisecond is
/// representable — ascending; none for an inverted window, which therefore stays the one piece it
/// always was. The day indices come from `div_euclid`, so a window starting before 1970 finds its
/// days like any other.
fn archive_days_touched((from, to): (i64, i64)) -> Vec<i64> {
    if from > to {
        return Vec::new();
    }
    (from.div_euclid(MS_PER_DAY).max(0)..=to.div_euclid(MS_PER_DAY))
        .filter_map(|index| index.checked_mul(MS_PER_DAY))
        .filter(|day| day.checked_add(MS_PER_DAY - 1).is_some())
        .collect()
}

/// `[from, to]` with every `owned` UTC day cut out, as inclusive pieces in order — the parts of a
/// chunk the HTTP lane may still fetch and store. `owned` is ascending ([`archive_owned_days`]).
///
/// With NOTHING owned the answer is exactly `[(from, to)]`, an inverted window included, which is
/// what keeps a chunk the archive never touched byte-identical to the lane before the archive
/// existed. With every day owned it is empty. Checked arithmetic, so a day at the edge of `i64` ends
/// the cut rather than overflowing it.
fn unowned_pieces((from, to): (i64, i64), owned: &[i64]) -> Vec<(i64, i64)> {
    if owned.is_empty() {
        return vec![(from, to)];
    }
    let mut pieces = Vec::new();
    let mut cursor = Some(from);
    for &day in owned {
        let Some(at) = cursor else { break };
        if day > at {
            pieces.push((at, (day - 1).min(to)));
        }
        cursor = day.checked_add(MS_PER_DAY).map(|next| next.max(at));
    }
    if let Some(at) = cursor
        && at <= to
    {
        pieces.push((at, to));
    }
    pieces
}

/// What was `fetched` piece by piece, cut down to `pieces` — the chunk's pieces RE-COMPUTED under the
/// day-owner lock, after an import stored one of its days while the download ran. The archive only
/// GAINS days, so every new piece lies inside a fetched one: it takes that piece's ticks inside its
/// own bounds, under its own bounds, and a fetched piece the archive now holds whole is dropped. A
/// new piece inside no fetched one was never fetched because its own key was already spent, and it
/// is stored already.
fn clip_to(
    fetched: Vec<((i64, i64), Vec<QuoteTick>)>,
    pieces: &[(i64, i64)],
) -> Vec<((i64, i64), Vec<QuoteTick>)> {
    let mut out = Vec::new();
    for ((fetched_from, fetched_to), quotes) in fetched {
        for &(from, to) in pieces.iter().filter(|(f, t)| fetched_from <= *f && *t <= fetched_to) {
            let kept = quotes.iter().filter(|q| (from..=to).contains(&q.ts)).cloned().collect();
            out.push(((from, to), kept));
        }
    }
    out
}

// ── the day-owner lock (the design's change C) ────────────────────────────────────────────────

/// The `(store root, symbol)` pairs whose [`DayOwnerLock`] is held right now, in this process.
static DAY_OWNERS_HELD: Mutex<BTreeSet<(PathBuf, String)>> = Mutex::new(BTreeSet::new());

/// Woken every time a pair leaves [`DAY_OWNERS_HELD`].
static DAY_OWNER_RELEASED: Condvar = Condvar::new();

/// The ONE in-process lock both Dukascopy quote lanes take around their STORE steps, per `(store
/// root, symbol)` — the design's change C (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`
/// §4.3). [`ArchiveImport`] holds it for its whole request, from the moment it reads the series'
/// keys to classify a day until its last day is stored; [`backfill_quotes_then_bars`] holds it for
/// each chunk's store step and never across a download, re-asking the archive under it. So the
/// question each lane asks before it writes — does the other lane own this day? — can never be
/// answered by a store the other lane is about to change.
///
/// ⚠ **One process is enough**, because docs/decisions/0084 makes the datahub the store's only
/// writer; across processes the store's own `SeriesLock` still serialises each append, and its
/// `has_commit` under that lock still makes a same-key repeat a no-op — but it cannot see that two
/// DIFFERENT keys cover one day, which is what this lock is for.
///
/// ⚠ **Not re-entrant.** A thread that holds an [`ArchiveImport`] and then runs the HTTP lane over
/// the same `(store root, symbol)` waits for itself forever.
///
/// The root is canonicalised once per acquire (falling back to the path as given when that fails),
/// so two handles opened on one directory share one lock. A pair is a value in a set rather than a
/// mutex of its own, so nothing is kept for a symbol nobody holds; the guard's `Drop` frees it — on
/// unwind too — and wakes every waiter, each of which re-checks its own pair.
struct DayOwnerLock {
    held: (PathBuf, String),
}

impl DayOwnerLock {
    /// Block until no one holds `(hist's root, symbol)`, then hold it until the guard drops.
    fn acquire(hist: &DataFusionHist, symbol: &str) -> Self {
        let root = hist.root();
        let held = (
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            symbol.to_string(),
        );
        let mut set = DAY_OWNERS_HELD.lock().unwrap_or_else(PoisonError::into_inner);
        while set.contains(&held) {
            set = DAY_OWNER_RELEASED.wait(set).unwrap_or_else(PoisonError::into_inner);
        }
        set.insert(held.clone());
        Self { held }
    }
}

impl Drop for DayOwnerLock {
    fn drop(&mut self) {
        let mut set = DAY_OWNERS_HELD.lock().unwrap_or_else(PoisonError::into_inner);
        set.remove(&self.held);
        drop(set);
        DAY_OWNER_RELEASED.notify_all();
    }
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

// ── THE ARCHIVE HALF: one owner per UTC day, and the import of one day ──────────────────────────
//
// What the datahub's archive import calls (docs/decisions/0100). It names no file, no decoder and no
// wire type: the datahub walks the imports directory, decodes a day with the bridge, maps the result
// onto its own plan and answer, and hands this module QUOTES — so this crate still names no bridge
// (docs/decisions/0094).

/// What the archive import does with ONE UTC day that has a daily file — the design's one-owner table
/// (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §4.2), computed by
/// [`DayOwners::classify`] and checked in VARIANT ORDER, the first that applies deciding: so a recent
/// day is never imported, and a held day is never also reported as overlapped.
///
/// It is the store half of `crates/vike-datahub-client/src/archive.rs`'s `DayClass`, one variant for
/// one, minus that type's plan-time `Refused` (a mixed-layout day or a header past a cap), which is
/// the datahub's to decide before the store is ever asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayClass {
    /// The day ends inside `PUBLICATION_MARGIN_MS` — the HTTP lane's own margin, through the same
    /// `settled` predicate. Refused: "import again later". The archive never writes a provisional key.
    TooRecent,
    /// The archive already stored the day: its [`archive_quote_commit_key`] is spent. The file is
    /// skipped and never decoded; any requested interval still missing derives from the stored ticks.
    HeldByArchive,
    /// The HTTP lane already stored the day under its CANONICAL day key, [`quote_commit_key`] over
    /// the day's own bounds. Skipped and topped up exactly as [`Self::HeldByArchive`].
    HeldByHttp,
    /// The ONLY Dukascopy quote key meeting the day is a PROVISIONAL one whose range IS the day — a
    /// recent full-day chunk the HTTP lane fetched early. The import stores the day and supersedes
    /// `key` in the same locked publish, exactly as the HTTP lane's own settled fetch would.
    Supersede {
        /// The provisional key the import supersedes.
        key: String,
    },
    /// Some OTHER Dukascopy quote key meets the day — a ragged request edge, a chunk of an interval
    /// that does not divide a day, a provisional window cut other than to the day, a tail written
    /// before per-chunk provisional keys, or a key whose bounds do not parse. REFUSED and listed:
    /// importing the whole day would store the overlapped ticks twice, and importing the rest of it
    /// would trust a range whose ticks may have been fetched before the vendor finished publishing.
    /// The day stays the HTTP lane's to fill.
    Overlapped {
        /// Every key that meets the day, in the store's commit-log order.
        keys: Vec<String>,
    },
    /// No Dukascopy quote key meets the day: the import decodes it, stores it under the archive day
    /// key and derives the requested bars.
    Free,
}

impl ArchiveDayClass {
    /// Whether the import STORES this day — [`Self::Free`] and [`Self::Supersede`]; the number a
    /// plan's typed confirmation counts.
    pub fn is_importable(&self) -> bool {
        matches!(self, Self::Free | Self::Supersede { .. })
    }
}

/// Which of the three Dukascopy quote LANES a commit key came down — the two HTTP-lane namespaces and
/// the archive's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteLane {
    /// [`quote_commit_key`]: a SETTLED HTTP-lane chunk (or a piece of one).
    Http,
    /// [`provisional_quote_commit_key`]: a RECENT HTTP-lane chunk, or the twin every settled commit
    /// pre-spends — an import's included.
    Provisional,
    /// [`archive_quote_commit_key`]: an imported day.
    Archive,
}

/// One of the series' Dukascopy quote keys, parsed: its lane and its inclusive `[start, end]` —
/// `None` when the bounds after the symbol do not parse, which [`DayOwners::classify`] reads as
/// meeting EVERY day, the conservative direction.
#[derive(Debug, Clone)]
struct LaneKey {
    lane: QuoteLane,
    range: Option<(i64, i64)>,
    key: String,
}

impl LaneKey {
    /// Whether this key's range shares a millisecond with `[day, end]`. An inverted range is read as
    /// the span between its two bounds, so it can only meet MORE days than its writer meant.
    fn meets(&self, day: i64, end: i64) -> bool {
        self.range.is_none_or(|(a, b)| a.min(b) <= end && a.max(b) >= day)
    }
}

/// The Dukascopy quote keys of ONE series, parsed once — what every day of an archive import is
/// classified against ([`Self::classify`]).
///
/// Built from the series' commit log, which the design reads ONCE per request rather than asking
/// `series_has_commit` per day: that call re-parses the whole manifest every time, and an exact-key
/// question cannot see a key whose RANGE meets a day under other bounds — which is exactly the
/// [`ArchiveDayClass::Overlapped`] case. [`quote_series_facts`] is the one read, and
/// [`ArchiveImport::begin`] makes it under the day-owner lock.
///
/// A key belongs to the series' lanes when it opens with one of the three formatters' heads FOR THIS
/// SYMBOL — `dukascopy:EURUSD:`, `dukascopy-provisional:EURUSD:`, `dukascopy-archive:EURUSD:`, each
/// derived from its formatter rather than spelled again here. Every other key is not a Dukascopy
/// quote lane's and owns no day: another producer's, or another symbol's sharing a grouped leaf.
#[derive(Debug, Clone)]
pub struct DayOwners {
    keys: Vec<LaneKey>,
}

impl DayOwners {
    /// Parse `keys` — a quote series' commit log, as `vike_data::DataFusionHist::series_facts` or
    /// `series_commits` answers it — for `symbol`'s three Dukascopy quote lanes.
    pub fn parse(symbol: &str, keys: &[String]) -> Self {
        let heads = [
            (QuoteLane::Http, key_head(quote_commit_key, symbol)),
            (QuoteLane::Provisional, key_head(provisional_quote_commit_key, symbol)),
            (QuoteLane::Archive, key_head(archive_quote_commit_key, symbol)),
        ];
        let keys = keys
            .iter()
            .filter_map(|key| {
                heads.iter().find_map(|(lane, head)| {
                    let bounds = key.strip_prefix(head.as_str())?;
                    Some(LaneKey { lane: *lane, range: parse_bounds(bounds), key: key.clone() })
                })
            })
            .collect();
        Self { keys }
    }

    /// The design's §4.2 table for the UTC day starting at `day`, judged at `now_ms` — see
    /// [`ArchiveDayClass`] for each row and why the order is load-bearing. `day` is the start of a
    /// UTC day; [`ArchiveImport::import_day`] refuses one that is not.
    pub fn classify(&self, day: i64, now_ms: i64) -> ArchiveDayClass {
        let end = day.saturating_add(MS_PER_DAY - 1);
        if !settled(end, now_ms) {
            return ArchiveDayClass::TooRecent;
        }
        let exact = |lane| self.keys.iter().any(|k| k.lane == lane && k.range == Some((day, end)));
        if exact(QuoteLane::Archive) {
            return ArchiveDayClass::HeldByArchive;
        }
        if exact(QuoteLane::Http) {
            return ArchiveDayClass::HeldByHttp;
        }
        let meeting: Vec<&LaneKey> = self.keys.iter().filter(|k| k.meets(day, end)).collect();
        match meeting.as_slice() {
            [] => ArchiveDayClass::Free,
            [only] if only.lane == QuoteLane::Provisional && only.range == Some((day, end)) => {
                ArchiveDayClass::Supersede { key: only.key.clone() }
            }
            _ => ArchiveDayClass::Overlapped {
                keys: meeting.iter().map(|k| k.key.clone()).collect(),
            },
        }
    }

    /// Record what [`ArchiveImport::import_day`] just spent for `[day, end]` — the archive day key and
    /// the provisional twin every settled commit pre-spends — so a later question in the same
    /// request answers [`ArchiveDayClass::HeldByArchive`], as a fresh read of the store would.
    fn record_import(&mut self, day: i64, end: i64, archive_key: String, provisional_key: String) {
        for (lane, key) in
            [(QuoteLane::Archive, archive_key), (QuoteLane::Provisional, provisional_key)]
        {
            self.keys.push(LaneKey { lane, range: Some((day, end)), key });
        }
    }
}

/// The head `format` writes for `symbol` before its `{start}-{end}` bounds — `dukascopy:EURUSD:` for
/// [`quote_commit_key`] — read off the formatter itself with the bounds `0-0` cut away, so the three
/// templates are spelled once, in their formatters, and nowhere in the parser.
fn key_head(format: fn(&str, i64, i64) -> String, symbol: &str) -> String {
    let mut head = format(symbol, 0, 0);
    head.truncate(head.len() - "0-0".len());
    head
}

/// The `{start}-{end}` bounds of a Dukascopy quote key, or `None` when they do not parse. The split
/// is the first `-` after the first character with an `i64` on each side, so a pre-1970 (negative)
/// bound on either side reads correctly.
fn parse_bounds(bounds: &str) -> Option<(i64, i64)> {
    bounds
        .char_indices()
        .skip(1)
        .filter(|&(_, c)| c == '-')
        .find_map(|(at, _)| Some((bounds[..at].parse().ok()?, bounds[at + 1..].parse().ok()?)))
}

/// `symbol`'s Dukascopy quote series' coverage and commit log, from ONE manifest parse
/// (`vike_data::DataFusionHist::series_facts`): what a plan that writes nothing — a dry run —
/// classifies its days against through [`DayOwners::parse`], without taking the day-owner lock. An
/// import that WRITES takes the same read under the lock instead, through [`ArchiveImport::begin`].
pub fn quote_series_facts(
    hist: &DataFusionHist,
    symbol: &str,
) -> Result<(SeriesCoverage, Vec<String>), CollectError> {
    Ok(hist.series_facts(&quote_series(symbol))?)
}

/// Why ONE archive day was not stored (or not wholly): a stable CLASS token and an operator's
/// sentence. The datahub carries both onto the wire's `DayRefusal` unchanged.
///
/// The classes this module mints are `TooRecent`, `Overlapped`, `OutsideDay` and
/// `SupersedeRefused`; a `load` closure handed to [`ArchiveImport::import_day`] mints its own — the
/// decoder's refusal classes — and the import passes them through untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveDayRefusal {
    /// The refusal's stable CamelCase class token.
    pub class: String,
    /// What was wrong, and what was and was not written.
    pub detail: String,
}

impl ArchiveDayRefusal {
    fn new(class: &str, detail: String) -> Self {
        Self { class: class.to_string(), detail }
    }
}

/// How many bars ONE interval wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveBars {
    /// The interval, as the request named it.
    pub interval: String,
    /// Bars written — `0` when the day's bars at this interval were already stored.
    pub rows: usize,
}

/// What [`ArchiveImport::import_day`] did with one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayResult {
    /// Stored: `ticks` quotes under the archive day key, and the bars derived from them.
    Imported {
        /// Quote rows stored.
        ticks: usize,
        /// Bars written per requested interval.
        bars: Vec<ArchiveBars>,
    },
    /// The file decoded to NO ticks — per the vendor, nothing was recorded that day. Nothing is
    /// stored and no key is spent, so the day stays [`ArchiveDayClass::Free`] and a later re-publish
    /// can still fill it.
    Empty,
    /// A HELD day: nothing decoded, and every requested interval derived from the stored ticks — `0`
    /// rows for one already stored.
    ToppedUp {
        /// Bars written per requested interval.
        bars: Vec<ArchiveBars>,
    },
    /// The day was refused, and the request carries on to the next. Its `detail` says whether its
    /// ticks were stored: a refusal before or AT the quote write spends no key; a refusal of a BAR
    /// supersede leaves the day's ticks stored and its bars at that interval not.
    Refused(ArchiveDayRefusal),
}

/// ONE archive import request's hold on a `(store root, symbol)`: the day-owner lock, the series'
/// keys read once under it, and the request's bar intervals — what [`Self::import_day`] stores each
/// day through. Dropping it releases the lock.
///
/// The datahub's shape is: walk and plan, then `begin`, then [`Self::import_day`] for every day of
/// the request, in order, then drop. The lock is held for the whole request — the design's §5 caps
/// a request at 31 days, which is the datahub's to enforce at its door — so a day this session
/// classified cannot be written by the HTTP lane before this session stores it: that lane takes the
/// same lock for each of its store steps. ⚠ So a thread holding one must not run
/// [`backfill_quotes_then_bars`] over the same series: the lock is not re-entrant.
pub struct ArchiveImport<'h> {
    hist: &'h DataFusionHist,
    symbol: String,
    bars: Vec<String>,
    now_ms: i64,
    coverage: SeriesCoverage,
    owners: DayOwners,
    _owner: DayOwnerLock,
}

impl<'h> ArchiveImport<'h> {
    /// Validate `bars`, take the day-owner lock for `(hist's root, symbol)` — waiting for any HTTP
    /// chunk's store step in flight — and read the quote series' coverage and keys ONCE.
    ///
    /// `bars` are the intervals derived per stored day, each through the HTTP lane's own
    /// `resample_settled` under its canonical `dukascopy-resample:` key, so bars resampled from
    /// archive ticks and from fetched ones share keys and are never written twice. Each must have a
    /// positive width that DIVIDES a UTC day; one that does not (`7m`) has buckets straddling
    /// midnight, which a day's import cannot build, and is REFUSED here, before anything is locked,
    /// read or written — `data hist fetch` derives it later from the stored ticks. An empty list
    /// imports ticks alone.
    ///
    /// `now_ms` is the one clock read the whole request is judged against — a day ending inside the
    /// publication margin of it is [`ArchiveDayClass::TooRecent`].
    pub fn begin(
        hist: &'h DataFusionHist,
        symbol: &str,
        bars: &[String],
        now_ms: i64,
    ) -> Result<Self, CollectError> {
        for interval in bars {
            let divides = vike_model::time::interval_ms(interval)
                .is_some_and(|step| step > 0 && MS_PER_DAY % step == 0);
            if !divides {
                return Err(CollectError::Refused(format!(
                    "dukascopy-archive {symbol}: interval {interval:?} does not divide a UTC day, so \
                     a day's import cannot build its bars — a bucket would straddle midnight. \
                     `data hist fetch` derives it from the stored ticks instead. Nothing was \
                     locked, read or written."
                )));
            }
        }
        let owner = DayOwnerLock::acquire(hist, symbol);
        let (coverage, keys) = quote_series_facts(hist, symbol)?;
        Ok(Self {
            hist,
            symbol: symbol.to_string(),
            bars: bars.to_vec(),
            now_ms,
            coverage,
            owners: DayOwners::parse(symbol, &keys),
            _owner: owner,
        })
    }

    /// The quote series' coverage as this session read it, under the lock — what a plan reports as
    /// the store's size before the import.
    pub fn coverage(&self) -> &SeriesCoverage {
        &self.coverage
    }

    /// [`DayOwners::classify`] for `day`, against the keys this session read and the days it has
    /// imported since, at the session's `now_ms`.
    pub fn classify(&self, day: i64) -> ArchiveDayClass {
        self.owners.classify(day, self.now_ms)
    }

    /// Import the UTC day starting at `day`: classify it, and act on its class — the design's §4.2
    /// table and §4.4:
    ///
    /// - [`ArchiveDayClass::Free`] / [`ArchiveDayClass::Supersede`]: call `load` (the decode), store
    ///   the ticks under the archive day key through `append_quotes_superseding` with the day's
    ///   PROVISIONAL key as the one to supersede — a pre-spend on a free day, the removal of the HTTP
    ///   lane's early fetch on a supersede day, one code path — then derive every requested interval.
    /// - [`ArchiveDayClass::HeldByArchive`] / [`ArchiveDayClass::HeldByHttp`]: `load` is NEVER called;
    ///   each requested interval still missing derives from the stored ticks — which is also how a
    ///   request killed between a day's ticks and its bars RESUMES.
    /// - [`ArchiveDayClass::TooRecent`] / [`ArchiveDayClass::Overlapped`]: `load` is never called and
    ///   nothing is written; the day is [`ArchiveDayResult::Refused`].
    ///
    /// The answer is per day, so the caller goes on to the next: `load`'s own refusal (a file that
    /// does not decode), an empty payload, a tick outside the day, and the store REFUSING a supersede
    /// it can no longer perform exactly (compaction folded the provisional part into a multi-key part)
    /// each cost that one day and no key. `Err` is reserved for what is not one day's problem — the
    /// store failing, or a `day` that is not a UTC midnight at or after 1970, which is the caller's
    /// mistake and is refused before anything is read.
    ///
    /// Holds at most ONE day's ticks: they are dropped once stored, before the resample reads the
    /// day back. Logs one line per day, the boundary.
    pub fn import_day(
        &mut self,
        day: i64,
        load: impl FnOnce() -> Result<Vec<QuoteTick>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let symbol = self.symbol.clone();
        let Some(end) = archive_day_end(day) else {
            return Err(CollectError::Refused(format!(
                "dukascopy-archive {symbol}: {day} is not the start of a UTC day at or after 1970, \
                 which is what an archive day is. Nothing was read or written."
            )));
        };
        let result = match self.classify(day) {
            ArchiveDayClass::TooRecent => ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "TooRecent",
                format!(
                    "the day ends inside the {PUBLICATION_MARGIN_MS} ms publication margin, so the \
                     vendor may still be publishing it — import it again later. Nothing was read \
                     and no commit key was spent."
                ),
            )),
            ArchiveDayClass::Overlapped { keys } => {
                let keys = keys.join(", ");
                ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                    "Overlapped",
                    format!(
                        "Dukascopy quote keys of other bounds already meet this day ({keys}), so \
                         importing it would store their ticks twice — it stays the HTTP lane's to \
                         fill. Nothing was read and no commit key was spent."
                    ),
                ))
            }
            ArchiveDayClass::HeldByArchive | ArchiveDayClass::HeldByHttp => {
                let DayBars { written, refused } = self.resample_day(day, end)?;
                if refused.is_empty() {
                    ArchiveDayResult::ToppedUp { bars: written }
                } else {
                    ArchiveDayResult::Refused(self.bar_refusal(
                        &refused,
                        &written,
                        "its ticks were already stored, and stay so",
                    ))
                }
            }
            ArchiveDayClass::Free | ArchiveDayClass::Supersede { .. } => {
                self.store_day(day, end, load)?
            }
        };
        tracing::info!("dukascopy-archive {symbol} [{day}, {end}]: {result:?}");
        Ok(result)
    }

    /// The store half of a FREE or SUPERSEDE day — see [`Self::import_day`].
    fn store_day(
        &mut self,
        day: i64,
        end: i64,
        load: impl FnOnce() -> Result<Vec<QuoteTick>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let quotes = match load() {
            Ok(quotes) => quotes,
            Err(refusal) => return Ok(ArchiveDayResult::Refused(refusal)),
        };
        if quotes.is_empty() {
            return Ok(ArchiveDayResult::Empty);
        }
        // The decoder places every tick inside its file's day; this holds the one-owner rule even
        // against a caller that did not, because a stray tick would sit on a day this key does not
        // name — and another lane may own that day.
        if let Some(stray) = quotes.iter().find(|q| !(day..=end).contains(&q.ts)) {
            return Ok(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "OutsideDay",
                format!(
                    "a tick at {} lies outside the day [{day}, {end}] the file names. Nothing was \
                     stored and no commit key was spent.",
                    stray.ts
                ),
            )));
        }
        let key = archive_quote_commit_key(&self.symbol, day, end);
        let provisional = provisional_quote_commit_key(&self.symbol, day, end);
        let stored = self.hist.append_quotes_superseding(
            VENUE,
            &self.symbol,
            &quotes,
            Some(&key),
            Some(&provisional),
        );
        let ticks = match stored {
            Ok(rows) => rows,
            Err(e) if e.is_supersede_refusal() => {
                // The remedy is the HTTP lane's, once per requested interval, since deleting the
                // quote series strands each interval's bars; with none requested, the quote series
                // alone.
                let remedy = if self.bars.is_empty() {
                    "The refusal is PERMANENT for this day — a re-run refuses the same way — and \
                     only deleting this symbol's dukascopy quote series and importing again \
                     clears it."
                        .to_string()
                } else {
                    let each = self.bars.iter().map(|iv| refusal_remedy(&self.symbol, iv));
                    each.collect::<Vec<_>>().join(" ")
                };
                return Ok(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                    "SupersedeRefused",
                    format!(
                        "{e}. The day's ticks were NOT stored and no commit key was spent. {remedy}"
                    ),
                )));
            }
            Err(e) => return Err(e.into()),
        };
        // The resample re-reads the day from the store: the decoded ticks are dead weight now.
        drop(quotes);
        self.owners.record_import(day, end, key.clone(), provisional);
        let DayBars { written, refused } = self.resample_day(day, end)?;
        if refused.is_empty() {
            return Ok(ArchiveDayResult::Imported { ticks, bars: written });
        }
        let stored = format!("its {ticks} ticks ARE stored under `{key}`, and stay so");
        Ok(ArchiveDayResult::Refused(self.bar_refusal(&refused, &written, &stored)))
    }

    /// Derive every requested interval for `[day, end]` from the stored ticks, skipping one whose
    /// canonical resample key is already spent without reading the day back — the logic of the HTTP
    /// lane's `resample_stored_chunk`, whose whole buckets for a day-dividing interval are the day.
    fn resample_day(&self, day: i64, end: i64) -> Result<DayBars, CollectError> {
        let range = TsRange::of(day, end);
        let mut bars = DayBars { written: Vec::new(), refused: Vec::new() };
        for interval in &self.bars {
            let series = SeriesId::per_symbol("bar", VENUE, &self.symbol, Some(interval.clone()));
            let key = resample_commit_key(&self.symbol, interval, range);
            let rows = if self.hist.series_has_commit(&series, &key)? {
                0
            } else {
                match resample_settled(self.hist, &self.symbol, interval, range) {
                    Ok(rows) => rows,
                    Err(CollectError::Data(e)) if e.is_supersede_refusal() => {
                        bars.refused.push((interval.clone(), e.to_string()));
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            };
            bars.written.push(ArchiveBars { interval: interval.clone(), rows });
        }
        Ok(bars)
    }

    /// The refusal a day ends in when the store refused the BAR-side supersede of some intervals:
    /// which ones and why, what the others wrote, `ticks` — whether the day's ticks are stored — and
    /// the remedy, once per refused interval.
    fn bar_refusal(
        &self,
        refused: &[(String, String)],
        written: &[ArchiveBars],
        ticks: &str,
    ) -> ArchiveDayRefusal {
        let why = refused.iter().map(|(iv, e)| format!("{iv}: {e}")).collect::<Vec<_>>().join("; ");
        let others =
            written.iter().map(|b| format!("{} {}", b.rows, b.interval)).collect::<Vec<_>>();
        let remedy = refused
            .iter()
            .map(|(iv, _)| refusal_remedy(&self.symbol, iv))
            .collect::<Vec<_>>()
            .join(" ");
        ArchiveDayRefusal::new(
            "SupersedeRefused",
            format!(
                "the store REFUSED to supersede a provisional bar entry it could no longer remove \
                 exactly ({why}), so those bars were not written; {ticks}. Bars written for the \
                 other intervals: {others:?}. {remedy}"
            ),
        )
    }
}

/// What [`ArchiveImport`]'s per-day resample did, interval by interval.
struct DayBars {
    /// The bars written per requested interval — `0` for one already stored.
    written: Vec<ArchiveBars>,
    /// The intervals whose BAR-side supersede the store refused, each with the store's reason.
    refused: Vec<(String, String)>,
}

/// The last millisecond of the archive day starting at `day` — `None` unless `day` is a UTC midnight
/// at or after 1970 whose last millisecond is representable.
fn archive_day_end(day: i64) -> Option<i64> {
    if day < 0 || day.rem_euclid(MS_PER_DAY) != 0 {
        return None;
    }
    day.checked_add(MS_PER_DAY - 1)
}

#[path = "dukascopy_tests.rs"]
#[cfg(test)]
mod dukascopy_tests;
