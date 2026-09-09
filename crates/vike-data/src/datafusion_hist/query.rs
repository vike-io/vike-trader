//! DataFusion read paths: manifest-driven file selection, then in-engine ts filter.
//!
//! **Per-file reads for schema tolerance.** Both the `Vec` [`collect`](DataFusionHist::collect) and the
//! streaming scans read each selected part on its OWN `read_parquet` (never one listing over all parts):
//! a series mixing parts written under different schema revisions (an additive nullable column) would
//! make a single listing resolve one schema and fail or mis-project. Reading per file lets each part
//! decode under its own inferred schema (decoders read columns BY NAME), and the `Vec` callers sort rows
//! afterwards so cross-file batch order is irrelevant.
//!
//! **Bounded-memory streaming scans.** [`DataFusionHist::scan_trades_stream`] /
//! [`scan_quotes_stream`](DataFusionHist::scan_quotes_stream) / [`load_bars_stream`](DataFusionHist::load_bars_stream)
//! ride DataFusion's `execute_stream()` instead of `collect()`: RecordBatches are pulled lazily and the
//! returned iterator holds only ONE decoded batch at a time — AND only ONE file's stream open at a time,
//! chained in ts-ascending order (each part exhausts before the next opens). So a multi-year tick range
//! (billions of rows) streams through at O(batch) memory, independent of range or part count, without the
//! `Vec<TradeTick>` materialization the `scan_*` methods do.

use std::path::Path;

use datafusion::arrow::record_batch::RecordBatch;
use datafusion::physical_plan::SendableRecordBatchStream;
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext, col, lit};
use futures_util::StreamExt; // `.next()` on datafusion's async SendableRecordBatchStream (not re-exported)

use vike_model::{Bar, QuoteTick, TradeTick};

use crate::hist::{DataError, TsRange};

use super::codec::{bars_from_batch, quotes_from_batch, trades_from_batch};
use super::manifest::{FileEntry, read_manifest};
use super::{DataFusionHist, file_url, part_dir, q};

/// A lazily-streamed sequence of decoded rows — the public return of the `scan_*_stream` methods.
/// A boxed iterator so NO DataFusion/Arrow type leaks across the crate boundary; `'a` ties it to the
/// borrow of the store (whose owned runtime drives the underlying async stream).
pub type RowStream<'a, T> = Box<dyn Iterator<Item = Result<T, DataError>> + 'a>;

/// Decode one RecordBatch into its rows (one of the `*_from_batch` fns, symbol pre-captured for ticks).
type BatchDecoder<T> = Box<dyn Fn(&RecordBatch) -> Result<Vec<T>, DataError>>;

/// Bounded-memory bridge from a SEQUENCE of per-file async streams to a sync row [`Iterator`].
/// At most ONE decoded batch's rows are resident AND at most ONE file's stream is open: `next()`
/// drains the current batch, and only when it empties does it `block_on` the store's runtime to pull
/// the NEXT batch; when a file's stream ends it opens the NEXT file's stream (each part read as its
/// OWN stream — so parts under different schema revisions each decode under their own inferred
/// schema, and memory stays O(batch), independent of the range or the part count). Holds a shared
/// `&Runtime` borrow (tying the iterator to `&self`) plus the owned, `Send` current stream.
struct BatchStreamIter<'rt, T> {
    rt: &'rt tokio::runtime::Runtime,
    /// Files not yet opened, in ts-ascending READ ORDER; each becomes its own stream in turn.
    urls: std::vec::IntoIter<String>,
    /// Row-level ts filter re-applied to every file (the manifest file prune is coarse).
    range: TsRange,
    /// The currently-open file's lazy stream — `None` before the first file and between files.
    stream: Option<SendableRecordBatchStream>,
    decode: BatchDecoder<T>,
    /// Rows of the current (single resident) batch not yet yielded.
    buf: std::vec::IntoIter<T>,
    /// Latched once every file is drained or a file errored — `next()` is `None` thereafter.
    done: bool,
}

