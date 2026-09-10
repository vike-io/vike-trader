//! Shared Arrow-decode plumbing for the Parquet-backed collectors (the Arrow twin of
//! [`crate::csvutil`]). Both the pmxt archive ingest ([`crate::pmxt::ingest`]) and the ClickHouse
//! Polymarket ingest ([`crate::clickhouse_poly`]) decode `FORMAT Parquet` payloads a row group at
//! a time (bounded memory) through datafusion's re-exported `arrow`/`parquet`, and both had
//! rebuilt the same two pieces byte-for-byte: (1) the nullable typed column accessors
//! (`str_col`/`i64_col`/`f64_col`/`ts_col`/`decimal_col` — downcast a batch column by name, else a
//! clean `CollectError::Fetch`) and (2) the row-group reader open/iterate skeleton with its
//! identical `Fetch` error wraps. Both land here.
//!
//! The error-message vendor prefix is a `ctx: &str` parameter (e.g. `"pmxt"` / `"clickhouse"`),
//! mirroring how [`crate::http::send`] takes its `ctx` — the accessor message is
//! `"{ctx} parquet: missing/!<type> column {name}"`, byte-identical to what each caller emitted
//! before. The per-column decimal scales and null policies stay in the callers (this module only
//! owns the type-checked column fetch + the reader skeleton); pmxt keeps its stateful `MapState`
//! loop, driving it over [`for_each_batch`] instead of its own reader.

use std::fs::File;
use std::path::Path;

use datafusion::arrow::array::{
    Array, Decimal128Array, Float64Array, Int64Array, StringArray, TimestampMillisecondArray,
};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::error::CollectError;

// ---- Nullable typed column accessors ----------------------------------------------------------
// Downcast a batch column by name to the expected Arrow array type, or a descriptive
// `CollectError::Fetch` naming the offending column (and the vendor via `ctx`). A wrong/missing
// column is a clean error, never a panic.

/// A `Utf8` column named `name`.
pub fn str_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a StringArray, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| CollectError::Fetch(format!("{ctx} parquet: missing/!string column {name}")))
}

/// An `Int64` column named `name`.
pub fn i64_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a Int64Array, CollectError> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .ok_or_else(|| CollectError::Fetch(format!("{ctx} parquet: missing/!int64 column {name}")))
}

/// A `Float64` column named `name`.
pub fn f64_col<'a>(
    b: &'a RecordBatch,
    name: &str,
    ctx: &str,
) -> Result<&'a Float64Array, CollectError> {
    b.column_by_name(name).and_then(|c| c.as_any().downcast_ref::<Float64Array>()).ok_or_else(
        || CollectError::Fetch(format!("{ctx} parquet: missing/!float64 column {name}")),
    )
}

/// A `Timestamp(Millisecond)` column named `name`.
pub fn ts_col<'a>(
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
pub fn decimal_col<'a>(
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
/// caller owns what happens per batch: pmxt runs its stateful `MapState` loop, clickhouse collects
/// per-token vectors.
pub fn for_each_batch<F>(path: &Path, mut f: F) -> Result<(), CollectError>
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
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    /// The `ctx` prefix is threaded verbatim into the accessor error (mirrors `http::send`), so pmxt
    /// and clickhouse keep their distinct byte-identical messages off the ONE shared accessor.
    #[test]
    fn ctx_prefix_names_the_vendor_in_accessor_errors() {
        let schema = Schema::new(vec![Field::new("price", DataType::Int64, false)]);
        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(Int64Array::from(vec![1_i64]))])
                .unwrap();
        // wrong type (Int64 where Float64 asked) → clean Err carrying ctx + column, not a panic.
        // (`CollectError::Fetch`'s Display prepends `venue fetch: `; the ctx-parameterized body is
        // what this dedup preserves byte-identically per vendor.)
        let err = f64_col(&batch, "price", "pmxt").unwrap_err();
        assert_eq!(format!("{err}"), "venue fetch: pmxt parquet: missing/!float64 column price");
        let err = f64_col(&batch, "price", "clickhouse").unwrap_err();
        assert_eq!(
            format!("{err}"),
            "venue fetch: clickhouse parquet: missing/!float64 column price"
        );
        // a present, correctly-typed column downcasts.
        assert!(i64_col(&batch, "price", "pmxt").is_ok());
        // a missing column is the same clean error.
        let err = str_col(&batch, "nope", "clickhouse").unwrap_err();
        assert_eq!(
            format!("{err}"),
            "venue fetch: clickhouse parquet: missing/!string column nope"
        );
    }
}
