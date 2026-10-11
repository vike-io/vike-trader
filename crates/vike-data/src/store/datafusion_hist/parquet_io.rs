//! Parquet part writing and the small file helpers every concern shares (fsync, URL, interval).

use std::path::Path;
use std::sync::Arc;

use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::basic::{Compression, ZstdLevel};
use datafusion::parquet::file::metadata::KeyValue;
use datafusion::parquet::file::properties::WriterProperties;

use crate::store::hist::DataError;
use crate::store::hist_maint::{GROUPED_ROW_GROUP_ROWS, WriteProfile};

use super::codec::SeriesCodec;
use super::{io, q};

/// Write `batches` to one Parquet file. `Hot` = light zstd (live append); `Sealed` = heavier zstd +
/// Parquet footer key under which a part records the commit keys whose rows it holds.
///
/// This is what makes a part SELF-DESCRIBING, and it is the difference between a manifest that can
/// be rebuilt and one that merely looks rebuildable. Every other field of a `FileEntry` is already
/// recoverable from the file itself — `name` from the path, `date` from its `date=` directory,
/// `rows` from the footer's row count, `ts_min`/`ts_max` from the `ts` column's row-group
/// statistics. `commit_keys` was the sole exception, and it is the load-bearing one: it IS the
/// idempotency log, so a rebuild without it would silently re-admit already-applied appends and
/// DUPLICATE rows. Storing it in the part closes that gap, so [`DataFusionHist::rebuild_manifest`]
/// is lossless rather than best-effort.
///
/// Multiple keys are newline-separated — a compacted part carries the union of its inputs' keys.
pub(crate) const COMMIT_KEYS_META: &str = "vike.commit_keys";

/// bounded row groups so a large compacted file keeps page-level `ts` pruning. Encodings are
/// byte-level only — every f64 is bit-identical across profiles (the store's `to_bits()` gate holds).
/// Rows per re-encoded compaction batch.
///
/// **This exists to keep one Arrow array's i32 offsets from overflowing.** A `StringArray` /
/// `BinaryArray` addresses its values with 32-bit offsets, so a SINGLE array cannot hold more than
/// `i32::MAX` (~2 GiB) of bytes — and compaction used to re-encode an entire `date=` partition into
/// ONE `RecordBatch`, i.e. one array per column, no matter how large the day was. Past that limit
/// arrow does not return an error, it PANICS:
///
/// ```text
/// thread 'vike-data-maint' panicked at arrow-array/src/array/byte_array.rs:236:45: offset overflow
/// ```
///
/// which killed the maintenance thread outright (a panic is not an `Err`, so the per-series
/// isolation could not contain it) and left the store with no compaction and no retention until the
/// process restarted. Observed on the CI box 2026-08-02 on a 31 M-row `kind=book` polymarket day whose
/// rows carry a wide JSON payload.
///
/// 262,144 rows keeps a single column under 2 GiB unless rows average more than ~8 KB in ONE column,
/// which no codec in this crate comes near. Chunking costs nothing: a Parquet file holds any number
/// of batches, and the writer's own `max_row_group_row_count` still decides row-group boundaries, so
/// the output file is unchanged in layout and byte-identical in content — only the in-memory arrays
/// feeding it are bounded. Nothing about the manifest, the part names, or the publish path changes.
pub(crate) const COMPACT_BATCH_ROWS: usize = 262_144;

/// Re-encode `rows` under `C`'s current schema as batches of at most [`COMPACT_BATCH_ROWS`] rows.
///
/// Always returns at least one batch, so an empty compaction still writes a valid (empty) part —
/// the shape the single-batch code produced before.
pub(crate) fn encode_chunked<C: SeriesCodec>(
    rows: &[C::Row],
    schema: &Arc<Schema>,
) -> Result<Vec<RecordBatch>, DataError> {
    if rows.is_empty() {
        let idxs: Vec<usize> = Vec::new();
        return Ok(vec![RecordBatch::try_new(schema.clone(), C::columns(rows, &idxs)).map_err(q)?]);
    }
    let mut out = Vec::with_capacity(rows.len().div_ceil(COMPACT_BATCH_ROWS));
    for start in (0..rows.len()).step_by(COMPACT_BATCH_ROWS) {
        let end = (start + COMPACT_BATCH_ROWS).min(rows.len());
        let idxs: Vec<usize> = (start..end).collect();
        out.push(RecordBatch::try_new(schema.clone(), C::columns(rows, &idxs)).map_err(q)?);
    }
    Ok(out)
}

