//! `data.vike.io` archive backfill — Polymarket full L2 (`book_events`), taker tape (`trades`),
//! and derived top-of-book (`l1_quotes`), one UTC day per Hive-style Parquet partition:
//! `{base}/venue=polymarket/date=YYYY-MM-DD/{book_events,trades,l1_quotes}.parquet`, plus an open
//! `manifest.json` (no key) listing available dates + per-stream schema + sizes. Auth is a single
//! `X-API-Key` header (workspace `.env` key `VIKE_ARCHIVE_API_KEY` — this module takes it as a
//! plain `Option<String>` parameter; ONLY the `vike_archive_backfill` bin reads the env/`.env`, per
//! the repo's "libraries take config as parameters" rule).
//!
//! **The differentiator: ranged-HTTP row-group pruning, not whole-file download.** A `book_events`
//! day runs ~7.4 GB (465M rows, 453 row groups per the vendor's own numbers), but the file is
//! served with `Accept-Ranges: bytes` (verified live: HTTP 206 on an explicit `Range` header AND on
//! a suffix range, `Content-Range` echoes the true total size) and rows are CLUSTERED by
//! `token_id` with real per-row-group min/max statistics on that column. [`HttpRangeReader`]
//! implements `parquet::file::reader::ChunkReader` (this workspace's pinned `parquet` — reached via
//! `datafusion::parquet`, ONE arrow version, no new dep — is 58.3.0, whose `ChunkReader` trait is
//! exactly the "footer-then-column-chunks" random-access seam the `parquet` crate itself was built
//! around): `ParquetRecordBatchReaderBuilder::try_new` calls `get_bytes` itself to pull the footer
//! (last 8 bytes → footer length → footer, verified ~1.17 MB in 2 range reads) and, after
//! `.with_row_groups(selected)`, only the column chunks of the SELECTED row groups — never the
//! whole file. **No whole-file-download fallback was needed**: this trait is directly implementable
//! over a plain blocking ranged GET, so the "if impractical, fall back" contingency in this
//! feature's design brief did not trigger; a filtered single-token scan costs one row group
//! (tens of MB) rather than the whole day.
//!
//! [`select_row_groups`] does the pruning: for each row group, if the `token_id` column's own
//! min/max statistics (byte-lexicographic — the same comparison Parquet's own stats are defined
//! over for `BYTE_ARRAY`/`Utf8` columns, and the ONLY one that stays correct regardless of the
//! file's actual sort order, since a row group's min/max are always true bounds over that group's
//! own rows) bracket ANY requested token's bytes, the group is kept; a missing/absent-stats column
//! is included defensively (never silently drops rows). Column pruning (skipping `condition_id`/
//! `is_snapshot`, which this port never decodes) is a further available optimization NOT
//! implemented here — every row group read still pulls every column's chunk; scope was kept to the
//! row-group axis, the documented differentiator.
//!
//! Real physical Parquet schema (verified against the vendor's own free daily sample at
//! `/archive/samples/*.parquet` — NOT the manifest's ClickHouse-logical type names, which differ
//! from what actually lands in Arrow):
//!
//! | stream | column | Arrow physical type | vike_model field | notes |
//! |---|---|---|---|---|
//! | book_events | `token_id` | `Utf8` | `BookUpdate.symbol` | per-row — a scan mixes many tokens |
//! | book_events | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | book_events | `seq` | `UInt64` | `.seq` | |
//! | book_events | `event_type` | **`Binary`** (NOT Utf8 — ClickHouse `Enum8` exports as raw bytes) | → `BookUpdateKind` | `book`→Snapshot, `price_change`→Delta, `status`→Gap/Stale/LiveResume via `status`, `trade`/`tick_size_change` skipped |
//! | book_events | `side` | **`Binary`** | delta bids/asks placement | `buy`→bids, `sell`→asks (taker-side convention) |
//! | book_events | `price` | `Decimal128(9,4)` | level price | `÷1e4` |
//! | book_events | `size` | `Decimal128(18,6)` | level size | `÷1e6` |
//! | book_events | `bids` / `asks` | `Utf8` | snapshot levels | plain-number JSON `[[price,size],...]` (NOT pmxt's stringified variant) |
//! | book_events | `tick_size` | `Decimal128(9,4)` | `.tick_size` | `÷1e4` |
//! | book_events | `status` | `Utf8` | gap/stale/live_resume label | only read on `event_type="status"` |
//! | trades | `token_id` | `Utf8` | `TradeTick.symbol` | |
//! | trades | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | trades | `price` / `size` | **`Float64`** (already double — not Decimal here) | `.price` / `.size` | |
//! | trades | `side` | **`Utf8`** (plain string here, unlike book_events' enum) | `.is_buyer_maker` | taker side: `"sell"` → `true` |
//! | l1_quotes | `token_id` | `Utf8` | `QuoteTick.symbol` | |
//! | l1_quotes | `ts` / `local_ts` | `Int64` | `.ts` / `.local_ts` | |
//! | l1_quotes | `bid` / `ask` | `Decimal128(9,4)` | `.bid` / `.ask` | `÷1e4` |
//! | l1_quotes | `bid_size` / `ask_size` | `Decimal128(18,6)` | `.bid_size` / `.ask_size` | `÷1e6` |
//!
//! `condition_id`/`is_snapshot` exist on the wire but are never decoded (no `vike_model` field
//! needs them). Ingest is idempotent per `vikearchive:{kind}:{date}:rg{row_group_idx}` commit keys
//! (mirrors pmxt's `pmxt:{kind}:{asset}:{hour}` idiom, but WITHOUT the per-symbol component — safe
//! because `HistStore` commit-key dedup is scoped to the series directory, i.e. per
//! `(venue, symbol, kind)` already, so the same key reused across every token_id in one row group
//! collides with nothing). The row-group suffix (rather than one key per whole date) is
//! load-bearing for the streaming design below, not cosmetic — see [`ingest_stream_over`]'s doc.
//!
//! **Streaming ingest (memory-bounded, not whole-file buffered).** [`ArchiveClient::ingest_stream`]
//! processes row groups ONE AT A TIME: for each selected row-group index, it builds a fresh
//! `ParquetRecordBatchReader` scoped to JUST that group (via `.with_row_groups(vec![idx])`, reusing
//! one cached `ArrowReaderMetadata` footer so re-scoping never re-fetches it), decodes it, appends
//! its rows to the `HistStore` immediately, and drops everything before moving to the next index —
//! so resident memory is bounded by one row group's decoded rows (tens of MB per the module intro),
//! never by the whole file or the whole date range. This replaced an earlier version that grouped
//! EVERY row of the ENTIRE reader into one `HashMap<String, Vec<_>>` before writing anything —
//! `per_symbol` was populated by draining the full `for batch in reader` loop first, so nothing hit
//! disk and resident memory scaled with (row groups × rows) until the whole stream had been decoded
//! (measured live: a ~9.3 GB-compressed two-day book_events pull grew RSS to 6 GB with the store at
//! 0 bytes on disk for 11+ minutes). The per-row-group commit key is the necessary
//! consequence: a single shared per-date key would make every write AFTER the first one for a given
//! symbol a silent no-op (the store's `commit_rows` treats a repeated commit key as "already done,
//! Ok(0)" — see `vike-store`'s `datafusion_hist.rs`), since one token's rows can straddle two
//! adjacent row groups in a sorted-by-token_id file. Net effect for a fully-completed run is
//! identical rows in the store either way; the finer key also makes a partial run resumable
//! row-group-by-row-group rather than only date-by-date.
//!
//! **Parallel local-file ingest (`ingest_local_file_parallel`).** The `--file` path (measured:
//! 1m44.8s / 98% CPU / one of 32 cores / 500 MB peak RSS for one BTC-5m `book_events` day,
//! 74.4M rows) is strictly SERIAL — no network latency left to hide behind (that was `--file`'s
//! own fix), so the remaining wall-clock is decode+encode CPU, one core's worth. This function
//! fans the SAME row-group-at-a-time pipeline (`ingest_book`/`ingest_trades`/`ingest_quotes`,
//! identical decode + commit-key logic) across up to `jobs` OS threads via a dedicated
//! `rayon::ThreadPool` (never the global pool — width is an explicit CLI knob, `--jobs N` on
//! `vike_archive_backfill`, not `RAYON_NUM_THREADS`). `chunk_row_groups` splits the selected
//! row groups into `jobs` CONTIGUOUS chunks, one per worker, preserving the file's own
//! `token_id`-sorted row-group order within each chunk: since a token can straddle at most two
//! ADJACENT row groups (never more — see the streaming-ingest paragraph above), a contiguous
//! split means only the `jobs - 1` chunk-boundary tokens can ever see writes from two different
//! workers: every other token's rows are written by exactly one worker end-to-end, so
//! `DataFusionHist`'s per-series file lock (`SeriesLock`, scoped to one `kind=/venue=/symbol=`
//! directory) sees real cross-worker contention only at those few boundaries, not on every
//! commit. Memory stays bounded by `jobs` — the pool has EXACTLY `jobs` threads, each holding at
//! most one row group's decoded rows at a time (the same one-row-group bound the serial path
//! holds), so at most `jobs` row groups are ever resident together; `jobs <= 1` degenerates to a
//! single chunk covering every row group on one thread, decoding/committing in the EXACT same
//! order as [`ingest_local_file`] (byte-identical store output). Progress callbacks fire in
//! COMPLETION order across workers, not file order — [`RowGroupProgress::index`]'s doc covers
//! the distinction. See `.superpowers/sdd/2026-07-28-poly-mm-latency-batch/
//! ingest-parallel-report.md` for the measured scaling and where it plateaus.
//!
//! **A real concurrency bug this parallelization surfaced (fixed, not theoretical):**
//! [`LocalFileReader`] originally delegated straight to `parquet::file::reader`'s own
//! `impl ChunkReader for File`, which reads via `self.try_clone()?.seek(start)?.read(...)`. That
//! is safe SERIALLY (one row group's reads always finish before the next starts) but NOT under
//! concurrency: `File::try_clone` shares the underlying open-file-description's cursor with the
//! original handle, so two worker threads racing a `seek`+`read` pair over clones of the ONE
//! `Arc<File>` this type wraps can splice each other's reads — one thread's `seek` moves the
//! cursor a second thread is mid-`read` against. This reproduced live on the CI box the first time
//! `ingest_local_file_parallel` ran with `--jobs 4`: a corrupted read fed the zstd decoder garbage
//! and decode failed with "External: Unknown frame descriptor" (never seen at `--jobs 1`). Fixed
//! by reading via `read_at`/`seek_read` (true positional syscalls — see `read_at` and
//! [`PositionedReader`]) instead, which never touch the shared cursor at all.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReader,
    ParquetRecordBatchReaderBuilder,
};
use datafusion::parquet::file::reader::ChunkReader;

