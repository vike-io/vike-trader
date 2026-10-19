//! The archive's three stream decoders over SYNTHETIC batches, as the INGEST reads them: the FLAT
//! layout's `Binary` `event_type`/`side`, every row kept (`RowScope::All`) and attributed to its
//! own `token_id`, plus the `trades` stream's `Float64` columns. These cases came with the
//! decoders from `vike_backfill`'s `vike_archive`, whose near-copy of each this module replaced.

use super::*;
use datafusion::arrow::array::{BinaryArray, Float64Array};

/// The FLAT layout's `book_events` batch: `event_type`/`side` as raw `Binary` (ClickHouse's enum
/// export), every other column as the family layout has it.
fn flat_book_events_batch(rows: &[Row<'_>]) -> RecordBatch {
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

// ---- level JSON decode ----------------------------------------------------------------------

#[test]
fn parse_levels_json_handles_numbers_empty_and_garbage() {
    assert_eq!(parse_levels_json(""), Vec::<BookLevel>::new());
    assert_eq!(
        parse_levels_json("[[0.5,100.0],[0.49,50.0]]"),
        vec![BookLevel::new(0.5, 100.0), BookLevel::new(0.49, 50.0)]
    );
    assert_eq!(parse_levels_json("not json"), Vec::<BookLevel>::new());
}

// ---- decimal scales ---------------------------------------------------------------------------

#[test]
fn the_decimal_scales_are_the_archive_schemas() {
    assert_eq!(PRICE_SCALE_DIVISOR, 10_000.0, "price/bid/ask/tick_size are scale-4");
    assert_eq!(SIZE_SCALE_DIVISOR, 1_000_000.0, "sizes are scale-6");
}

// ---- book_events -> BookUpdate (synthetic RecordBatch, no Parquet round trip) --------------

/// The FAMILY-layout twin of [`flat_book_events_batch`]: identical row shape, but `event_type`/
/// `side` are `Utf8` instead of `Binary` — the real, live-verified physical-type divergence
/// between the flat and family `book_events` exports (see the decoder's `StrOrBinCol`). Proves
/// [`book_updates_from_batch`] decodes the family layout's encoding too, not just the flat one.
fn book_events_batch_family_utf8(rows: &[Row<'_>]) -> RecordBatch {
    let schema = Schema::new(vec![
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
    .unwrap()
}

#[test]
fn family_layouts_utf8_event_type_and_side_decode_identically_to_flats_binary_encoding() {
    let rows: Vec<Row<'_>> = vec![
        (
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
        ),
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
    ];
    let flat = book_updates_from_batch(&flat_book_events_batch(&rows), RowScope::All).unwrap();
    let family =
        book_updates_from_batch(&book_events_batch_family_utf8(&rows), RowScope::All).unwrap();
    // `BookUpdate` derives no `PartialEq` (vike-model), so compare via `Debug` — still a
    // precise structural comparison, just not the `==` operator.
    assert_eq!(
        format!("{flat:?}"),
        format!("{family:?}"),
        "the same logical rows must decode identically regardless of layout"
    );
    assert_eq!(flat.len(), 4);
}

#[test]
fn snapshot_row_decodes_full_depth_and_per_row_symbol() {
    let b = flat_book_events_batch(&[(
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
    )]);
    let out = book_updates_from_batch(&b, RowScope::All).unwrap();
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
    let b = flat_book_events_batch(&[
        ("TOKA", 1, 2, 5, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOKA", 1, 2, 6, "price_change", "sell", 0.60, 0.0, "", "", 0.01, ""),
    ]);
    let out = book_updates_from_batch(&b, RowScope::All).unwrap();
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
    let b = flat_book_events_batch(&[
        ("TOKA", 9, 10, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "gap_start"),
        ("TOKA", 9, 11, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "stale"),
        ("TOKA", 9, 12, 0, "status", "none", 0.0, 0.0, "", "", 0.0, "live_resume"),
    ]);
    let out = book_updates_from_batch(&b, RowScope::All).unwrap();
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].kind, BookUpdateKind::GapStart);
    assert_eq!(out[1].kind, BookUpdateKind::Stale);
    assert_eq!(out[2].kind, BookUpdateKind::LiveResume);
    assert!(out.iter().all(|u| u.bids.is_empty() && u.asks.is_empty()));
}

#[test]
fn trade_and_tick_size_change_rows_are_skipped() {
    let b = flat_book_events_batch(&[
        ("TOKA", 1, 2, 0, "trade", "sell", 0.5, 1.0, "", "", 0.0, ""),
        ("TOKA", 1, 2, 0, "tick_size_change", "none", 0.0, 0.0, "", "", 0.02, ""),
    ]);
    let out = book_updates_from_batch(&b, RowScope::All).unwrap();
    assert!(out.is_empty(), "trade/tick_size_change carry no BookUpdate: {out:?}");
}

#[test]
fn mixed_tokens_in_one_batch_keep_their_own_symbol() {
    let b = flat_book_events_batch(&[
        ("TOKA", 1, 1, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
        ("TOKB", 2, 2, 0, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, ""),
    ]);
    let out = book_updates_from_batch(&b, RowScope::All).unwrap();
    assert_eq!(out[0].symbol, "TOKA");
    assert_eq!(out[1].symbol, "TOKB");
}

// ---- trades -> TradeTick ----------------------------------------------------------------------

fn trades_batch(rows: &[(&str, i64, i64, f64, f64, &str)]) -> RecordBatch {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("price", DataType::Float64, false),
        Field::new("size", DataType::Float64, false),
        Field::new("side", DataType::Utf8, false),
    ]);
    RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<_>>())),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.3).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.5).collect::<Vec<_>>())),
        ],
    )
    .unwrap()
}

