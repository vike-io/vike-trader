//! The `--file` local-Parquet ingest entrypoints: serial, bulk, and the `--jobs N` parallel fan-out.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use datafusion::parquet::arrow::arrow_reader::{
    ArrowReaderMetadata, ArrowReaderOptions, ParquetRecordBatchReaderBuilder,
};
use datafusion::parquet::file::reader::ChunkReader;
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use vike_data::store::archive_store::select_row_groups;
use vike_data::{BulkIngestSession, DataFusionHist};

use super::datasets::Stream;
use super::readers::LocalFileReader;
use super::{
    CTX, PROGRESS_LOG_EVERY, RowGroupProgress, ingest_book, ingest_quotes, ingest_stream_bulk_over,
    ingest_stream_over, ingest_trades, stage_book, stage_quotes, stage_trades,
};
use crate::error::CollectError;

// ---- local-file ingest entrypoints (the `--file` path — no `ArchiveClient` needed: no agent, no
// base URL, no API key) ----------------------------------------------------------------------------

/// Ingest one local Parquet file — already downloaded to disk — for one (date, stream), through the
/// SAME streaming pipeline `ArchiveClient::ingest_stream` uses (`ingest_stream_over`),
/// substituting [`LocalFileReader`] for `HttpRangeReader` as the only difference: no `HEAD`, no
/// per-row-group ranged `GET`s, no Cloudflare round trips — a local `seek`+`read` (backed by the OS
/// page cache) stands in for what would otherwise be one HTTP request per row group. Same
/// idempotent per-row-group commit keys, same one-row-group-resident-at-a-time memory bound, same
/// progress logging cadence. `date`/`stream` are supplied by the caller rather than inferred from
/// `path` — see `vike_archive_backfill`'s module doc for why (a real downloaded file's name has no
/// obligation to match the archive's own `{stream}.parquet` convention — the file this feature was
/// built to measure, `btc5m_2026-07-28.parquet`, is a case in point).
pub fn ingest_local_file(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind().as_str();
    ingest_stream_over(reader, store, date, stream, tokens, |p: RowGroupProgress| {
        let is_boundary = p.index == 0 || p.index + 1 == p.total;
        if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
            tracing::info!(
                %date,
                kind,
                row_group = p.index + 1,
                total_row_groups = p.total,
                bytes_this_group = p.compressed_bytes,
                rows_this_group = p.rows_this_group,
                rows_cumulative = p.rows_cumulative,
                elapsed_ms = p.elapsed_ms,
                "vike-archive: local-file ingest progress"
            );
        }
    })
}

