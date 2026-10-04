//! DataFusion read paths: manifest-driven file selection, then in-engine ts filter.
//!
//! **Per-file reads for schema tolerance.** The `Vec` read
//! ([`collect_for_symbol`](DataFusionHist::collect_for_symbol)) reads each selected part on its OWN
//! `read_parquet` (never one listing over all parts): a series mixing parts written under different
//! schema revisions (an additive nullable column) would make a single listing resolve one schema and
//! fail or mis-project. Reading per file lets each part decode under its own inferred schema (decoders
//! read columns BY NAME), and the `Vec` callers sort rows afterwards so cross-file batch order is
//! irrelevant.
//!
//! **There is NO streaming scan, of any kind.** The three that existed — `load_bars_stream` and the
//! tick twins `scan_trades_stream`/`scan_quotes_stream` — were deleted (2026-10-01) because nothing
//! outside their own tests ever called them. The tick twins were also wrong, not merely unused: they
//! read a symbol's per-symbol directory alone and so could never return a row a grouped series
//! holds. The bar twin had no such flaw (a bar series has no grouped layout), only no caller. What
//! bounds a read here is a BUDGET ([`DataFusionHist::collect_head`], below), not a stream, and a
//! streaming read built later has to walk every layout a symbol can live in, as that does.
//!
//! **Bounded-memory EDGES.** [`DataFusionHist::ts_edges`] is the bounded read that returns no rows at
//! all: the first `ts`, the last `ts` and the row count of a range, from an aggregate over the `ts`
//! column of each selected part. It is what `HistStore::bar_edges` answers from, and it exists so a
//! caller that only wants to know what a range LOOKS like never has to decode it.
//!
//! **Bounded-memory HEADS.** [`DataFusionHist::collect_head`] reads the START of a range and stops:
//! it walks the parts in `ts` order a block at a time, under [`narrow_to_budget`], until `n` rows of
//! the range are in or the range is exhausted. It is what `HistStore::load_bars_head`,
//! `HistStore::scan_exec_fills_head` and every capped read (the four tick kinds and the three
//! research kinds, `HistStore::scan_quotes_capped` and its siblings) answer from, and it exists so
//! a capped page of a long series costs the page, not the series. A tick read walks the parts of
//! EVERY layout a symbol can live in — its per-symbol series and each `group=` directory — under
//! ONE budget, because a budget run per layout answers each completely up to a DIFFERENT point.

use std::path::{Path, PathBuf};

use datafusion::arrow::record_batch::RecordBatch;
// Aliased rather than importing `min`/`max`/`count` by name: those read as the std/iterator methods
// this file also calls (`e.min(cut)`, `hi.max(..)`), and a bare free `min(col("ts"))` beside them is
// a misread waiting to happen.
use datafusion::functions_aggregate::expr_fn as agg;
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext, col, lit};

use crate::hist::{BarEdges, DataError, TsRange};
use crate::series::SeriesId;

use super::codec::{SeriesCodec, i64_col, str_values_required};
use super::manifest::{FileEntry, read_manifest};
use super::{DataFusionHist, file_url, part_dir, q};

/// One directory a [`DataFusionHist::collect_head`] read draws parts from.
///
/// `symbol: None` is a series that holds ONE symbol — the per-symbol layout, and every bar series —
/// so its rows need no judging. `symbol: Some(s)` is a `group=` directory whose parts hold many
/// symbols: the `symbol_col == s` predicate is pushed into the read, and the caller's `keep` then
/// judges each decoded row.
pub(super) struct Layout<'a> {
    pub(super) dir: PathBuf,
    pub(super) symbol: Option<&'a str>,
}

/// A manifest part, tagged with the index of the [`Layout`] it belongs to — what lets ONE budget
/// run over the parts of several directories at once.
#[derive(Clone, Copy)]
struct LayoutPart<'m> {
    layout: usize,
    entry: &'m FileEntry,
}