use vike_data::HistStore;
use vike_marketdata::{BookUpdate, QuoteTick, TradeTick};
use vike_store::archive_store::{
    RowScope, VENUE, book_updates_from_batch, quotes_from_l1_batch, select_row_groups,
    trades_from_batch,
};
use vike_store::{BulkIngestSession, DataFusionHist};

use crate::error::CollectError;
use datasets::Stream;

pub mod client;
pub mod datasets;
pub mod local;
pub mod readers;

// Only the module doc and item docs name these moved items, as intra-doc links.
#[cfg(doc)]
use {
    client::ArchiveClient,
    local::{ingest_local_file, ingest_local_file_parallel},
    readers::{HttpRangeReader, LocalFileReader, PositionedReader},
};

/// The default `data.vike.io` archive base (no trailing slash).
pub const DEFAULT_BASE: &str = "https://data.vike.io/archive";
/// The auth header name (verified against the live manifest's own `auth.header` field).
const API_KEY_HEADER: &str = "X-API-Key";

/// Vendor prefix for error messages (mirrors `crate::arrowutil`'s `ctx` convention).
const CTX: &str = "vike-archive";

// ⚠ ROW-GROUP PRUNING stood here and moved DOWN with the reader on 2026-09-20:
// `vike_store::archive_store::select_row_groups` is its home, and this module imports it from there
// (see the `use` at the top). It is a read-side fact about the published files — which groups a
// token set can possibly be in — not a fact about fetching, and holding it HERE is what made the
// reader depend upward.

