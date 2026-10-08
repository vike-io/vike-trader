use super::*;
use datafusion::arrow::array::{ArrayRef, Decimal128Array, Int64Array, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use std::sync::Arc;

/// `(token_id, ts, local_ts, seq, event_type, side, price, size, bids, asks, tick_size,
/// status)` — the family layout's 12-decoded-field row shape (the other 4 real columns,
/// `condition_id`/`is_snapshot`/`best_bid`/`best_ask`, are never decoded by this module, same
/// as `vike_archive.rs`).
type Row<'a> = (&'a str, i64, i64, u64, &'a str, &'a str, f64, f64, &'a str, &'a str, f64, &'a str);

/// Writes `rows` as a family-layout Parquet file (`event_type`/`side` as plain `Utf8` — the
/// real physical type this module targets, per its doc), optionally forcing a max row-group
/// row count so a fixture can exercise pruning across many small row groups.
fn write_family_parquet(rows: &[Row<'_>], max_rows_per_group: Option<usize>) -> Vec<u8> {
    let schema = Arc::new(Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("seq", DataType::UInt64, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("side", DataType::Utf8, false),
        Field::new("price", DataType::Decimal128(9, 4), false),
        Field::new("size", DataType::Decimal128(18, 6), false),
        Field::new("bids", DataType::Utf8, false),
        Field::new("asks", DataType::Utf8, false),
        Field::new("tick_size", DataType::Decimal128(9, 4), false),
        Field::new("status", DataType::Utf8, false),
    ]));
    let price = Decimal128Array::from(
        rows.iter().map(|r| (r.6 * PRICE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    let size = Decimal128Array::from(
        rows.iter().map(|r| (r.7 * SIZE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(18, 6)
    .unwrap();
    let tick_size = Decimal128Array::from(
        rows.iter().map(|r| (r.10 * PRICE_SCALE_DIVISOR).round() as i128).collect::<Vec<_>>(),
    )
    .with_precision_and_scale(9, 4)
    .unwrap();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())) as ArrayRef,
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(UInt64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
            Arc::new(price) as ArrayRef,
            Arc::new(size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.8).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.9).collect::<Vec<_>>())),
            Arc::new(tick_size) as ArrayRef,
            Arc::new(StringArray::from(rows.iter().map(|r| r.11).collect::<Vec<_>>())),
        ],
    )
    .unwrap();
    let props = max_rows_per_group
        .map(|n| WriterProperties::builder().set_max_row_group_row_count(Some(n)).build());
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, props).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    buf
}

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