#[test]
fn trades_from_batch_inverts_the_taker_side_convention() {
    let b = trades_batch(&[
        ("TOKA", 1_700_000_000_100, 1_700_000_000_101, 0.95, 3.0, "sell"),
        ("TOKA", 1_700_000_000_200, 1_700_000_000_201, 0.10, 1.0, "buy"),
    ]);
    let out = trades_from_batch(&b, RowScope::All).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out[0].is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
    assert_eq!(out[0].price, 0.95);
    assert_eq!(out[0].size, 3.0);
    assert_eq!(out[0].symbol, "TOKA");
    assert!(!out[1].is_buyer_maker, "side=buy -> taker bought -> is_buyer_maker=false");
}

// ---- l1_quotes -> QuoteTick -------------------------------------------------------------------

#[test]
fn quotes_from_l1_batch_decodes_decimal_l1() {
    let schema = Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("bid", DataType::Decimal128(9, 4), false),
        Field::new("ask", DataType::Decimal128(9, 4), false),
        Field::new("bid_size", DataType::Decimal128(18, 6), false),
        Field::new("ask_size", DataType::Decimal128(18, 6), false),
    ]);
    let bid = Decimal128Array::from(vec![4_400_i128]).with_precision_and_scale(9, 4).unwrap();
    let ask = Decimal128Array::from(vec![4_700_i128]).with_precision_and_scale(9, 4).unwrap();
    let bid_size =
        Decimal128Array::from(vec![10_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
    let ask_size =
        Decimal128Array::from(vec![8_000_000_i128]).with_precision_and_scale(18, 6).unwrap();
    let b = RecordBatch::try_new(
        Arc::new(schema),
        vec![
            Arc::new(StringArray::from(vec!["TOKA"])),
            Arc::new(Int64Array::from(vec![1_700_000_000_200_i64])),
            Arc::new(Int64Array::from(vec![1_700_000_000_201_i64])),
            Arc::new(bid) as ArrayRef,
            Arc::new(ask) as ArrayRef,
            Arc::new(bid_size) as ArrayRef,
            Arc::new(ask_size) as ArrayRef,
        ],
    )
    .unwrap();
    let out = quotes_from_l1_batch(&b, RowScope::All).unwrap();
    assert_eq!(out.len(), 1);
    assert!((out[0].bid - 0.44).abs() < 1e-9);
    assert!((out[0].ask - 0.47).abs() < 1e-9);
    assert!((out[0].bid_size - 10.0).abs() < 1e-9);
    assert!((out[0].ask_size - 8.0).abs() < 1e-9);
    assert_eq!(out[0].symbol, "TOKA");
}

/// The two readers differ ONLY in which rows they keep: the store's `RowScope::Symbol` keeps one
/// token inside an inclusive `ts` window, and the rows it keeps decode exactly as `All` decodes them.
#[test]
fn a_symbol_scope_keeps_one_token_inside_the_window_and_decodes_it_unchanged() {
    let b = trades_batch(&[
        ("TOKA", 100, 101, 0.95, 3.0, "sell"),
        ("TOKB", 150, 151, 0.50, 1.0, "buy"),
        ("TOKA", 200, 201, 0.10, 1.0, "buy"),
        ("TOKA", 300, 301, 0.20, 2.0, "sell"),
    ]);
    let all = trades_from_batch(&b, RowScope::All).unwrap();
    assert_eq!(all.len(), 4);
    let scoped = trades_from_batch(
        &b,
        RowScope::Symbol { symbol: "TOKA", range: TsRange { start: Some(100), end: Some(200) } },
    )
    .unwrap();
    assert_eq!(scoped.len(), 2, "TOKB and the row past the window are dropped: {scoped:?}");
    assert_eq!(format!("{:?}", scoped[0]), format!("{:?}", all[0]));
    assert_eq!(format!("{:?}", scoped[1]), format!("{:?}", all[2]));
}