/// BULK ingest fanned across `jobs` workers — the composition of [`ingest_local_file_bulk`]'s
/// staging win with [`ingest_local_file_parallel`]'s fan-out, which previously could not be had
/// together.
///
/// **Why this could not simply reuse one session.** [`BulkIngestSession`] takes `&mut self`, so a
/// `--bulk --jobs N` run silently degraded to serial. The obvious fix — share one session behind a
/// lock — would serialize the very staging it exists to parallelize. The real obstacle is subtler
/// and is a CORRECTNESS one: a session stamps its commit keys `{key_prefix}:w{window_idx}` from its
/// OWN counter, which starts at 0. N sessions sharing one prefix would therefore mint the SAME key
/// (`prefix:w0`, `prefix:w1`, …) for DIFFERENT row sets, and `commit_rows_bulk` treats an
/// already-seen key as a durable no-op — so the second worker to reach a given series would have
/// its rows silently DROPPED. Not a race; a guaranteed, quiet loss.
///
/// So each worker gets its own session under its own key namespace, `{key_prefix}:j{chunk}`. Keys
/// are then unique by construction, and each worker's `window_idx` stays private.
///
/// **Chunking is what makes per-worker sessions cheap.** [`chunk_row_groups`] splits CONTIGUOUSLY,
/// and the file is `token_id`-sorted, so a token straddles at most one chunk boundary: nearly every
/// token's rows land wholly inside one worker, hence one session, hence ONE commit for that series
/// — the collapse the bulk profile exists for is preserved per worker rather than defeated. At most
/// `jobs - 1` tokens (the boundary ones) pay a second commit.
///
/// ⚠ **A resumed run must use the same `--jobs`.** Bulk idempotency is by commit KEY, and the keys
/// now embed the chunk index, which is a function of `(selected row groups, jobs)`. Re-running a
/// crashed `--jobs 4` import as `--jobs 8` re-chunks, mints different keys, and re-writes rows that
/// were already committed — DUPLICATES, not a no-op, because this store never dedups by row value.
/// This is an extension of the constraint bulk already had (the same `BulkConfig` window, since
/// flush boundaries decide which rows share a key), not a new class of hazard — but it is now one
/// more flag that must match on a retry.
///
/// Returns rows staged (not necessarily yet committed); each worker flushes its own trailing window
/// before returning, so unlike [`ingest_local_file_bulk`] the caller does NOT flush afterwards —
/// there is no single session left to flush.
pub fn ingest_local_file_bulk_parallel(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
    jobs: usize,
    cfg: vike_data::BulkConfig,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind().as_str();
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let chunks = chunk_row_groups(&selected, jobs);
    let pool_width = chunks.len().max(1);
    let start = Instant::now();
    let rows_cumulative = AtomicUsize::new(0);
    let groups_done = AtomicUsize::new(0);

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_width)
        .thread_name(|i| format!("vike-archive-bulk-{i}"))
        .build()
        .map_err(|e| {
            CollectError::Fetch(format!("{CTX} rayon pool ({pool_width} threads): {e}"))
        })?;

    pool.install(|| {
        chunks.par_iter().enumerate().try_for_each(
            |(chunk_idx, chunk)| -> Result<(), CollectError> {
                // One session per worker, under its own key namespace (see the ⚠ above).
                let mut session = store.bulk_session(cfg);
                let worker_prefix = format!("{key_prefix}:j{chunk_idx}");
                for &rg_idx in chunk {
                    let rg_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                        reader.clone(),
                        arrow_meta.clone(),
                    )
                    .with_row_groups(vec![rg_idx])
                    .build()
                    .map_err(|e| {
                        CollectError::Fetch(format!(
                            "{CTX} parquet reader {date}/{} rg{rg_idx}: {e}",
                            stream.file_name()
                        ))
                    })?;
                    let rows_this_group = match stream {
                        Stream::Book => stage_book(&mut session, rg_reader, tokens)?,
                        Stream::Trade => stage_trades(&mut session, rg_reader, tokens)?,
                        Stream::Quote => stage_quotes(&mut session, rg_reader, tokens)?,
                    };
                    session.end_batch(&worker_prefix).map_err(|e| {
                        CollectError::Fetch(format!(
                            "{CTX} bulk flush {date}/{}: {e}",
                            stream.file_name()
                        ))
                    })?;
                    let cumulative = rows_cumulative.fetch_add(rows_this_group, Ordering::Relaxed)
                        + rows_this_group;
                    let index = groups_done.fetch_add(1, Ordering::Relaxed);
                    let is_boundary = index == 0 || index + 1 == total;
                    if is_boundary || (index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                        tracing::info!(
                            %date,
                            kind,
                            jobs,
                            row_groups_done = index + 1,
                            total_row_groups = total,
                            bytes_this_group = md.row_group(rg_idx).compressed_size(),
                            rows_this_group,
                            rows_cumulative = cumulative,
                            elapsed_ms = start.elapsed().as_millis() as u64,
                            "vike-archive: local-file BULK parallel ingest progress (completion order, not file order)"
                        );
                    }
                }
                // Each worker flushes its OWN trailing window — there is no shared session for the
                // caller to flush afterwards.
                session.flush(&worker_prefix).map_err(|e| {
                    CollectError::Fetch(format!(
                        "{CTX} bulk final flush {date}/{}: {e}",
                        stream.file_name()
                    ))
                })?;
                Ok(())
            },
        )
    })?;

    Ok(rows_cumulative.load(Ordering::Relaxed))
}