/// How often [`ArchiveClient::ingest_stream`] logs progress at INFO (every Nth row group, plus
/// always the first and last) — terse enough not to spam a run over hundreds of row groups, while
/// still proving from the outside that the process is alive and making progress (the defect this
/// whole streaming rewrite fixes: previously nothing was observable — and nothing was written to
/// disk — until the entire file had been decoded).
const PROGRESS_LOG_EVERY: usize = 25;

/// One row group's ingest result, reported to [`ArchiveClient::ingest_stream`]'s progress callback.
/// `index` is 0-based position within the SELECTED (pruned) row groups, not the raw Parquet row
/// group index (that one is folded into `compressed_bytes`' lookup only, not exposed — callers care
/// about "how far through this run", not the file's raw row-group numbering). Under
/// [`ingest_local_file_parallel`] specifically, `index` is COMPLETION order (a shared atomic
/// counter incremented as each worker finishes a group) rather than the group's position in the
/// file — several workers finish concurrently, so "which row group is this" in file order isn't a
/// single well-ordered number anymore; completion order still gives every value in `0..total`
/// exactly once, so the existing "first/last/every-Nth" progress-logging idiom keeps working
/// unchanged.
#[derive(Debug, Clone, Copy)]
pub struct RowGroupProgress {
    pub index: usize,
    pub total: usize,
    pub compressed_bytes: i64,
    pub rows_this_group: usize,
    pub rows_cumulative: usize,
    pub elapsed_ms: u64,
}

