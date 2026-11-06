//! The archive's `book_events` fixture builders, ONE copy for this crate's `archive_store` decode tests and
//! `vike-backfill`'s `vike_archive` ingest tests (each carried its own until 2026-10-31). Compiled for this
//! crate's unit tests and under `test-support`, which only a DEV edge turns on.

use std::sync::Arc;

use datafusion::arrow::array::{
    ArrayRef, BinaryArray, Decimal128Array, Int64Array, StringArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;

/// `(token_id, ts, local_ts, seq, event_type, side, price, size, bids, asks, tick_size, status)` — the
/// family layout's 12-decoded-field row shape (the other 4 real columns, `condition_id`/`is_snapshot`/
/// `best_bid`/`best_ask`, are never decoded).
pub type BookEventRow<'a> =
    (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

/// The physical type of `event_type`/`side`: the FLAT export's raw `Binary` (ClickHouse's enum export)
/// or the FAMILY export's `Utf8` — the live-verified divergence the decoder's `StrOrBinCol` reads both of.
#[derive(Clone, Copy)]
pub enum TagColumns {
    Binary,
    Utf8,
}

/// A `book_events` batch of `rows`, `event_type`/`side` as `tags`, every other column as both layouts have
/// it (prices at scale 4, sizes at scale 6).
pub fn book_events_batch<'r>(tags: TagColumns, rows: &[BookEventRow<'r>]) -> RecordBatch {
    let tag_type = match tags {
        TagColumns::Binary => DataType::Binary,
        TagColumns::Utf8 => DataType::Utf8,
    };
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("seq", DataType::UInt64, false),
        Field::new("event_type", tag_type.clone(), false),
        Field::new("side", tag_type, false),
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
    let tag = |pick: fn(&BookEventRow<'r>) -> &'r str| -> ArrayRef {
        match tags {
            TagColumns::Binary => Arc::new(BinaryArray::from(
                rows.iter().map(|r| pick(r).as_bytes()).collect::<Vec<_>>(),
            )),
            TagColumns::Utf8 => {
                Arc::new(StringArray::from(rows.iter().map(pick).collect::<Vec<_>>()))
            }
        }
    };
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            tag(|r| r.4),
            tag(|r| r.5),
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

/// [`book_events_batch`] written as one Parquet file, optionally split into row groups of at most
/// `max_rows_per_group` rows (`ArrowWriter` auto-splits a single `write()` once
/// `WriterProperties::set_max_row_group_row_count` is set), so a fixture can exercise pruning.
pub fn book_events_parquet(
    tags: TagColumns,
    rows: &[BookEventRow<'_>],
    max_rows_per_group: Option<usize>,
) -> Vec<u8> {
    let batch = book_events_batch(tags, rows);
    let props = max_rows_per_group
        .map(|n| WriterProperties::builder().set_max_row_group_row_count(Some(n)).build());
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, batch.schema(), props).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    buf
}