/// BULK/offline write-profile twin of [`ingest_local_file`] — the same relationship
/// `ArchiveClient::ingest_stream_bulk` has to `ArchiveClient::ingest_stream`: each row group's
/// decoded rows are STAGED into `session` instead of committed straight to the store. See
/// `ArchiveClient::ingest_stream_bulk`'s doc for the flush contract this shares verbatim — the
/// caller must still call `session.flush(key_prefix)` once more after this returns to durably
/// commit the trailing partial window.
pub fn ingest_local_file_bulk(
    session: &mut BulkIngestSession<'_>,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    key_prefix: &str,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind().as_str();
    ingest_stream_bulk_over(
        reader,
        session,
        date,
        stream,
        tokens,
        key_prefix,
        |p: RowGroupProgress| {
            let is_boundary = p.index == 0 || p.index + 1 == p.total;
            if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
                tracing::info!(
                    %date,
                    kind,
                    row_group = p.index + 1,
                    total_row_groups = p.total,
                    bytes_this_group = p.compressed_bytes,
                    rows_this_group = p.rows_this_group,
                    rows_cumulative = p.rows_cumulative,
                    elapsed_ms = p.elapsed_ms,
                    "vike-archive: local-file bulk ingest progress (staged; not necessarily flushed yet)"
                );
            }
        },
    )
}

// ---- parallel local-file ingest (the `--jobs N` fan-out; LOCAL FILE ONLY — see the module doc's
// "Parallel local-file ingest" section for the full design and why the HTTP path is untouched) ----

/// Split `selected` (row-group indices, already in the file's own `token_id`-sorted order — see
/// [`select_row_groups`]) into up to `jobs` CONTIGUOUS chunks of as-equal-as-possible length (the
/// remainder distributed one-per-chunk to the first chunks) — the fan-out unit
/// [`ingest_local_file_parallel`] hands one chunk to each worker thread. Contiguous (not
/// round-robin/interleaved) is load-bearing: since a token can straddle at most two ADJACENT row
/// groups, a contiguous split means at most one token sits on each of the `jobs - 1` chunk
/// boundaries — every other token's rows land entirely within one chunk, hence one worker,
/// avoiding cross-worker writes to the same store series. `jobs == 0` is treated as 1 (never
/// returns zero chunks for a non-empty `selected`); `jobs` above `selected.len()` clamps down to
/// `selected.len()` (one row group per chunk, never an empty chunk). An empty `selected` (a token
/// filter that matched nothing) returns no chunks at all.
fn chunk_row_groups(selected: &[usize], jobs: usize) -> Vec<Vec<usize>> {
    if selected.is_empty() {
        return Vec::new();
    }
    let jobs = jobs.max(1).min(selected.len());
    let base = selected.len() / jobs;
    let rem = selected.len() % jobs;
    let mut out = Vec::with_capacity(jobs);
    let mut start = 0;
    for i in 0..jobs {
        let len = base + usize::from(i < rem);
        out.push(selected[start..start + len].to_vec());
        start += len;
    }
    out
}