/// The memory-bounded streaming core behind [`ArchiveClient::ingest_stream`]: processes `reader`'s
/// selected row groups ONE AT A TIME — build a row-group-scoped `ParquetRecordBatchReader` (reusing
/// one cached `ArrowReaderMetadata` footer, so re-scoping to a new group never re-fetches it),
/// decode, append to `store`, drop, repeat — so at most one row group's decoded rows are ever
/// resident. Generic over `T: ChunkReader + Clone` so it is unit-testable over a plain in-memory
/// `Bytes` file (no network involved), exactly like [`select_row_groups`]'s own end-to-end reader
/// test below; [`ArchiveClient::ingest_stream`] is the only real caller, over `HttpRangeReader`.
///
/// Idempotency is scoped PER ROW GROUP (`vikearchive:{kind}:{date}:rg{idx}`), not per date: a single
/// date-wide commit key would make the store's `commit_rows` treat every write after the FIRST one
/// for a given symbol as "already committed" (`Ok(0)`, no-op) — silently dropping that symbol's rows
/// from every row group after its first, since one token's rows can straddle two adjacent row
/// groups in a file sorted by `token_id`. See the module doc's "Streaming ingest" section.
fn ingest_stream_over<T: ChunkReader + Clone + 'static>(
    reader: T,
    store: &DataFusionHist,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    mut on_row_group: impl FnMut(RowGroupProgress),
) -> Result<usize, CollectError> {
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let start = Instant::now();
    let mut rows_cumulative = 0usize;
    for (index, &rg_idx) in selected.iter().enumerate() {
        // `.with_row_groups(vec![rg_idx])` scopes this reader to EXACTLY one row group's column
        // chunks; `reader.clone()`/`arrow_meta.clone()` are both cheap (pool-backed `ureq::Agent` /
        // `Bytes`, and an `Arc`-backed footer respectively) — no new HTTP HEAD, no footer re-fetch.
        let rg_reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(reader.clone(), arrow_meta.clone())
                .with_row_groups(vec![rg_idx])
                .build()
                .map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                        stream.file_name()
                    ))
                })?;
        let commit_key = format!("vikearchive:{}:{date}:rg{rg_idx}", stream.kind());
        let rows_this_group = match stream {
            Stream::Book => ingest_book(rg_reader, store, tokens, &commit_key)?,
            Stream::Trade => ingest_trades(rg_reader, store, tokens, &commit_key)?,
            Stream::Quote => ingest_quotes(rg_reader, store, tokens, &commit_key)?,
        };
        rows_cumulative += rows_this_group;
        on_row_group(RowGroupProgress {
            index,
            total,
            compressed_bytes: md.row_group(rg_idx).compressed_size(),
            rows_this_group,
            rows_cumulative,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        // `rg_reader` (and whatever it decoded) is dropped HERE, before the next iteration ever
        // opens a new row group's reader — the load-bearing property this whole rewrite exists for.
    }
    Ok(rows_cumulative)
}