impl<'rt, T> BatchStreamIter<'rt, T> {
    fn new(
        rt: &'rt tokio::runtime::Runtime,
        urls: Vec<String>,
        range: TsRange,
        decode: impl Fn(&RecordBatch) -> Result<Vec<T>, DataError> + 'static,
    ) -> Self {
        Self {
            rt,
            urls: urls.into_iter(),
            range,
            stream: None,
            decode: Box::new(decode),
            buf: Vec::new().into_iter(),
            done: false,
        }
    }
}

impl<T> Iterator for BatchStreamIter<'_, T> {
    type Item = Result<T, DataError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(row) = self.buf.next() {
                return Some(Ok(row));
            }
            if self.done {
                return None;
            }
            let rt = self.rt;
            // Pull the next batch, advancing across files: when the current file's stream ends, open
            // the NEXT file (only then is a new stream/batch resident; the previous file is freed).
            let batch = loop {
                if self.stream.is_none() {
                    match self.urls.next() {
                        Some(url) => match open_file_stream(rt, url, self.range) {
                            Ok(s) => self.stream = Some(s),
                            Err(e) => {
                                self.done = true;
                                return Some(Err(e));
                            }
                        },
                        None => {
                            self.done = true; // no more files
                            return None;
                        }
                    }
                }
                // `self.stream` is Some here; the mutable borrow ends when `block_on` returns.
                let stream = self.stream.as_mut().unwrap();
                match rt.block_on(stream.next()) {
                    Some(Ok(b)) => break b,
                    Some(Err(e)) => {
                        self.done = true;
                        return Some(Err(q(e)));
                    }
                    None => self.stream = None, // this file exhausted → loop opens the next
                }
            };
            match (self.decode)(&batch) {
                Ok(rows) => self.buf = rows.into_iter(),
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}

/// Open ONE part file as a lazy per-row-group stream (manifest already selected it), re-applying the
/// exact row-level ts filter. `execute_stream()` (NOT `collect()`) keeps it O(batch) memory; a single
/// sequential partition (`target_partitions(1)`) means `execute_stream` has nothing to coalesce, so
/// batches arrive in file order. Reading each file on its own → per-file schema tolerance (a part
/// under an older schema revision decodes under its own inferred schema).
fn open_file_stream(
    rt: &tokio::runtime::Runtime,
    url: String,
    range: TsRange,
) -> Result<SendableRecordBatchStream, DataError> {
    rt.block_on(async move {
        let ctx = SessionContext::new_with_config(SessionConfig::new().with_target_partitions(1));
        let mut df = ctx.read_parquet(vec![url], ParquetReadOptions::default()).await.map_err(q)?;
        if let Some(s) = range.start {
            df = df.filter(col("ts").gt_eq(lit(s))).map_err(q)?;
        }
        if let Some(e) = range.end {
            df = df.filter(col("ts").lt_eq(lit(e))).map_err(q)?;
        }
        df.execute_stream().await.map_err(q)
    })
}

impl DataFusionHist {
    // ---- read: manifest-driven file selection, then in-engine ts filter --------------------

