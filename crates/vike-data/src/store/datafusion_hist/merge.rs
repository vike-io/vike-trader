//! Compaction's merge order and merge planning, and the `date=` part directory they address.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::store::hist::DataError;
use crate::store::hist_maint::CompactionConfig;

use super::codec::{RowSymbolFn, SeriesCodec};
use super::manifest;

/// The symbol a SUPERSEDING merge of one series keys its rows on: `None` for a per-symbol series
/// (the path names the instrument), `C::ROW_SYMBOL` for a grouped one.
///
/// A grouped directory of a kind with NO row symbol is REFUSED rather than superseded: keying it on
/// `sort_key` alone is exactly the defect [`supersede_by_rank`]'s grouped arm exists to close. No
/// writer produces that shape — only quote, trade and book have a grouped write path, and each
/// codec declares its `ROW_SYMBOL` — so this guards a hand-built or future directory, and under
/// `run_maintenance` the refusal is one FAILED series, not a stopped pass. ⚠ The PLAIN merge does
/// not call this: it drops no row in any order, so it merges such a directory in `sort_key` order
/// and warns instead (`compaction.rs`'s `compact_roundtrip_generic`) — a refusal there would also
/// have skipped that series' retention on every pass.
pub(crate) fn grouped_symbol_of<C: SeriesCodec>(
    grouped: bool,
) -> Result<Option<RowSymbolFn<C::Row>>, DataError> {
    if !grouped {
        return Ok(None);
    }
    match C::ROW_SYMBOL {
        Some(symbol) => Ok(Some(symbol)),
        None => Err(DataError::Query(format!(
            "compaction: {} rows carry no symbol, so a GROUPED series of them cannot be merged \
             without treating two instruments' rows at one ts as one — refused",
            std::any::type_name::<C>()
        ))),
    }
}

/// Sort a merge's rows into the order its sealed part is written in — the ONE ordering both
/// compaction paths use, so a superseding pass that drops nothing writes the same part as a plain
/// one.
///
/// Per-symbol (`symbol_of` is `None`): `C::sort_key`, i.e. ts order, exactly as before grouping
/// existed. Grouped: `(symbol, sort_key)` — SYMBOL-MAJOR, the order the grouped write paths
/// (`append_grouped`, `bulk::merge_and_commit`) already write, for the same reason. A one-symbol
/// read prunes by the `symbol_col` statistics, and in a part whose rows interleave symbols in ts
/// order every page spans every symbol. Compaction writes `WriteProfile::Sealed` (1,048,576-row
/// groups), so a merged part is usually ONE row group and the pruning left is the PAGE index
/// (~20K-row pages). MEASURED on a 1M-row, 400-symbol part (the latency box, 2026-10-03): a one-symbol read
/// decoded 20,480 rows (1 of 50 pages, 2.05%) symbol-major against all 1,000,000 ts-sorted, for
/// 12% more bytes. Before this, compaction re-sorted a grouped part by ts and so undid the order
/// its writer chose.
///
/// Both sorts are STABLE, so rows with an equal key keep their input order — parts in manifest
/// order, each in FILE order, because compaction reads every part in ONE partition (`query.rs`'s
/// `read_batches_per_file` says why that pin is load-bearing: for book, the tied rows are one
/// event's levels) — and the output is deterministic. Within one symbol the two orders agree, so a
/// one-symbol read returns the same rows, ties included, under either.
pub(crate) fn sort_for_merge<C: SeriesCodec, T>(
    items: &mut [T],
    row: impl Fn(&T) -> &C::Row,
    symbol_of: Option<RowSymbolFn<C::Row>>,
) {
    match symbol_of {
        None => items.sort_by_key(|t| C::sort_key(row(t))),
        Some(symbol) => items.sort_by(|a, b| {
            let (a, b) = (row(a), row(b));
            symbol(a).cmp(symbol(b)).then_with(|| C::sort_key(a).cmp(&C::sort_key(b)))
        }),
    }
}