/// The parallel twin of [`ingest_stream_over`]: `T: Sync` (rather than that function's plain
/// `Clone`) because `reader`/`arrow_meta` are shared BY REFERENCE across a `rayon::ThreadPool`'s
/// worker threads, each cloning its own handle before scoping to a row group — cheap for
/// [`LocalFileReader`] (an `Arc<File>` clone) exactly as it is for the serial path.
/// [`chunk_row_groups`] partitions the selected row groups into `jobs` contiguous chunks; each
/// pool thread drains its OWN chunk serially (one row group resident at a time, identical to the
/// serial core), so at most `jobs` row groups are ever mid-decode together. `on_row_group` is
/// called from whichever worker thread finishes a group (see [`RowGroupProgress::index`]'s doc
/// for the completion-order caveat this implies) — `Fn + Sync`, not the serial core's `FnMut`,
/// because it is invoked concurrently. `jobs <= 1` builds a ONE-thread pool over a SINGLE chunk
/// covering every selected row group in file order — the exact decode/commit sequence
/// [`ingest_stream_over`] itself would produce, so a `jobs=1` caller gets byte-identical output.
fn ingest_local_over_parallel<T>(
    reader: T,
    store: &DataFusionHist,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    jobs: usize,
    on_row_group: impl Fn(RowGroupProgress) + Sync,
) -> Result<usize, CollectError>
where
    T: ChunkReader + Clone + Sync + 'static,
{
    let arrow_meta =
        ArrowReaderMetadata::load(&reader, ArrowReaderOptions::default()).map_err(|e| {
            CollectError::Fetch(format!("{CTX} parquet open {date}/{}: {e}", stream.file_name()))
        })?;
    let md = arrow_meta.metadata().clone();
    let selected = select_row_groups(&md, tokens);
    let total = selected.len();
    let chunks = chunk_row_groups(&selected, jobs);
    let pool_width = chunks.len().max(1);
    let start = Instant::now();
    let rows_cumulative = AtomicUsize::new(0);
    let groups_done = AtomicUsize::new(0);

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_width)
        .thread_name(|i| format!("vike-archive-ingest-{i}"))
        .build()
        .map_err(|e| {
            CollectError::Fetch(format!("{CTX} rayon pool ({pool_width} threads): {e}"))
        })?;

    pool.install(|| {
        chunks.par_iter().try_for_each(|chunk| -> Result<(), CollectError> {
            for &rg_idx in chunk {
                let rg_reader = ParquetRecordBatchReaderBuilder::new_with_metadata(
                    reader.clone(),
                    arrow_meta.clone(),
                )
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
                let cumulative =
                    rows_cumulative.fetch_add(rows_this_group, Ordering::Relaxed) + rows_this_group;
                let index = groups_done.fetch_add(1, Ordering::Relaxed);
                on_row_group(RowGroupProgress {
                    index,
                    total,
                    compressed_bytes: md.row_group(rg_idx).compressed_size(),
                    rows_this_group,
                    rows_cumulative: cumulative,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                });
                // `rg_reader` (and whatever it decoded) drops HERE, before this worker's next row
                // group — the same one-row-group-resident-per-thread bound the serial path holds,
                // just multiplied by `pool_width` workers instead of held to exactly one.
            }
            Ok(())
        })
    })?;

    Ok(rows_cumulative.load(Ordering::Relaxed))
}

/// Ingest one local Parquet file using up to `jobs` worker threads fanned across row groups — the
/// parallel twin of [`ingest_local_file`]. See the module doc's "Parallel local-file ingest"
/// section for the full design (fan-out unit, memory bound, why write contention stays low) and
/// [`chunk_row_groups`]/[`ingest_local_over_parallel`] for the mechanics. `jobs` is clamped to at
/// least 1 by [`chunk_row_groups`]; a caller wanting the OLD byte-for-byte serial behavior should
/// pass `jobs = 1` (identical decode/commit order to [`ingest_local_file`]) rather than call that
/// function directly, though both remain available and behave identically at `jobs = 1`.
pub fn ingest_local_file_parallel(
    store: &DataFusionHist,
    path: &Path,
    date: &str,
    stream: Stream,
    tokens: Option<&HashSet<String>>,
    jobs: usize,
) -> Result<usize, CollectError> {
    let reader = LocalFileReader::open(path)?;
    let kind = stream.kind().as_str();
    ingest_local_over_parallel(reader, store, date, stream, tokens, jobs, |p: RowGroupProgress| {
        let is_boundary = p.index == 0 || p.index + 1 == p.total;
        if is_boundary || (p.index + 1).is_multiple_of(PROGRESS_LOG_EVERY) {
            tracing::info!(
                %date,
                kind,
                jobs,
                row_groups_done = p.index + 1,
                total_row_groups = p.total,
                bytes_this_group = p.compressed_bytes,
                rows_this_group = p.rows_this_group,
                rows_cumulative = p.rows_cumulative,
                elapsed_ms = p.elapsed_ms,
                "vike-archive: local-file parallel ingest progress (completion order, not file order)"
            );
        }
    })
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod local_tests;
