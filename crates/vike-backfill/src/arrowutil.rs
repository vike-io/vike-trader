//! Shared Arrow-decode plumbing for the Parquet-backed collectors (the Arrow twin of
//! [`crate::csvutil`]). The pmxt archive ingest ([`crate::pmxt::ingest`]) and the ClickHouse
//! Polymarket ingest both decoded `FORMAT Parquet` payloads a row group at a time (bounded memory)
//! through datafusion's re-exported `arrow`/`parquet`, and both had rebuilt the same two pieces
//! byte-for-byte: (1) the nullable typed column accessors (downcast a batch column by name, else a
//! clean `CollectError::Fetch`) and (2) the row-group reader open/iterate skeleton with its
//! identical `Fetch` error wraps. Both landed here.
//!
//! ⚠ **The ClickHouse caller is gone; the archive collector took its place.** `clickhouse_poly`
//! and the read-side `backtest_bridge` were deleted on 2026-09-20 — data is fetched by API or from
//! the venue directly, never by reaching ClickHouse; `crates/vike-store/src/backtest_store.rs`'s
//! module doc carries what that cost. The one consumer now is `pmxt`. ⚠ `crate::vike_archive` was
//! the second until 2026-10-11, through its own `decode` child and three accessors only it read
//! (`i64_col`, `u64_col`, `f64_col`); that child is deleted — the archive's streams decode through
//! `vike_store::archive_store`'s decoders, the one copy the read-in-place store uses too — and
//! the three accessors went with it rather than linger as `dead_code`. It is deliberately NOT
//! inlined back into a caller: the accessor shape and its error wording are the thing a new
//! collector copies.
//!
//! The error-message vendor prefix is a `ctx: &str` parameter (e.g. `"pmxt"`), mirroring how
//! [`crate::http::send`] takes its `ctx` — the accessor message is
//! `"{ctx} parquet: missing/!<type> column {name}"`, byte-identical to what each caller emitted
//! before. The per-column decimal scales and null policies stay in the callers (this module only
//! owns the type-checked column fetch + the reader skeleton); pmxt keeps its stateful `MapState`
//! loop, driving it over [`for_each_batch`] instead of its own reader.

use std::fs::File;
use std::path::Path;

use datafusion::arrow::array::{Array, Decimal128Array, StringArray, TimestampMillisecondArray};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::CollectError;

// ---- Nullable typed column accessors ----------------------------------------------------------
// Downcast a batch column by name to the expected Arrow array type, or a descriptive
// `CollectError::Fetch` naming the offending column (and the vendor via `ctx`). A wrong/missing
// column is a clean error, never a panic.

/// A `Utf8` column named `name`.
pub(crate) fn str_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a StringArray, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| CollectError::Fetch(format!("{ctx} parquet: missing/!string column {name}")))
}

/// A `Timestamp(Millisecond)` column named `name`.
pub(crate) fn ts_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a TimestampMillisecondArray, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<TimestampMillisecondArray>())
        .ok_or_else(|| {
            CollectError::Fetch(format!("{ctx} parquet: missing/!timestamp-ms column {name}"))
        })
}

/// A `Decimal128` column named `name` (the caller applies the per-column scale divisor).
pub(crate) fn decimal_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a Decimal128Array, CollectError> {
    b.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Decimal128Array>()).ok_or_else(
        || CollectError::Fetch(format!("{ctx} parquet: missing/!decimal128 column {name}")),
    )
}

// ---- Row-group reader skeleton ----------------------------------------------------------------

/// Open the Parquet file at `path` and drive `f` over each `RecordBatch` one row group at a time
/// (bounded memory — never the whole file). The open/parquet-open/reader-build/per-batch failures
/// each wrap into `CollectError::Fetch` with the path — the identical wraps both callers used. The
/// caller owns what happens per batch: pmxt runs its stateful `MapState` loop; the deleted
/// ClickHouse ingest collected per-token vectors.
pub(crate) fn for_each_batch<F>(path: &Path, mut f: F) -> Result<(), CollectError>
where
    F: FnMut(&RecordBatch) -> Result<(), CollectError>,
{
    let file = File::open(path)
        .map_err(|e| CollectError::Fetch(format!("open {}: {e}", path.display())))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| CollectError::Fetch(format!("parquet open {}: {e}", path.display())))?
        .build()
        .map_err(|e| CollectError::Fetch(format!("parquet reader {}: {e}", path.display())))?;
    for batch in reader {
        let batch = batch
            .map_err(|e| CollectError::Fetch(format!("parquet batch {}: {e}", path.display())))?;
        f(&batch)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    /// The `ctx` prefix is threaded verbatim into the accessor error (mirrors `http::send`), so two
    /// vendors keep distinct byte-identical messages off the ONE shared accessor.
    ///
    /// ⚠ The second `ctx` is a MADE-UP vendor name, and deliberately so now that only `pmxt` calls
    /// in: the property under test is that the parameter reaches the message, which a single real
    /// caller cannot demonstrate. It read `"clickhouse"` until that caller was deleted on
    /// 2026-09-20.
    #[test]
    fn ctx_prefix_names_the_vendor_in_accessor_errors() {
        let schema = Schema::new(vec![Field::new("price", DataType::Int64, false)]);
        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(Int64Array::from(vec![1_i64]))])
                .unwrap();
        // wrong type (Int64 where Utf8 asked) → clean Err carrying ctx + column, not a panic.
        // (`CollectError::Fetch`'s Display prepends `venue fetch: `; the ctx-parameterized body is
        // what this dedup preserves byte-identically per vendor.)
        let err = str_col(&batch, "price", "pmxt").unwrap_err();
        assert_eq!(format!("{err}"), "venue fetch: pmxt parquet: missing/!string column price");
        let err = str_col(&batch, "price", "some-other-vendor").unwrap_err();
        assert_eq!(
            format!("{err}"),
            "venue fetch: some-other-vendor parquet: missing/!string column price"
        );
        // a missing column is the same clean error, with the type the accessor asked for.
        let err = ts_col(&batch, "nope", "pmxt").unwrap_err();
        assert_eq!(
            format!("{err}"),
            "venue fetch: pmxt parquet: missing/!timestamp-ms column nope"
        );
        let err = decimal_col(&batch, "nope", "pmxt").unwrap_err();
        assert_eq!(format!("{err}"), "venue fetch: pmxt parquet: missing/!decimal128 column nope");
    }
}