pub(crate) fn write_parquet(
    path: &Path,
    schema: Arc<Schema>,
    batches: Vec<RecordBatch>,
    profile: WriteProfile,
    commit_keys: &[String],
) -> Result<(), DataError> {
    let level = match profile {
        WriteProfile::Hot | WriteProfile::Grouped => 1,
        WriteProfile::Sealed => 3,
    };
    let mut builder = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(level).map_err(q)?));
    match profile {
        // Compaction: big groups. A per-symbol sealed part is scanned by ts range. A GROUPED one is
        // read one symbol at a time and keeps these big groups ON PURPOSE: compaction writes it
        // symbol-major, so the PAGE index prunes a one-symbol read inside one group
        // (`sort_for_merge` carries the measurement). Keeping `Grouped`'s 8,192-row groups here
        // was measured too — under half the rows decoded again, for more than twice the bytes.
        WriteProfile::Sealed => {
            builder = builder.set_max_row_group_row_count(Some(1_048_576));
        }
        // Grouped: SMALL groups, because a grouped part is read one symbol at a time and pruning
        // can only skip whole row groups (see `GROUPED_ROW_GROUP_ROWS`).
        WriteProfile::Grouped => {
            builder = builder.set_max_row_group_row_count(Some(GROUPED_ROW_GROUP_ROWS));
        }
        WriteProfile::Hot => {}
    }
    // Stamp the commit keys into the footer — see [`COMMIT_KEYS_META`]. This is what makes the part
    // self-describing and the manifest genuinely rebuildable rather than rebuildable-looking.
    if !commit_keys.is_empty() {
        builder = builder.set_key_value_metadata(Some(vec![KeyValue::new(
            COMMIT_KEYS_META.to_string(),
            commit_keys.join("\n"),
        )]));
    }
    let props = builder.build();
    let file = std::fs::File::create(path).map_err(io)?;
    let mut w = ArrowWriter::try_new(file, schema, Some(props)).map_err(q)?;
    for batch in &batches {
        w.write(batch).map_err(q)?;
    }
    // `into_inner` finalizes exactly as `close` does (footer written, buffers flushed) but hands the
    // `File` back so it can be fsynced. This fsync is UNCONDITIONAL and is the store's core
    // durability invariant: a manifest entry is never published before the part it names is durable.
    // Without it the footer sits in page cache while the manifest entry naming this part goes on to
    // be fsynced and published, so a power loss yields a durable manifest pointing at a truncated
    // part — and the WAL cannot repair that, because the commit key is already in the manifest
    // (on this very part's `FileEntry` since v3), which makes the retry a no-op. [`Durability`]
    // varies what happens ABOVE this line, never this line.
    let file = w.into_inner().map_err(q)?;
    file.sync_all().map_err(io)?;
    Ok(())
}

/// fsync a DIRECTORY, so a file created or renamed inside it survives a power loss.
///
/// On POSIX a `create` or `rename` is only durable once the CONTAINING DIRECTORY is fsynced —
/// fsyncing the file alone leaves its name recoverable-but-absent. Windows exposes no
/// directory-handle flush through `std::fs` (opening a directory as a `File` fails outright), so
/// this is a deliberate no-op there rather than a silent error; NTFS orders metadata through its own
/// journal, and the POSIX gap this closes does not transfer directly.
#[cfg(unix)]
pub(crate) fn fsync_dir(dir: &Path) -> Result<(), DataError> {
    std::fs::File::open(dir).map_err(io)?.sync_all().map_err(io)
}

#[cfg(not(unix))]
pub(crate) fn fsync_dir(_dir: &Path) -> Result<(), DataError> {
    Ok(())
}

/// `file://` URL to a single Parquet file — works on Windows (a bare `C:\..` path fails
/// DataFusion's URL parse) AND unix.
///
/// The path is PERCENT-ENCODED, and that is not cosmetic: a series symbol lands verbatim in the
/// `symbol=` partition directory, and this store already carries symbols containing `#` — the
/// Polymarket window convention `<slug>#<outcome_index>` (`vike_strategy::CheapNp`'s symbol
/// grammar). In a URL `#` opens the FRAGMENT, so an unencoded path silently truncates there and
/// DataFusion reports "No files found ... Cannot infer schema from an empty location" for a file
/// that is sitting right there on disk. `?` (query) and `%` (the escape itself) have the same
/// hazard, as does any byte outside the URL path grammar.
pub(crate) fn file_url(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let p = p.strip_prefix('/').unwrap_or(&p);
    let mut out = String::from("file:///");
    for b in p.bytes() {
        // RFC 3986 unreserved + the sub-delims/path characters DataFusion's object-store URL
        // parser accepts literally. Everything else — notably `#`, `?`, `%`, space and any
        // non-ASCII byte — is escaped.
        let safe = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b':'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'@'
            );
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parse a bar interval ("1m"/"5m"/"15m"/"1h"/"4h"/"1d"/"30s") → step in epoch-ms. Delegates to
/// the single interval vocabulary in [`vike_model::time::interval_ms`].
pub(crate) fn interval_ms(interval: &str) -> Result<i64, DataError> {
    vike_model::time::interval_ms(interval)
        .ok_or_else(|| DataError::Query(format!("bad interval {interval:?}")))
}