/// Source-ranked supersession over rows ALREADY in [`sort_for_merge`] order, each tagged with its
/// part's source rank (LOWER = higher precedence). Within every run of rows sharing the same natural
/// key, keep only the rows whose rank equals the MINIMUM (highest-precedence source) present in that
/// run, dropping the rest as superseded duplicates; returns the kept rows (still key-ascending) and
/// the count dropped.
///
/// Rows that share their run's minimum rank are ALL kept — every level-row of one book event (they
/// share `(ts, seq)`), or several trades in one millisecond, come from ONE source and so ONE rank, so
/// a run touched by a single source drops nothing. That is exactly why a collision-free (single-
/// source) series supersedes byte-identically to the plain [`DataFusionHist::compact_roundtrip`]
/// rewrite. A run holding two sources keeps the whole higher-precedence source and drops the lower —
/// window supersession trusts one authoritative source per key rather than interleaving.
///
/// ## In a GROUPED series the natural key is `(symbol, sort_key)`
///
/// `symbol_of` is `Some` exactly when the rows come from a `group=` directory (see
/// [`grouped_symbol_of`]). There one part holds many instruments, so rows of DIFFERENT symbols can
/// share a `sort_key` without duplicating anything — and keying on `sort_key` alone dropped every
/// lower-ranked source's row of every OTHER symbol at that ts (reproduced through `run_maintenance`
/// by `crates/vike-data/tests/store/grouped_compaction.rs`). So within a `sort_key` run the minimum rank
/// is taken PER SYMBOL. The input order and the output order are unchanged; only which rows are
/// dropped changes, and only for a run that mixes ranks.
///
/// ⚠ The symbol-major merge order does NOT make the per-symbol minimum redundant: a `sort_key` run
/// still spans two symbols wherever one symbol's LAST key equals the next symbol's FIRST, and the
/// tests' fixtures are built on exactly that boundary.
///
/// `None` — every PER-SYMBOL series — is the original key, byte-identical: the path names the one
/// instrument, and a row's symbol cell there may be empty or stamped (see `SeriesCodec::ROW_SYMBOL`).
pub(crate) fn supersede_by_rank<C: SeriesCodec>(
    ranked: Vec<(usize, C::Row)>,
    symbol_of: Option<RowSymbolFn<C::Row>>,
) -> (Vec<C::Row>, usize) {
    let n = ranked.len();
    let mut keep = vec![true; n];
    let mut i = 0;
    while i < n {
        // the run [i, j) of rows sharing this sort key (input is already sort_key-sorted)
        let key = C::sort_key(&ranked[i].1);
        let mut j = i + 1;
        while j < n && C::sort_key(&ranked[j].1) == key {
            j += 1;
        }
        let run = &ranked[i..j];
        let min_rank = run.iter().map(|(r, _)| *r).min().expect("run is non-empty");
        match symbol_of {
            // Per-symbol: drop every row in the run whose source lost to a higher-precedence one at
            // this key.
            None => {
                for (slot, (rank, _)) in keep[i..j].iter_mut().zip(run) {
                    if *rank != min_rank {
                        *slot = false;
                    }
                }
            }
            // Grouped, and the run mixes sources: the precedence contest is per SYMBOL. A run whose
            // rows all share one rank drops nothing either way, so it costs no map.
            Some(symbol) if run.iter().any(|(r, _)| *r != min_rank) => {
                let mut min_by_symbol: BTreeMap<&str, usize> = BTreeMap::new();
                for (rank, row) in run {
                    let best = min_by_symbol.entry(symbol(row)).or_insert(*rank);
                    *best = (*best).min(*rank);
                }
                for (slot, (rank, row)) in keep[i..j].iter_mut().zip(run) {
                    if min_by_symbol.get(symbol(row)) != Some(rank) {
                        *slot = false;
                    }
                }
            }
            Some(_) => {}
        }
        i = j;
    }
    let mut out = Vec::with_capacity(n);
    let mut superseded = 0usize;
    for ((_, row), keep_row) in ranked.into_iter().zip(keep) {
        if keep_row {
            out.push(row);
        } else {
            superseded += 1;
        }
    }
    (out, superseded)
}