    /// [`Self::collect`] plus an optional `symbol_col == symbol` predicate — the read half of
    /// grouped series.
    ///
    /// A grouped part holds every symbol in its group, so without this a one-symbol read decodes
    /// the WHOLE group and filters in memory. Measured cost of exactly that: 562 per-symbol scans
    /// took **2,731 ms** against per-symbol series and **20,945 ms** against one grouped series —
    /// **7.7x slower**, which ate almost all of the 140x write win and left a NET of 1.15x.
    ///
    /// Pushing the predicate into DataFusion lets it skip row groups by the `symbol_col`
    /// statistics. ⚠ That only helps if the part is SORTED BY SYMBOL: in a ts-ordered part with
    /// symbols interleaved, every row group spans every symbol, so each min/max covers everything
    /// and nothing prunes. [`super::DataFusionHist::append_grouped`] sorts symbol-major for exactly
    /// this reason — it is load-bearing, not tidiness. (The archive files prune well for the same
    /// reason: `token_id`-sorted, which gets a 1-of-562 read down to 8.3% of compressed bytes.)
    pub(super) fn collect_for_symbol(
        &self,
        dir: &Path,
        range: TsRange,
        symbol: Option<&str>,
    ) -> Result<Vec<RecordBatch>, DataError> {
        let symbol_filter = symbol.map(|s| s.to_string());
        if !dir.exists() {
            return Ok(Vec::new()); // unknown series → legitimately empty
        }
        let m = read_manifest(dir)?;
        // FILE-level prune from the manifest — no directory LIST (spec must-fix #5)
        let urls: Vec<String> = m
            .files
            .iter()
            .filter(|f| f.overlaps(range))
            .map(|f| file_url(&part_dir(dir, &f.date).join(&f.name)))
            .collect();
        if urls.is_empty() {
            return Ok(Vec::new());
        }
        self.rt.block_on(async move {
            let ctx = SessionContext::new();
            let mut out: Vec<RecordBatch> = Vec::new();
            // Per-FILE reads (NOT one listing over all files): parts written under different schema
            // revisions (an additive nullable column) must each decode under their OWN inferred
            // schema — a single `read_parquet(urls, ..)` resolves one listing schema and can fail or
            // mis-project across mixed parts. The decoders read columns BY NAME per batch, and the
            // callers sort rows afterwards, so cross-file batch order carries no meaning here.
            for url in urls {
                let mut df =
                    ctx.read_parquet(vec![url], ParquetReadOptions::default()).await.map_err(q)?;
                // ROW-level ts filter is still exact (the file prune above is coarse)
                if let Some(s) = range.start {
                    df = df.filter(col("ts").gt_eq(lit(s))).map_err(q)?;
                }
                if let Some(e) = range.end {
                    df = df.filter(col("ts").lt_eq(lit(e))).map_err(q)?;
                }
                // Grouped series only: DataFusion prunes row groups by the `symbol_col` statistics
                // before decoding. Guarded on the column EXISTING, because per-symbol parts written
                // before it was added do not carry it and filtering on an absent column is an error,
                // not an empty result.
                if let Some(ref s) = symbol_filter
                    && df.schema().field_with_unqualified_name("symbol_col").is_ok()
                {
                    df = df.filter(col("symbol_col").eq(lit(s.as_str()))).map_err(q)?;
                }
                out.extend(df.collect().await.map_err(q)?);
            }
            Ok(out)
        })
    }

    // ---- read (STREAMING): bounded-memory scans via `execute_stream` ------------------------
    //
    // `collect` above decodes EVERY selected row-group into one `Vec` — O(range) memory, fine for
    // bounded ranges but an OOM on multi-year tick scans (billions of rows). The `scan_*_stream`
    // twins build the SAME DataFrame (manifest file-prune + row-level ts filter) but `execute_stream()`
    // it, yielding RecordBatches lazily; the returned iterator holds only ONE decoded batch at a time
    // (see [`BatchStreamIter`]). Values are bit-identical to the `Vec` scans (same per-row decoders).

    /// Select the overlapping part URLs for `dir`+`range` in ascending `(ts_min, ts_max)` READ ORDER
    /// exactly as [`Self::collect`] does its file prune (manifest file prune, no directory LIST).
    /// `None` = no overlapping parts (unknown series / empty range) → the caller yields an empty
    /// iterator. The streaming iterator opens each URL as its OWN stream in this order (see
    /// [`open_file_stream`]); for the normal ingest shape — ticks appended in ts order,
    /// `date=`-partitioned into disjoint parts — that per-file read order IS ts-ascending, so the
    /// chained stream equals the (sorted) `Vec` scan; the materialized `scan_*` stays the reference
    /// for a total sort over arbitrarily-ordered parts.
    fn stream_urls(&self, dir: &Path, range: TsRange) -> Result<Option<Vec<String>>, DataError> {
        if !dir.exists() {
            return Ok(None); // unknown series → empty stream
        }
        let m = read_manifest(dir)?;
        // FILE-level prune from the manifest (no directory LIST), same as `collect`
        let mut files: Vec<&FileEntry> = m.files.iter().filter(|f| f.overlaps(range)).collect();
        // deterministic, ts-ascending READ ORDER; stable so equal windows keep manifest (append) order
        files.sort_by_key(|f| (f.ts_min, f.ts_max));
        let urls: Vec<String> =
            files.iter().map(|f| file_url(&part_dir(dir, &f.date).join(&f.name))).collect();
        if urls.is_empty() {
            return Ok(None);
        }
        Ok(Some(urls))
    }