/// The BULK/offline write-profile twin of [`ingest_stream_over`]: identical row-group-at-a-time
/// decode (same reader re-scoping, same one-row-group-resident-at-a-time memory bound), but each
/// row group's decoded rows are STAGED into `session` (via `end_batch`, so the session's own
/// [`vike_store::BulkConfig`] window decides when a real commit fires) instead of committed
/// straight to `store` one call per symbol per row group. `key_prefix` feeds `end_batch`/`flush`'s
/// own commit-key windowing (see [`ArchiveClient::ingest_stream_bulk`]'s doc); this function does
/// NOT flush the trailing partial window itself — the caller does that once after every stream in
/// a run (letting a caller batch several streams into one session before the final flush).
fn ingest_stream_bulk_over<T: ChunkReader + Clone + 'static>(
    reader: T,
    session: &mut BulkIngestSession<'_>,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
    mut on_row_group: impl FnMut(RowGroupProgress),
) -> Result<usize, CollectError> {
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let start = Instant::now();
    let mut rows_cumulative = 0usize;
    for (index, &rg_idx) in selected.iter().enumerate() {
        let rg_reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(reader.clone(), arrow_meta.clone())
                .with_row_groups(vec![rg_idx])
                .build()
                .map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                        stream.file_name()
                    ))
                })?;
        let rows_this_group = match stream {
            Stream::Book => stage_book(session, rg_reader, tokens)?,
            Stream::Trade => stage_trades(session, rg_reader, tokens)?,
            Stream::Quote => stage_quotes(session, rg_reader, tokens)?,
        };
        // one logical "batch" per row group — the session flushes on its own window, not here
        session.end_batch(key_prefix).map_err(|e| {
            CollectError::Fetch(format!("{CTX} bulk flush {date}/{}: {e}", stream.file_name()))
        })?;
        rows_cumulative += rows_this_group;
        on_row_group(RowGroupProgress {
            index,
            total,
            compressed_bytes: md.row_group(rg_idx).compressed_size(),
            rows_this_group,
            rows_cumulative,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
        // `rg_reader` (and whatever it decoded) is dropped HERE, exactly like the live path — the
        // BUFFERED rows it staged into `session` remain resident until the next flush, bounded by
        // `session`'s own `BulkConfig` window, never by the whole file.
    }
    Ok(rows_cumulative)
}

fn keep(tokens: Option<&HashSet<String>>, symbol: &str) -> bool {
    tokens.is_none_or(|allow| allow.contains(symbol))
}

// ---- decode ONE row group's reader into a per-symbol map (shared by the LIVE and BULK commit
// paths below — only what happens to the decoded map differs between the two profiles) ----------
//
// The row decode itself is `vike_store::archive_store`'s — the ONE decoder of these streams,
// which the read-in-place store uses too. `RowScope::All` keeps every row, each attributed to its
// own `token_id`, because a pruned row group still mixes tokens; `keep` filters them below.

