//! Row-group pruning over already-loaded Parquet metadata.

use std::collections::HashSet;

use datafusion::parquet::file::metadata::ParquetMetaData;

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
pub fn select_row_groups(
    metadata: &ParquetMetaData,
    tokens: Option<&HashSet<String>>,
) -> Vec<usize> {
    let total = metadata.num_row_groups();
    let Some(tokens) = tokens else {
        return (0..total).collect();
    };
    if tokens.is_empty() {
        return Vec::new();
    }
    let Some(col_index) = metadata
        .file_metadata()
        .schema_descr()
        .columns()
        .iter()
        .position(|c| c.name() == "token_id")
    else {
        return (0..total).collect(); // no token_id column found — can't prune, include everything
    };
    (0..total)
        .filter(|&i| {
            let rg = metadata.row_group(i);
            let Some(stats) = rg.column(col_index).statistics() else {
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
