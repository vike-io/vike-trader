//! Fixtures two or more vike_archive test files share: the `book_events` row shape, its builders, a scratch store.

use std::sync::Arc;

use bytes::Bytes;
use datafusion::arrow::array::{
    ArrayRef, BinaryArray, Decimal128Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use vike_data::DataFusionHist;

pub(super) type BookRow<'a> =
    (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

pub(super) fn book_events_batch(rows: &[BookRow<'_>]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("seq", DataType::UInt64, false),
        Field::new("event_type", DataType::Binary, false),
        Field::new("side", DataType::Binary, false),
        Field::new("price", DataType::Decimal128(9, 4), false),
        Field::new("size", DataType::Decimal128(18, 6), false),
        Field::new("bids", DataType::Utf8, false),
        Field::new("asks", DataType::Utf8, false),
        Field::new("tick_size", DataType::Decimal128(9, 4), false),
        Field::new("status", DataType::Utf8, false),
    ]);
    let price = Decimal128Array::from(
        rows.iter().map(|r| (r.6 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    let size = Decimal128Array::from(
        rows.iter().map(|r| (r.7 * 1_000_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(18, 6)
    .unwrap();
    let tick_size = Decimal128Array::from(
        rows.iter().map(|r| (r.10 * 10_000.0).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(BinaryArray::from(rows.iter().map(|r| r.4.as_bytes()).collect::<Vec<_>>())),
            Arc::new(BinaryArray::from(rows.iter().map(|r| r.5.as_bytes()).collect::<Vec<_>>())),
            Arc::new(price) as ArrayRef,
            Arc::new(size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
            Arc::new(tick_size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

/// Writes `rows` as a `book_events`-shaped Parquet file (the same 12-column schema
/// [`book_events_batch`] builds), forced to split into row groups of at most
/// `max_rows_per_group` rows each (`ArrowWriter` auto-splits a single `write()` call across
/// row-group boundaries once `WriterProperties::set_max_row_group_row_count` is set — the same
/// mechanism [`write_single_column_parquet`] already relies on above).
pub(super) fn write_book_events_parquet(rows: &[BookRow<'_>], max_rows_per_group: usize) -> Bytes {
    let batch = book_events_batch(rows);
    let schema = batch.schema();
    let props =
        WriterProperties::builder().set_max_row_group_row_count(Some(max_rows_per_group)).build();
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    Bytes::from(buf)
}

pub(super) fn open() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    (dir, store)
}