/// The `date=YYYY-MM-DD` sub-directory of a series leaf where a part file lives.
pub(crate) fn part_dir(series_dir: &Path, date: &str) -> PathBuf {
    series_dir.join(format!("date={date}"))
}

/// Split one `date=`'s parts (`idxs`, indices into `files`) into consecutive merge groups, each
/// small enough to decode at once.
///
/// This is what makes a compaction pass BOUNDED. Phase 2 decodes a group's parts into Arrow all at
/// once — it must, because the output is sorted and a sort cannot stream — so the group, not the
/// date, is what sets peak memory. Planning one merge per `date=` made peak memory "however much
/// data that day happened to hold", which on the CI box (2026-08-04) was 3,634 parts / 658 MB of zstd
/// Parquet in a 4 GB cgroup: the OOM killer took the recorder down, and the kill left a lock that
/// wedged every restart for 11 h.
///
/// ⚠ **The bound is `max_merge_rows`, NOT `target_bytes`.** Bounding by compressed input was the
/// first fix and it was not enough: measured on that same the CI box series, a 64 MB budget still peaked
/// at **8.95 GB**, because the parts are only 2.5x compressed and the real cost is dictionary/RLE
/// decoding, the sort's copy, and per-file buffering across the ~950 files a byte budget admits.
/// `target_bytes` survives as what it always meant — the sealed-file size to converge toward, and
/// so which parts are already finished.
///
/// Four rules decide the grouping, and each earns its place:
///
///   * **A part at or over `target_bytes` is skipped.** It is finished; folding it in would rewrite
///     the whole thing to absorb a few KB of neighbours (write amplification).
///   * **A part whose OWN row count reaches `max_merge_rows` is skipped too.** It cannot be merged
///     within budget by definition, and without this the next rule would happily pair two of them.
///   * **A group takes at least 2 parts even if that overshoots.** Otherwise a series whose parts
///     each exceed half the budget would compact NOTHING, ever, and the fragment count compaction
///     exists to control would grow without bound. Two parts is the smallest unit of progress, and
///     the rule above caps the overshoot at just under 2x rather than leaving it open.
///   * **A trailing group of ONE part is dropped** — merging one part into one part is a rewrite
///     that changes nothing but the name. UNLESS `min_parts <= 1`, which is the caller saying a
///     lone fragment is worth a pass on its own: a one-part "merge" still re-encodes under the
///     CURRENT schema, which is how an older-schema part gets upgraded.
///
/// Groups otherwise follow manifest order, so each output covers a contiguous run and the date
/// converges toward one part over successive passes. Row counts come from the manifest, so the row
/// bound costs no I/O at all; only the `target_bytes` check needs a stat, and a part that cannot be
/// stat'd is treated as not-yet-at-target rather than failing the pass — the merge surfaces the real
/// error, and a stat is not the place to decide a part is broken.
pub(crate) fn plan_merge_groups(
    dir: &Path,
    files: &[manifest::FileEntry],
    idxs: &[usize],
    cfg: &CompactionConfig,
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_rows: usize = 0;
    for &i in idxs {
        let rows = files[i].rows;
        if rows >= cfg.max_merge_rows {
            continue; // too big to merge inside the memory budget, by itself
        }
        let bytes = std::fs::metadata(dir.join(&files[i].name)).map(|md| md.len()).unwrap_or(0);
        if bytes >= cfg.target_bytes {
            continue; // already at target size — nothing to gain by rewriting it
        }
        if cur.len() >= 2 && cur_rows.saturating_add(rows) > cfg.max_merge_rows {
            groups.push(std::mem::take(&mut cur));
            cur_rows = 0;
        }
        cur.push(i);
        cur_rows = cur_rows.saturating_add(rows);
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    let min_group = cfg.min_parts.clamp(1, 2);
    groups.retain(|g| g.len() >= min_group);
    groups
}
