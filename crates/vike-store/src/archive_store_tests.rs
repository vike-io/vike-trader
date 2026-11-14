use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, Decimal128Array, Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use vike_data::TimestampRange;
use vike_marketdata::{L2BookLevel, L2BookUpdateKind};

use super::*;
// The `book_events` row shape and its batch/Parquet builders, shared with vike-backfill's ingest tests.
use crate::test_support::{BookEventRow, TagColumns, book_events_batch, book_events_parquet};

#[path = "archive_store_tests/decode.rs"]
#[cfg(test)]
mod decode;

#[path = "archive_store_tests/stream_decode.rs"]
#[cfg(test)]
mod stream_decode;