/// Fold one batch of [`DataFusionHist::ts_edges`]' per-part aggregate (`first_ts`, `last_ts`,
/// `rows`) into the running answer: min of the mins, max of the maxes, sum of the counts.
///
/// A part with no row in range aggregates to `first_ts`/`last_ts` = NULL and `rows` = 0, and a
/// NULL is SKIPPED here rather than read: `Int64Array::value` on a NULL slot returns whatever the
/// buffer holds (usually 0), and a phantom `0` would win every `min` on a series of recent bars.
/// Iterating the array's `Option`s is what makes "no rows" impossible to mistake for "row at 0".
fn fold_edges(acc: &mut BarEdges, batch: &RecordBatch) -> Result<(), DataError> {
    for v in i64_col(batch, "first_ts")?.iter().flatten() {
        acc.first_ts = Some(acc.first_ts.map_or(v, |cur| cur.min(v)));
    }
    for v in i64_col(batch, "last_ts")?.iter().flatten() {
        acc.last_ts = Some(acc.last_ts.map_or(v, |cur| cur.max(v)));
    }
    for n in i64_col(batch, "rows")?.iter().flatten() {
        acc.rows += u64::try_from(n).map_err(q)?;
    }
    Ok(())
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
    ///
    /// ⚠ **No budget here.** A capped read is [`Self::collect_head`]'s, which walks every layout a
    /// symbol lives in under ONE budget; a budget applied to one directory at a time is the defect
    /// that walk exists to remove.
    pub(super) fn collect_for_symbol(
        &self,
        dir: &Path,
        range: TsRange,
        symbol: Option<&str>,
    ) -> Result<Vec<RecordBatch>, DataError> {
        if !dir.exists() {
            return Ok(Vec::new()); // unknown series → legitimately empty
        }
        let m = read_manifest(dir)?;
        let sel = overlapping_in_ts_order(&m.files, range);
        self.read_parts(dir, &sel, range, symbol)
    }

    /// The bounded HEAD read behind `HistStore::load_bars_head`, `HistStore::scan_exec_fills_head`
    /// and every `scan_*_capped` read (four tick kinds, three research kinds, one layout each for
    /// the research kinds and for bars): the
    /// rows of `layouts` in `range`, `C`-decoded and ts-ascending, as a COMPLETE PREFIX of the whole
    /// read of those layouts, holding AT LEAST `n` rows unless the range is exhausted. `ctx` is the
    /// codec's decode argument, as for `scan_series`; `keep` judges the decoded rows of a GROUPED
    /// layout and is never asked about a per-symbol one. `n == usize::MAX` is the whole read, which
    /// is how the uncapped tick scans reach this same code.
    ///
    /// **ONE budget over the UNION of the layouts.** A tick symbol can live in its own per-symbol
    /// series AND in any `group=` directory of its kind, and the parts of all of them are sorted
    /// together by `(ts_min, ts_max)` before [`narrow_to_budget`] sees them, so a pass takes one
    /// block out of the union and clamps it once. The clamp argument below does not care which
    /// directory a part came from — every part left, from any layout, begins past the clamp — so
    /// the merged answer is a complete prefix BY CONSTRUCTION. Narrowing each layout on its own
    /// (what this replaced) answers each completely up to its OWN clamp: the merged page is then
    /// complete only up to the smallest, a pager continues from its last row, and the rows between
    /// the clamps were lost with no error.
    ///
    /// **A loop of narrowed blocks, and the loop is the point.** Each pass hands the parts not yet
    /// read to [`narrow_to_budget`] with what is still missing, reads the block it selects over the
    /// range it CLAMPED to, and continues one past that clamp. That is the existing paging rule run
    /// inside the store, and it has to run inside the store: `narrow_to_budget` counts a part's
    /// TOTAL rows — including rows before the range's start and, in a grouped part, the rows of
    /// EVERY other symbol in it — so one block can come back holding a handful of rows, or none,
    /// while the range continues. A pager that receives an empty answer stops, so before this loop
    /// a symbol absent from its group's first part read as an EMPTY series. Only here, where the
    /// clamp is known, can "short" be told apart from "done".
    ///
    /// **Why the blocks join into a prefix of the whole read.** `narrow_to_budget` clamps one before
    /// the first part it did NOT take, and every part it did take ends at or before that clamp, so a
    /// block is complete for `[lo, clamp]`, the parts left all begin past it, and no timestamp can be
    /// split between two blocks. Within a timestamp, the rows keep the order a whole read gives them:
    /// each block reads the layouts in their given order and each layout's parts in the same
    /// `(ts_min, ts_max)` order a whole read does, and a stable sort keeps it.
    ///
    /// **Memory is the answer plus one block of the symbol's own rows.** A block is the parts that
    /// reach the missing count plus any part straddling them; only the symbol's rows of them are
    /// decoded (a grouped read pushes its predicate down), and they are dropped once moved into the
    /// answer. A part never spans two UTC dates (`commit_rows` seals one part per date, and
    /// compaction merges within one), so on a tidy series a block overshoots by at most about a day
    /// of rows per part it takes. The manifests rule out every part outside the range without a
    /// directory listing, and the loop never opens a part past the block that completed the count.
    ///
    /// ⚠ **The cost a SPARSE symbol pays.** A block is sized by its parts' total rows, so a symbol
    /// with few rows in a busy group gets one part per pass, and a read that must reach `n` of its
    /// rows (or the end) opens every part between — each pruned by the `symbol_col` statistics. That
    /// is what every page of such a read cost before budgets existed; here it is paid once per call.
    ///
    /// ⚠ **`n == 0` answers empty WITHOUT reading.** `narrow_to_budget` reads a zero budget as NO
    /// budget, so letting `0` through would read the whole range to answer with nothing.
    pub(super) fn collect_head<C: SeriesCodec>(
        &self,
        layouts: &[Layout<'_>],
        range: TsRange,
        ctx: &str,
        n: usize,
        keep: impl Fn(&C::Row) -> bool,
    ) -> Result<Vec<C::Row>, DataError> {
        if n == 0 {
            return Ok(Vec::new()); // nothing asked for
        }
        // Every layout's manifest, read once. An absent directory is an unknown series and holds
        // nothing, exactly as `collect_for_symbol` treats one.
        let mut manifests = Vec::with_capacity(layouts.len());
        for l in layouts {
            manifests.push(if l.dir.exists() { Some(read_manifest(&l.dir)?) } else { None });
        }
        let files: Vec<Option<&[FileEntry]>> =
            manifests.iter().map(|m| m.as_ref().map(|m| m.files.as_slice())).collect();
        let sel = union_in_ts_order(&files, range);
        let mut out: Vec<C::Row> = Vec::new();
        let mut lo = range.start;
        let mut read = 0usize; // parts already read: always a prefix of `sel`
        while read < sel.len() {
            let rest = &sel[read..];
            // `narrow_to_budget` only ever TRUNCATES the list it is handed, so the block it selects
            // is `rest`'s first `block.len()` parts, layout tags and all.
            let mut block: Vec<&FileEntry> = rest.iter().map(|p| p.entry).collect();
            let answered = narrow_to_budget(
                &mut block,
                TsRange { start: lo, end: range.end },
                Some(n - out.len()),
            );
            let taken = &rest[..block.len()];
            let mut rows: Vec<C::Row> = Vec::new();
            for (i, l) in layouts.iter().enumerate() {
                let parts: Vec<&FileEntry> =
                    taken.iter().filter(|p| p.layout == i).map(|p| p.entry).collect();
                for b in &self.read_parts(&l.dir, &parts, answered, l.symbol)? {
                    let decoded = C::decode(b, ctx)?;
                    match l.symbol {
                        Some(_) => rows.extend(decoded.into_iter().filter(&keep)),
                        None => rows.extend(decoded),
                    }
                }
            }
            rows.sort_by_key(C::sort_key);
            out.extend(rows);
            // `narrow_to_budget` hands the range back untouched exactly when it took every part, so
            // a block as long as `rest` has answered the range to its end.
            if taken.len() == rest.len() || out.len() >= n {
                break;
            }
            // Narrowed: the block answered `[lo, clamp]`, the clamp is one before the first part it
            // left, and that part is where the next block starts.
            let next = rest[taken.len()].entry.ts_min;
            debug_assert_eq!(answered.end, Some(next.saturating_sub(1)), "the clamp moved");
            lo = Some(next);
            read += taken.len();
        }
        Ok(out)
    }

    /// Read the parts `sel` of `dir` over `range`, each part on its OWN `read_parquet` with the
    /// exact row-level `ts` filter, and — for a grouped series — the `symbol_col` predicate pushed
    /// into the read. The rows come back unsorted; every caller sorts them.
    fn read_parts(
        &self,
        dir: &Path,
        sel: &[&FileEntry],
        range: TsRange,
        symbol: Option<&str>,
    ) -> Result<Vec<RecordBatch>, DataError> {
        let symbol_filter = symbol.map(|s| s.to_string());
        let urls: Vec<String> =
            sel.iter().map(|f| file_url(&part_dir(dir, &f.date).join(&f.name))).collect();
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
                // ROW-level ts filter is still exact (the manifest's file prune is coarse)
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

    /// The PROJECTION-ONLY twin of [`collect_for_symbol`](Self::collect_for_symbol) for a per-symbol
    /// series: the smallest `ts`, the largest `ts` and the row COUNT of the rows a read of `dir`
    /// over `range` would return — without decoding, or holding, any of them. It is what
    /// `HistStore::bar_edges` answers from.
    ///
    /// **Same rows, by construction.** The FILE-level prune is the manifest's `overlaps` (no
    /// directory LIST) and the ROW-level filters are the same two exact `ts` comparisons, applied
    /// per part, so this and `collect_for_symbol` can only disagree about what they DECODE. What is
    /// asked of each selected part is one aggregate over its `ts` column — a one-column projection —
    /// and the per-part answers fold by min / max / sum. Memory is therefore one part's aggregate
    /// row, independent of how many rows or parts the range spans, where `collect_for_symbol` holds
    /// every row of the range as Arrow and the caller then decodes it a second time.
    ///
    /// ⚠ **It folds EVERY overlapping part; it does not open "the first and the last".** Parts of one
    /// series may overlap in `ts` — a re-fetched window under a new commit key, an out-of-order
    /// backfill, a supersede — so the extremes are not necessarily in the extreme parts, and a
    /// boundary-only shortcut would answer wrongly for exactly those layouts and rightly for every
    /// tidy one. `crates/vike-data/tests/bar_edges.rs`'s
    /// `overlapping_parts_are_all_folded_not_assumed_disjoint` pins it.
    ///
    /// ⚠ **One partition, deliberately** (`SessionConfig::with_target_partitions(1)` below): this
    /// is a proof read of a range the caller just wrote, run inside a data daemon that also serves
    /// the live planes, and it must not fan out over every core to answer a question about two
    /// numbers.
    ///
    /// An absent series directory is an empty answer, exactly as it is for `collect_for_symbol`.
    pub(super) fn ts_edges(&self, dir: &Path, range: TsRange) -> Result<BarEdges, DataError> {
        if !dir.exists() {
            return Ok(BarEdges::default()); // unknown series → legitimately empty
        }
        let m = read_manifest(dir)?;
        // FILE-level prune from the manifest — the same call `collect_for_symbol` makes.
        let urls: Vec<String> = m
            .files
            .iter()
            .filter(|f| f.overlaps(range))
            .map(|f| file_url(&part_dir(dir, &f.date).join(&f.name)))
            .collect();
        if urls.is_empty() {
            return Ok(BarEdges::default());
        }
        self.rt.block_on(async move {
            let ctx =
                SessionContext::new_with_config(SessionConfig::new().with_target_partitions(1));
            let mut edges = BarEdges::default();
            // Per-FILE reads (NOT one listing over all files) for the schema-tolerance reason
            // `collect_for_symbol` gives: this only asks for `ts`, which every part carries, but a
            // single listing over parts of different schema revisions can still fail to resolve.
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
                let df = df
                    .aggregate(
                        vec![],
                        vec![
                            agg::min(col("ts")).alias("first_ts"),
                            agg::max(col("ts")).alias("last_ts"),
                            agg::count(col("ts")).alias("rows"),
                        ],
                    )
                    .map_err(q)?;
                for b in df.collect().await.map_err(q)? {
                    fold_edges(&mut edges, &b)?;
                }
            }
            Ok(edges)
        })
    }

    /// The distinct instrument symbols a series HOLDS in `range`, sorted — the one read that can
    /// name the members of a GROUPED series.
    ///
    /// A grouped series has no `symbol=` path segment: its members live only in the row-level
    /// `symbol_col` column, and [`SeriesId::symbol`] is EMPTY for it by construction. So
    /// [`list_series`](super::DataFusionHist::list_series) can tell you a `group=btc-updown-5m`
    /// exists and cannot tell you what is IN it, while every `scan_*` verb requires the symbol as an
    /// INPUT. Between the two there was no way to ask "what does this group hold?" — which is a
    /// question an operator has (a rolling family mints new instruments continuously, so the answer
    /// is a function of the WINDOW, not a constant) and a probe needs before it can measure
    /// anything per-instrument.
    ///
    /// Same file prune as [`collect_for_symbol`](Self::collect_for_symbol) (manifest index, no
    /// directory LIST) and one projected column, so it decodes `symbol_col` and nothing else.
    /// Per-symbol series answer with their own symbol when the column is present and with an EMPTY
    /// vec when it is not — a part written before `symbol_col` existed carries no symbols to
    /// report, and inventing `id.symbol` for it would be answering from the PATH while claiming to
    /// have read the rows.
    pub fn series_symbols(&self, id: &SeriesId, range: TsRange) -> Result<Vec<String>, DataError> {
        let dir = self.series_dir_of(id);
        if !dir.exists() {
            return Ok(Vec::new()); // unknown series → legitimately empty
        }
        let m = read_manifest(&dir)?;
        let urls: Vec<String> = m
            .files
            .iter()
            .filter(|f| f.overlaps(range))
            .map(|f| file_url(&part_dir(&dir, &f.date).join(&f.name)))
            .collect();
        if urls.is_empty() {
            return Ok(Vec::new());
        }
        self.rt.block_on(async move {
            let ctx = SessionContext::new();
            let mut out: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for url in urls {
                // Per-FILE read for the same schema-tolerance reason `collect_for_symbol` gives.
                let mut df =
                    ctx.read_parquet(vec![url], ParquetReadOptions::default()).await.map_err(q)?;
                if df.schema().field_with_unqualified_name("symbol_col").is_err() {
                    continue; // a part predating the column holds no symbols to report
                }
                if let Some(s) = range.start {
                    df = df.filter(col("ts").gt_eq(lit(s))).map_err(q)?;
                }
                if let Some(e) = range.end {
                    df = df.filter(col("ts").lt_eq(lit(e))).map_err(q)?;
                }
                let df = df.select(vec![col("symbol_col")]).map_err(q)?.distinct().map_err(q)?;
                for b in df.collect().await.map_err(q)? {
                    out.extend(
                        str_values_required(&b, "symbol_col")?
                            .into_iter()
                            .filter(|s| !s.is_empty()),
                    );
                }
            }
            Ok(out.into_iter().collect())
        })
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

/// The parts of a series that overlap `range`, in ascending `(ts_min, ts_max)` order — the FILE-level
/// prune from the manifest, with no directory LIST (spec must-fix #5).
///
/// ⚠ **A ts ORDER before any budget**, because without one "the first N parts" names an ARBITRARY
/// set — `read_manifest` returns APPEND order. A narrowed read has to be a PREFIX of the range, or
/// the caller continues past rows it never received. The sort is stable, so parts with equal bounds
/// keep append order, which is what fixes the order of a timestamp's rows across parts.
fn overlapping_in_ts_order(files: &[FileEntry], range: TsRange) -> Vec<&FileEntry> {
    let mut sel: Vec<&FileEntry> = files.iter().filter(|f| f.overlaps(range)).collect();
    sel.sort_by_key(|f| (f.ts_min, f.ts_max));
    sel
}

/// [`overlapping_in_ts_order`] over SEVERAL layouts at once: every part of every layout that
/// overlaps `range`, as ONE list in ascending `(ts_min, ts_max)` order, each tagged with the index
/// of the layout it came from (`None` is a layout whose directory does not exist, and contributes
/// nothing). It is the list [`DataFusionHist::collect_head`] hands to [`narrow_to_budget`], so ONE
/// clamp is computed over the parts of every layout together.
///
/// Appended layout by layout and then STABLE-sorted, so parts with equal bounds keep layout order
/// and, within one layout, the order `overlapping_in_ts_order` gave them — which is what makes the
/// order of a timestamp's rows across layouts the same in every block as in a whole read.
fn union_in_ts_order<'m>(
    layouts: &[Option<&'m [FileEntry]>],
    range: TsRange,
) -> Vec<LayoutPart<'m>> {
    let mut sel: Vec<LayoutPart<'m>> = Vec::new();
    for (layout, files) in layouts.iter().copied().enumerate() {
        let Some(files) = files else { continue };
        sel.extend(
            overlapping_in_ts_order(files, range)
                .into_iter()
                .map(|entry| LayoutPart { layout, entry }),
        );
    }
    sel.sort_by_key(|p| (p.entry.ts_min, p.entry.ts_max));
    sel
}

/// Narrow `sel` to a ROW BUDGET and return the range the narrowed set answers COMPLETELY.
///
/// ⚠ **Narrowing the PARTS, not truncating the RESULT, is the whole point.** Every `HistStore` scan
/// on this backend materializes its entire range — [`DataFusionHist::collect_for_symbol`] collects
/// every selected part into one `Vec<RecordBatch>` and the decoder then builds a second full `Vec`
/// beside it — so a cap applied to the ANSWER bounds the frame and not the allocation. Measured on
/// the live store: one symbol's `kind=depth` series is 3.5 billion level-rows, and its worst single
/// day is about 27 GB decoded. That allocation happens in the data daemon, beside a live
/// order-signing daemon.
///
/// **Why a CLAMP and not merely fewer parts.** The caller's answer is sorted by `ts`, and a paging
/// reader continues from `last_ts + 1`. Dropping parts without moving `end` would answer
/// "everything in `[start, end]`" with a set missing rows INSIDE it — the reader would step past
/// them and lose them in silence, which is the failure this whole seam is built to avoid. With the
/// clamp the answer is exactly complete for `[start, cut]`, so continuing past the last row is safe
/// by construction.
///
/// **Why a straddling part comes along.** The cut sits after the taken block, so a part starting
/// inside that block must be taken too — otherwise it would lie within the answered range while
/// being absent from the answer: the same silent hole one level down. Taking it also guarantees
/// `cut >= hi`, so the clamp can never fall before the data it was computed from.
///
/// **Progress outranks the budget.** The first part is always taken, and so is anything overlapping
/// it, even when that alone exceeds the budget. A single part larger than a page is the one shape
/// this cannot bound, and returning nothing would leave a paging reader asking the identical
/// question forever.
///
/// `None` — or a zero budget — is the unnarrowed read, byte-identical to before this existed.
fn narrow_to_budget(sel: &mut Vec<&FileEntry>, range: TsRange, budget: Option<usize>) -> TsRange {
    let Some(budget) = budget.filter(|b| *b > 0) else { return range };
    let mut take = 0usize;
    let mut rows = 0usize;
    let mut hi = i64::MIN;
    while take < sel.len() {
        let f = sel[take];
        let straddles = take > 0 && f.ts_min <= hi;
        if take > 0 && !straddles && rows >= budget {
            break;
        }
        rows += f.rows;
        hi = hi.max(f.ts_max);
        take += 1;
    }
    if take == sel.len() {
        return range; // the whole request fits — nothing to narrow, nothing to clamp
    }
    let cut = sel[take].ts_min.saturating_sub(1);
    sel.truncate(take);
    TsRange { start: range.start, end: Some(range.end.map_or(cut, |e| e.min(cut))) }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn part(ts_min: i64, ts_max: i64, rows: usize) -> FileEntry {
        FileEntry {
            name: format!("part-{ts_min}.parquet"),
            date: "2026-01-01".to_string(),
            ts_min,
            ts_max,
            rows,
            commit_keys: Vec::new(),
        }
    }

    /// The helper under test takes `&mut Vec<&FileEntry>`, so a test owns the entries and lends them.
    fn run(owned: &[FileEntry], range: TsRange, budget: Option<usize>) -> (Vec<i64>, TsRange) {
        let mut sel: Vec<&FileEntry> = owned.iter().collect();
        sel.sort_by_key(|f| (f.ts_min, f.ts_max));
        let out = narrow_to_budget(&mut sel, range, budget);
        (sel.iter().map(|f| f.ts_min).collect(), out)
    }

    /// No budget is the old read: every part, the range untouched.
    #[test]
    fn no_budget_narrows_nothing() {
        let owned = vec![part(0, 9, 100), part(10, 19, 100), part(20, 29, 100)];
        let (taken, r) = run(&owned, TsRange::all(), None);
        assert_eq!(taken, vec![0, 10, 20]);
        assert_eq!(r, TsRange::all(), "the range must be handed back untouched");

        // ...and a ZERO budget is the same thing rather than "ask for nothing".
        let (taken, r) = run(&owned, TsRange::all(), Some(0));
        assert_eq!(taken, vec![0, 10, 20]);
        assert_eq!(r, TsRange::all());
    }

    /// ⚠ The clamp is the assertion that matters: a narrowed read must answer a SMALLER range
    /// completely, never the asked-for range partially.
    #[test]
    fn a_narrowed_read_clamps_the_end_to_just_before_the_first_dropped_part() {
        let owned = vec![part(0, 9, 100), part(10, 19, 100), part(20, 29, 100)];
        let (taken, r) = run(&owned, TsRange::all(), Some(150));
        // 100 rows is under the budget so part 2 is taken too; 200 then crosses it, so part 3
        // is dropped. The clamp is therefore part 3's ts_min minus one — 19, NOT part 1's ts_max.
        assert_eq!(taken, vec![0, 10], "the budget is a soft cap — it stops AFTER crossing it");
        assert_eq!(
            r.end,
            Some(19),
            "the clamp is one before the first DROPPED part's ts_min, not the last taken part's \
             ts_max — anything else would answer a range holding rows it did not return"
        );
        assert_eq!(r.start, None, "the low bound is the caller's and is never moved");
    }

    /// The whole request fitting is not a narrowing, and must not clamp.
    #[test]
    fn a_request_that_fits_is_returned_whole() {
        let owned = vec![part(0, 9, 10), part(10, 19, 10)];
        let (taken, r) = run(&owned, TsRange::of(0, 100), Some(1_000));
        assert_eq!(taken, vec![0, 10]);
        assert_eq!(r, TsRange::of(0, 100), "an unnarrowed read keeps the caller's own end");
    }

    /// ⚠ A part that STRADDLES the cut comes along, or it would sit inside the answered range while
    /// being absent from the answer — the silent hole this function exists to prevent.
    #[test]
    fn an_overlapping_part_is_taken_with_the_block_it_overlaps() {
        // part B starts INSIDE part A's span; the budget is met after A alone.
        let owned = vec![part(0, 100, 999), part(50, 150, 999), part(200, 300, 999)];
        let (taken, r) = run(&owned, TsRange::all(), Some(1));
        assert_eq!(
            taken,
            vec![0, 50],
            "the straddling part is taken even though the budget is met"
        );
        assert_eq!(r.end, Some(199), "the clamp lands before the first NON-overlapping part");
        assert!(r.end.unwrap() >= 150, "...and never before the data it was computed from");
    }

    /// Progress outranks the budget: one oversized part is still returned, whole.
    #[test]
    fn a_single_part_larger_than_the_budget_is_still_taken() {
        let owned = vec![part(0, 9, 1_000_000), part(10, 19, 1)];
        let (taken, r) = run(&owned, TsRange::all(), Some(10));
        assert_eq!(taken, vec![0], "returning nothing would make a paging reader loop forever");
        assert_eq!(r.end, Some(9));
    }

    /// The caller's own `end` still wins when it is TIGHTER than the clamp.
    #[test]
    fn the_callers_end_is_never_widened() {
        let owned = vec![part(0, 9, 100), part(10, 19, 100), part(20, 29, 100)];
        let (_, r) = run(&owned, TsRange::of(0, 5), Some(150));
        assert_eq!(r.end, Some(5), "a narrowing may only ever tighten the range");
    }

    /// ⚠ **Two layouts, ONE clamp.** A symbol's per-symbol series and its group directory have
    /// parts that interleave in `ts`; `union_in_ts_order` sorts them into one list, so one narrowing
    /// clamps them together and every part it leaves — from EITHER directory — begins past the
    /// clamp. Narrowing each directory on its own (the shape this replaced) gives each its own
    /// clamp, and the second half of this test pins that those differ, which is the whole defect: a
    /// page complete to the larger one is missing the other layout's rows in between.
    #[test]
    fn the_union_of_two_layouts_is_narrowed_under_one_clamp() {
        // Layout 0 (per-symbol): small parts. Layout 1 (a group): one busy part in the middle.
        let series = vec![part(0, 9, 2), part(40, 49, 2), part(80, 89, 2)];
        let group = vec![part(20, 29, 500), part(60, 69, 5)];
        let layouts: [Option<&[FileEntry]>; 3] =
            [Some(series.as_slice()), None, Some(group.as_slice())];
        let union = union_in_ts_order(&layouts, TsRange::all());
        assert_eq!(
            union.iter().map(|p| (p.layout, p.entry.ts_min)).collect::<Vec<_>>(),
            vec![(0, 0), (2, 20), (0, 40), (2, 60), (0, 80)],
            "one list, in ts order, each part tagged with its layout; an absent layout adds nothing"
        );

        let mut block: Vec<&FileEntry> = union.iter().map(|p| p.entry).collect();
        let r = narrow_to_budget(&mut block, TsRange::all(), Some(100));
        assert_eq!(block.len(), 2, "the series' first part, then the busy group part crosses it");
        assert_eq!(r.end, Some(39), "ONE clamp, one before the first part left in EITHER layout");
        for p in &union[block.len()..] {
            assert!(
                p.entry.ts_min > r.end.unwrap(),
                "layout {}'s part at {} lies inside the answered span while absent from the answer",
                p.layout,
                p.entry.ts_min
            );
        }

        // The defect, written down: narrowed one directory at a time, the two clamps disagree.
        let (_, alone_series) = run(&series, TsRange::all(), Some(100));
        let (_, alone_group) = run(&group, TsRange::all(), Some(100));
        assert_eq!(alone_series.end, None, "the small series fits whole and is not clamped");
        assert_eq!(alone_group.end, Some(59), "the busy group is clamped before its second part");
        assert_ne!(alone_series.end, alone_group.end, "two clamps for one answer");
    }
}
