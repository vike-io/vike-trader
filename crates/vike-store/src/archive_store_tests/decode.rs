//! Pure decode of synthetic family-layout batches, against the documented import conventions.

use super::*;

// ---- pure decode (structural equivalence with the documented import conventions) -----------
//
// Same expected values as `stream_decode.rs`'s FLAT-layout cases beside this file (0.5/100.0
// bid, 0.42/7.0 buy-delta, gap_start/stale/live_resume mapping): there is ONE decoder, so these pin
// the family layout and that file pins the flat one.

/// Row 9's table: every label either source can carry, against the match both used to spell.
#[test]
fn every_row_label_reads_as_one_book_kind_and_one_side() {
    use vike_marketdata::L2BookUpdateKind::{Delta, GapStart, LiveResume, Snapshot, Stale};
    for (event_type, status, want) in [
        ("book", "", Some(Snapshot)),
        ("book", "stale", Some(Snapshot)),
        ("price_change", "", Some(Delta)),
        ("status", "gap_start", Some(GapStart)),
        ("status", "stale", Some(Stale)),
        ("status", "live_resume", Some(LiveResume)),
        ("status", "", None),
        ("status", "GAP_START", None),
        ("status", "unheard_of", None),
        ("trade", "stale", None),
        ("tick_size_change", "", None),
        ("", "", None),
    ] {
        assert_eq!(book_kind_of(event_type, status), want, "{event_type:?} {status:?}");
    }
    for (side, maker) in [("sell", true), ("buy", false), ("none", false), ("", false)] {
        assert_eq!(is_buyer_maker_of(side), maker, "{side:?}");
    }
    assert!(!is_buyer_maker_of("SELL"), "the label is exact, as both copies compared it");
}

fn one_row_batch(row: BookEventRow<'_>) -> RecordBatch {
    let path_bytes = book_events_parquet(TagColumns::Utf8, &[row], None);
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
    let out = book_updates_from_batch(
        &b,
        RowScope::Symbol { symbol: "TOKA", range: TimestampRange::all() },
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    let u = &out[0];
    assert_eq!(u.kind, L2BookUpdateKind::Snapshot);
    assert_eq!(u.timestamp, 1_700_000_000_000);
    assert_eq!(u.local_timestamp, 1_700_000_000_003);
    assert_eq!(u.sequence, 1);
    assert_eq!(u.bids, vec![L2BookLevel::new(0.5, 100.0)]);
    assert_eq!(u.asks, vec![L2BookLevel::new(0.51, 80.0)]);
    assert!((u.tick_size - 0.01).abs() < 1e-12);
    assert_eq!(u.symbol, "TOKA");
}

#[test]
fn delta_row_decodes_the_populated_side_from_decimal_columns() {
    let rows: [BookEventRow<'_>; 2] = [
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
    ];
    let bytes = book_events_parquet(TagColumns::Utf8, &rows, None);
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        out.extend(
            book_updates_from_batch(
                &batch.unwrap(),
                RowScope::Symbol { symbol: "TOKA", range: TimestampRange::all() },
            )
            .unwrap(),
        );
    }
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].kind, L2BookUpdateKind::Delta);
    assert!((out[0].bids[0].price - 0.42).abs() < 1e-9);
    assert!((out[0].bids[0].qty - 7.0).abs() < 1e-9);
    assert!(out[0].asks.is_empty());
    assert!(out[1].bids.is_empty());
    assert!((out[1].asks[0].price - 0.60).abs() < 1e-9);
}

#[test]
fn status_rows_map_to_the_right_kind_with_empty_levels() {
    let rows: [BookEventRow<'_>; 3] = [
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
        ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
        ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
    ];
    let bytes = book_events_parquet(TagColumns::Utf8, &rows, None);
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut out = Vec::new();
    for batch in reader {
        out.extend(
            book_updates_from_batch(
                &batch.unwrap(),
                RowScope::Symbol { symbol: "TOKA", range: TimestampRange::all() },
            )
            .unwrap(),
        );
    }
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].kind, L2BookUpdateKind::GapStart);
    assert_eq!(out[1].kind, L2BookUpdateKind::Stale);
    assert_eq!(out[2].kind, L2BookUpdateKind::LiveResume);
    assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
}

#[test]
fn trade_rows_are_excluded_from_book_updates() {
    let rows: [BookEventRow<'_>; 2] = [
        ("TOKA", 1, 1, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
        ("TOKA", 2, 2, 1, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
    ];
    let bytes = book_events_parquet(TagColumns::Utf8, &rows, None);
    let reader = || {
        ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.clone()))
            .unwrap()
            .build()
            .unwrap()
    };
    let mut books = Vec::new();
    for batch in reader() {
        books.extend(
            book_updates_from_batch(
                &batch.unwrap(),
                RowScope::Symbol { symbol: "TOKA", range: TimestampRange::all() },
            )
            .unwrap(),
        );
    }
    assert_eq!(books.len(), 1, "only the book row");
    assert_eq!(books[0].kind, L2BookUpdateKind::Snapshot);
}