/// A batch this module downloaded that does not decode is a FETCH failure (the file is not what
/// the archive publishes), never a store one — the variant the decoder's own copy here returned.
fn decode_failed(e: vike_data::DataError) -> CollectError {
    CollectError::Fetch(format!("{CTX} parquet: {e}"))
}

fn decode_book_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<BookUpdate>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<BookUpdate>> = HashMap::new();
    for batch in reader {
        let batch =
            batch.map_err(|e| CollectError::Fetch(format!("{CTX} book_events batch: {e}")))?;
        for u in book_updates_from_batch(&batch, RowScope::All).map_err(decode_failed)? {
            if keep(tokens, &u.symbol) {
                per_symbol.entry(u.symbol.clone()).or_default().push(u);
            }
        }
    }
    Ok(per_symbol)
}

fn decode_trades_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<TradeTick>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<TradeTick>> = HashMap::new();
    for batch in reader {
        let batch = batch.map_err(|e| CollectError::Fetch(format!("{CTX} trades batch: {e}")))?;
        for t in trades_from_batch(&batch, RowScope::All).map_err(decode_failed)? {
            if keep(tokens, &t.symbol) {
                per_symbol.entry(t.symbol.clone()).or_default().push(t);
            }
        }
    }
    Ok(per_symbol)
}

fn decode_quotes_row_group(
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<HashMap<String, Vec<QuoteTick>>, CollectError> {
    let mut per_symbol: HashMap<String, Vec<QuoteTick>> = HashMap::new();
    for batch in reader {
        let batch =
            batch.map_err(|e| CollectError::Fetch(format!("{CTX} l1_quotes batch: {e}")))?;
        for q in quotes_from_l1_batch(&batch, RowScope::All).map_err(decode_failed)? {
            if keep(tokens, &q.symbol) {
                per_symbol.entry(q.symbol.clone()).or_default().push(q);
            }
        }
    }
    Ok(per_symbol)
}

// ---- LIVE commit: one `HistStore::append_*` call per symbol per row group (unchanged path) -----

fn ingest_book(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_book_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, updates) in per_symbol {
        total += store.append_book_updates(VENUE, &symbol, &updates, Some(commit_key))?;
    }
    Ok(total)
}

fn ingest_trades(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_trades_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, trades) in per_symbol {
        total += store.append_trades(VENUE, &symbol, &trades, Some(commit_key))?;
    }
    Ok(total)
}

fn ingest_quotes(
    reader: ParquetRecordBatchReader,
    store: &DataFusionHist,
    tokens: Option<&HashSet<String>>,
    commit_key: &str,
) -> Result<usize, CollectError> {
    let per_symbol = decode_quotes_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, quotes) in per_symbol {
        total += store.append_quotes(VENUE, &symbol, &quotes, Some(commit_key))?;
    }
    Ok(total)
}

// ---- BULK commit: stage into the session; the session's own window decides when a real commit
// happens (see `vike_store::datafusion_hist::bulk`'s module doc). Decoded row counts are returned so the caller can
// still report per-row-group progress the same way the live path does. --------------------------

fn stage_book(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_book_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, updates) in per_symbol {
        total += updates.len();
        session.stage_book_updates(VENUE, &symbol, &updates);
    }
    Ok(total)
}

fn stage_trades(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_trades_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, trades) in per_symbol {
        total += trades.len();
        session.stage_trades(VENUE, &symbol, &trades);
    }
    Ok(total)
}

fn stage_quotes(
    session: &mut BulkIngestSession<'_>,
    reader: ParquetRecordBatchReader,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let per_symbol = decode_quotes_row_group(reader, tokens)?;
    let mut total = 0usize;
    for (symbol, quotes) in per_symbol {
        total += quotes.len();
        session.stage_quotes(VENUE, &symbol, &quotes);
    }
    Ok(total)
}

#[cfg(test)]
mod test_support;

#[path = "vike_archive_tests.rs"]
#[cfg(test)]
mod vike_archive_tests;
