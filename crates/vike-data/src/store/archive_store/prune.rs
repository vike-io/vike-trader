//! Row-group pruning over already-loaded Parquet metadata, and the per-file pruning plan.

use std::collections::HashSet;
use std::path::PathBuf;

use datafusion::parquet::file::metadata::ParquetMetaData;
use datafusion::parquet::file::statistics::Statistics;

use crate::TsRange;

// ---- row-group pruning (pure over already-loaded metadata; unit-tested with synthetic files) ----

/// Which row-group indices of `md` can possibly overlap `range`, using the `col_name` column's
/// per-row-group MIN/MAX statistics (must be a physical `INT64` column — true of both `ts` and
/// `local_ts` here). Falls back to "include everything" whenever pruning data is unavailable (no
/// such column, no statistics on a group's chunk, or the column is not `Int64`-typed) — pruning is
/// an optimization, never a filter that can silently lose rows. Mirrors
/// `vike_archive::select_row_groups`'s own defensive shape (byte-range containment there; typed
/// numeric overlap here), the `ts`-axis sibling of that `token_id`-axis pruning.
pub fn select_row_groups_by_ts(md: &ParquetMetaData, col_name: &str, range: TsRange) -> Vec<usize> {
    let total = md.num_row_groups();
    let start = range.start.unwrap_or(i64::MIN);
    let end = range.end.unwrap_or(i64::MAX);
    let Some(col_idx) =
        md.file_metadata().schema_descr().columns().iter().position(|c| c.name() == col_name)
    else {
        return (0..total).collect(); // no such column — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = md.row_group(i);
            let Some(stats) = rg.column(col_idx).statistics() else {
                return true; // no stats on this group's chunk — include defensively
            };
            let Statistics::Int64(vs) = stats else {
                return true; // not an Int64-typed statistics — can't reason about it, include
            };
            match (vs.min_opt(), vs.max_opt()) {
                (Some(&min), Some(&max)) => min <= end && max >= start, // range overlap
                _ => true,
            }
        })
        .collect()
}

// ---- the pruning-plan diagnostic (the MEASURE surface) -------------------------------------------

/// One file's row-group-pruning plan for a `(symbol, range)` query — how many of a file's row
/// groups would actually be read vs the whole file, and their compressed byte cost. Mirrors
/// `vike_archive::PrunePlan`'s shape (this module's local, file-path-carrying twin).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePrunePlan {
    pub path: PathBuf,
    pub total_row_groups: usize,
    pub selected_row_groups: usize,
    pub total_compressed_bytes: i64,
    pub selected_compressed_bytes: i64,
}

// ---- the ARCHIVE's own read-side constants, moved down 2026-09-20 ---------------------------
//
// `VENUE` and `select_row_groups` were declared in `vike_backfill::vike_archive` — the HTTP
// DOWNLOADER — and imported up from this reader. They are read-side facts about the published
// files (which venue they carry; which row groups a token set can possibly be in), not about
// fetching, and holding them in the downloader is what forced this whole module to live above
// the engine. They live with the reader now, and the downloader imports them from here, which is
// downward. The pruning function is PURE over already-loaded Parquet metadata: no network, no
// credentials, no venue client.
/// `token_id` column's per-row-group min/max statistics. `None` (no filter) selects every row
/// group. An explicit-but-empty token set selects none. Byte-lexicographic comparison — the exact
/// comparison Parquet defines for `BYTE_ARRAY`/`Utf8` column statistics, and correct regardless of
/// whether the file happens to be globally sorted: a row group's own min/max are always true
/// bounds over that group's own rows, so exclusion never drops a real match. Falls back to
/// "include everything" whenever pruning data is unavailable (no `token_id` column found, or a
/// row group's column carries no statistics) — pruning is an optimization, never a filter that can
/// silently lose rows.
pub fn select_row_groups(md: &ParquetMetaData, tokens: Option<&HashSet<String>>) -> Vec<usize> {
    let total = md.num_row_groups();
    let Some(tokens) = tokens else {
        return (0..total).collect();
    };
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(col_idx) =
        md.file_metadata().schema_descr().columns().iter().position(|c| c.name() == "token_id")
    else {
        return (0..total).collect(); // no token_id column found — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = md.row_group(i);
            let Some(stats) = rg.column(col_idx).statistics() else {
                return true; // no stats on this group's token_id chunk — include defensively
            };
            let (Some(min), Some(max)) = (stats.min_bytes_opt(), stats.max_bytes_opt()) else {
                return true;
            };
            tokens.iter().any(|t| {
                let tb = t.as_bytes();
                min <= tb && tb <= max
            })
        })
        .collect()
}