    /// Streaming twin of [`crate::hist::HistStore::scan_trades`]: yields trades in bounded batches
    /// (O(batch) memory, independent of range size) instead of materializing the whole range into a
    /// `Vec`. Values are bit-identical to `scan_trades`; order is ts-ascending for the normal
    /// date-partitioned ingest (see [`Self::open_stream`]). Each yielded item is a `Result` — a
    /// mid-stream decode/IO error surfaces as one `Err` and ends the iteration.
    pub fn scan_trades_stream(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<RowStream<'_, TradeTick>, DataError> {
        let dir = self.ticks_dir("trade", venue, symbol);
        let symbol = symbol.to_string();
        match self.stream_urls(&dir, range)? {
            None => Ok(Box::new(std::iter::empty())),
            Some(urls) => Ok(Box::new(BatchStreamIter::new(&self.rt, urls, range, move |b| {
                trades_from_batch(b, &symbol)
            }))),
        }
    }

    /// Streaming twin of [`crate::hist::HistStore::scan_quotes`] — see [`Self::scan_trades_stream`]
    /// for the memory and ordering contract.
    pub fn scan_quotes_stream(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<RowStream<'_, QuoteTick>, DataError> {
        let dir = self.ticks_dir("quote", venue, symbol);
        let symbol = symbol.to_string();
        match self.stream_urls(&dir, range)? {
            None => Ok(Box::new(std::iter::empty())),
            Some(urls) => Ok(Box::new(BatchStreamIter::new(&self.rt, urls, range, move |b| {
                quotes_from_batch(b, &symbol)
            }))),
        }
    }

    /// Streaming twin of [`crate::hist::HistStore::load_bars`]. Bars are small (one row per bar), so
    /// the `Vec` `load_bars` is usually the right shape — this exists for symmetry / uniform huge-range
    /// reads.
    pub fn load_bars_stream(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<RowStream<'_, Bar>, DataError> {
        let dir = self.bars_dir(venue, symbol, interval);
        match self.stream_urls(&dir, range)? {
            None => Ok(Box::new(std::iter::empty())),
            Some(urls) => {
                Ok(Box::new(BatchStreamIter::new(&self.rt, urls, range, bars_from_batch)))
            }
        }
    }

    /// Read the given part URLs into raw RecordBatches, PER FILE (schema-tolerant: no cross-file
    /// listing-schema unification), unsorted. Bounded to a caller-scoped set (one `date=`). The
    /// compaction path (its only caller) decodes these to domain rows, sorts by ts in domain space,
    /// and re-encodes with the CURRENT schema — so no DataFusion `sort_by` (a global ORDER BY) is
    /// needed here, and parts under different schema revisions each decode under their own schema.
    pub(super) fn read_batches_per_file(
        &self,
        urls: Vec<String>,
    ) -> Result<Vec<RecordBatch>, DataError> {
        self.rt.block_on(async move {
            let ctx = SessionContext::new();
            let mut out: Vec<RecordBatch> = Vec::new();
            for url in urls {
                let df =
                    ctx.read_parquet(vec![url], ParquetReadOptions::default()).await.map_err(q)?;
                out.extend(df.collect().await.map_err(q)?);
            }
            Ok(out)
        })
    }
}
