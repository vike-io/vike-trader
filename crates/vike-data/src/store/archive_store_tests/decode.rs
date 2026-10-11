//! Pure decode of synthetic family-layout batches, against the documented import conventions.

use super::*;

// ---- pure decode (structural equivalence with the documented import conventions) -----------
//
// Same expected values as `vike_archive.rs`'s own `book_events_batch`/`trades_batch` tests
// (0.5/100.0 bid, 0.42/7.0 buy-delta, gap_start/stale/live_resume mapping, "sell" taker ->
// is_buyer_maker=true) — this is the "decoded values match what the ingest path produces for
// the same input" property: both decoders apply the SAME scale divisors and the SAME
// event_type/status/side conventions the module doc documents, so agreement here is not a
// coincidence.

fn one_row_batch(row: Row<'_>) -> RecordBatch {
    let path_bytes = write_family_parquet(&[row], None);
    let mut reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(path_bytes))
        .unwrap()
        .build()
        .unwrap();
    reader.next().unwrap().unwrap()
}

#[test]
fn snapshot_row_decodes_full_depth() {
    let b = one_row_batch((
        "TOKA",
        1_700_000_000_000,
        1_700_000_000_003,
        1,
        "book",
        "none",
        0.0,
        0.0,
        "[[0.5,100.0]]",
        "[[0.51,80.0]]",
        0.01,
        "",
    ));
    let out = book_updates_from_batch(&b, "TOKA", TsRange::all()).unwrap();
    assert_eq!(out.len(), 1);
    let u = &out[0];
    assert_eq!(u.kind, BookUpdateKind::Snapshot);
    assert_eq!(u.ts, 1_700_000_000_000);
    assert_eq!(u.local_ts, 1_700_000_000_003);
    assert_eq!(u.seq, 1);
    assert_eq!(u.bids, vec![BookLevel::new(0.5, 100.0)]);
    assert_eq!(u.asks, vec![BookLevel::new(0.51, 80.0)]);
    assert!((u.tick_size - 0.01).abs() < 1e-12);
    assert_eq!(u.symbol, "TOKA");
}

#[test]
fn delta_row_decodes_the_populated_side_from_decimal_columns() {
    let rows: [Row<'_>; 2] = [
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        out.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
    }
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].kind, BookUpdateKind::Delta);
    assert!((out[0].bids[0].price - 0.42).abs() < 1e-9);
    assert!((out[0].bids[0].qty - 7.0).abs() < 1e-9);
    assert!(out[0].asks.is_empty());
    assert!(out[1].bids.is_empty());
    assert!((out[1].asks[0].price - 0.60).abs() < 1e-9);
}

#[test]
fn status_rows_map_to_the_right_kind_with_empty_levels() {
    let rows: [Row<'_>; 3] = [
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
        ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
        ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
    ];
    let bytes = write_family_parquet(&rows, None);
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        out.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
    }
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].kind, BookUpdateKind::GapStart);
    assert_eq!(out[1].kind, BookUpdateKind::Stale);
    assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
    assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
}

#[test]
fn trades_from_batch_inverts_the_taker_side_convention() {
    let rows: [Row<'_>; 2] = [
        (
            "TOKA",
            1_700_000_000_100,
            1_700_000_000_101,
            0,
            "trade",
            "sell",
            0.95,
            3.0,
            "",
            "",
            0.0,
            "",
        ),
        (
            "TOKA",
            1_700_000_000_200,
            1_700_000_000_201,
            0,
            "trade",
            "buy",
            0.10,
            1.0,
            "",
            "",
            0.0,
            "",
        ),
    ];
    let bytes = write_family_parquet(&rows, None);
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        out.extend(trades_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
    }
    assert_eq!(out.len(), 2);
    assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
    assert!((out[0].price - 0.95).abs() < 1e-9);
    assert!((out[0].size - 3.0).abs() < 1e-9);
    assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
}

#[test]
fn trade_rows_are_excluded_from_book_updates_and_vice_versa() {
    let rows: [Row<'_>; 2] = [
        ("TOKA", 1, 1, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
        ("TOKA", 2, 2, 1, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let reader = || {
        ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.clone()))
            .unwrap()
            .build()
            .unwrap()
    };
    let mut books = Vec::new();
    for batch in reader() {
        books.extend(book_updates_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
    }
    assert_eq!(books.len(), 1, "only the book row");
    assert_eq!(books[0].kind, BookUpdateKind::Snapshot);

    let mut trades = Vec::new();
    for batch in reader() {
        trades.extend(trades_from_batch(&batch.unwrap(), "TOKA", TsRange::all()).unwrap());
    }
    assert_eq!(trades.len(), 1, "only the trade row");
}
