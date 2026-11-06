use super::*;
use datafusion::arrow::array::{ArrayRef, Decimal128Array, Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use std::sync::Arc;
// The `book_events` row shape and its batch/Parquet builders, shared with vike-backfill's ingest tests.
use crate::test_support::{BookEventRow, TagColumns, book_events_batch, book_events_parquet};

fn write_temp_parquet(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Price comparison for the derived-L1 fold. Prices round-trip through `L2Book`'s tick INDEX
/// (`price_of(tick) = tick * tick_size`), so `51 * 0.01` need not be bit-identical to the
/// literal `0.51` — an exact `assert_eq!` here would be testing f64 representation, not the
/// fold. Half a tick is the meaningful tolerance: anything larger is a real mis-price.
fn near(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() < 0.005, "{what}: got {got}, want {want}");
}

#[path = "archive_store_tests/decode.rs"]
#[cfg(test)]
mod decode;

#[path = "archive_store_tests/scan_and_prune.rs"]
#[cfg(test)]
mod scan_and_prune;

#[path = "archive_store_tests/construction.rs"]
#[cfg(test)]
mod construction;

#[path = "archive_store_tests/derived_l1.rs"]
#[cfg(test)]
mod derived_l1;

#[path = "archive_store_tests/reports.rs"]
#[cfg(test)]
mod reports;

#[path = "archive_store_tests/stream_decode.rs"]
#[cfg(test)]
mod stream_decode;
