use super::*;
use datafusion::arrow::array::ArrayRef;
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

// ---- end-to-end: ArchiveParquetHistStore over a real multi-row-group Parquet file -----------

#[test]
fn scan_book_updates_returns_only_the_requested_token_and_range() {
    let rows: Vec<Row<'_>> = vec![
        ("TOK_A", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 200, 200, 2, "price_change", "buy", 0.6, 5.0, "", "", 0.01, ""),
        ("TOK_A", 999_999, 999_999, 3, "price_change", "buy", 0.7, 5.0, "", "", 0.01, ""),
        ("TOK_B", 150, 150, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    // One row group per row -> forces the store to prune across several groups, not just
    // decode a single one.
    let bytes = write_family_parquet(&rows, Some(1));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::of(0, 500)).unwrap();
    assert_eq!(out.len(), 2, "TOK_A rows within [0,500] only — not TOK_B, not the ts=999999 row");
    assert_eq!(out[0].ts, 100);
    assert_eq!(out[1].ts, 200);
    assert_eq!(out[1].bids, vec![BookLevel::new(0.6, 5.0)]);
}

/// The window-clock divergence, pinned as BEHAVIOUR rather than as prose (module doc;
/// [`crate::window_clock::WindowClock`] carries the measurement).
///
/// Two rows shaped like the real tape's two extremes: a `price_change` whose two clocks are
/// 18 ms apart, and a `book` SNAPSHOT ANCHOR whose venue stamp is hours older than its arrival
/// (measured max on one day: 41.9 h). For a five-minute window ending after the anchor arrives,
/// this store returns the delta and NOT the anchor — a `local_ts`-scoped window would return
/// both. That is the 1-market-window-in-8 case, reproduced in two rows. (The store that scoped
/// that way was the ClickHouse one, deleted 2026-09-20; the CLOCK it used is still on the tape.)
///
/// It fails the moment this store is "aligned" onto `local_ts`: the anchor would come back and
/// the second assertion would break. Which is the point — aligning it is a change to what every
/// existing study sees, not a tidy-up.
#[test]
fn a_stale_snapshot_anchor_is_outside_this_stores_window_and_inside_the_siblings() {
    let window = TsRange::of(1_000_000, 1_300_000); // a five-minute slice, in ms
    let arrived_in_window = 1_200_000i64;
    let anchor_venue_ts = arrived_in_window - 41 * 3_600_000; // 41 h older than its arrival
    let rows: Vec<Row<'_>> = vec![
        (
            "TOK_A",
            anchor_venue_ts,
            arrived_in_window,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,10.0]]",
            "[[0.51,10.0]]",
            0.01,
            "",
        ),
        ("TOK_A", 1_200_018, 1_200_036, 2, "price_change", "buy", 0.6, 5.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, Some(1));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let out = store.scan_book_updates(VENUE, "TOK_A", window).unwrap();
    assert_eq!(out.len(), 1, "this store scopes on ts, so only the delta is in the window");
    assert_eq!(out[0].kind, BookUpdateKind::Delta);
    assert!(
        !out.iter().any(|u| u.kind == BookUpdateKind::Snapshot),
        "the anchor ARRIVED inside this window and is still not returned — the divergence"
    );
    // ...and the sibling's `local_ts` window holds both rows, which is what makes the two
    // stores answer differently for the identical `TsRange`.
    for (ts, local_ts) in [(anchor_venue_ts, arrived_in_window), (1_200_018, 1_200_036)] {
        assert!(
            local_ts >= window.start.unwrap() && local_ts <= window.end.unwrap(),
            "both rows arrive inside the window ({ts}, {local_ts})"
        );
    }
    // Nothing this store returned has a `local_ts` outside the window, so the per-scan warning
    // stays silent here: this store can only ever see one direction of the difference.
    assert_eq!(
        other_clock_excludes(WindowClock::Ts, out.iter().map(|u| (u.ts, u.local_ts)), window),
        0,
    );
}

#[test]
fn scan_book_updates_wrong_venue_is_empty_not_an_error() {
    let rows: Vec<Row<'_>> =
        vec![("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);
    assert!(store.scan_book_updates("binance", "TOK_A", TsRange::all()).unwrap().is_empty());
    assert!(store.scan_trades("binance", "TOK_A", TsRange::all()).unwrap().is_empty());
}

#[test]
fn unknown_token_scans_empty_and_plans_zero_selected_row_groups() {
    let rows: Vec<Row<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,1.0]]", "[]", 0.01, ""),
        ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,1.0]]", "[]", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, Some(1));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    assert!(store.scan_book_updates(VENUE, "TOK_NOPE", TsRange::all()).unwrap().is_empty());
    assert!(store.scan_trades(VENUE, "TOK_NOPE", TsRange::all()).unwrap().is_empty());
    let plan = store.plan("TOK_NOPE", TsRange::all()).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].selected_row_groups, 0, "an absent token selects no row groups");
    assert_eq!(plan[0].total_row_groups, 2);
}

#[test]
fn plan_prunes_to_the_matching_token_row_group_only() {
    // Distinct, SORTED token ids -> one row group per id (max_row_group_size=1): each group's
    // min==max==that id, an unambiguous test of the pruning path (mirrors
    // `vike_archive::select_row_groups_prunes_to_the_matching_groups_only`, but exercised
    // through the store's own `plan`, over a REAL family-schema file).
    let rows: Vec<Row<'_>> = ["TOK_A", "TOK_B", "TOK_C", "TOK_D"]
        .iter()
        .enumerate()
        .map(|(i, tok)| {
            (*tok, i as i64, i as i64, i as u64, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")
        })
        .collect();
    let bytes = write_family_parquet(&rows, Some(1));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let plan = store.plan("TOK_B", TsRange::all()).unwrap();
    assert_eq!(plan[0].total_row_groups, 4);
    assert_eq!(plan[0].selected_row_groups, 1, "only TOK_B's own row group");
    assert!(plan[0].selected_compressed_bytes < plan[0].total_compressed_bytes);
}

#[test]
fn plan_prunes_by_ts_range_too() {
    // One token, several ts values, one row group per row: a narrow ts window should select
    // only the row groups whose ts range overlaps it, independent of token pruning (every row
    // is the SAME token here, so token-axis pruning selects everything — this isolates the
    // ts-axis pruning this module adds on top of `vike_archive::select_row_groups`).
    let rows: Vec<Row<'_>> = (0..5)
        .map(|i| {
            let ts = i * 1000;
            ("TOK_A", ts, ts, i as u64, "book", "none", 0.0, 0.0, "[]", "[]", 0.01, "")
        })
        .collect();
    let bytes = write_family_parquet(&rows, Some(1));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let plan = store.plan("TOK_A", TsRange::of(2000, 2000)).unwrap();
    assert_eq!(plan[0].total_row_groups, 5);
    assert_eq!(plan[0].selected_row_groups, 1, "only the ts=2000 row group overlaps");

    let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::of(2000, 2000)).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].ts, 2000);
}

#[test]
fn pruned_scan_matches_an_unpruned_full_scan_over_the_same_file() {
    // Equivalence check: decoding through the row-group-pruned `ArchiveParquetHistStore` path
    // must return EXACTLY the rows an unpruned, whole-file decode of the same bytes would —
    // pruning must never drop or alter a real match.
    let rows: Vec<Row<'_>> = (0..12)
        .map(|i| {
            let tok = if i % 3 == 0 {
                "TOK_A"
            } else if i % 3 == 1 {
                "TOK_B"
            } else {
                "TOK_C"
            };
            let ts = i as i64 * 10;
            (tok, ts, ts, i as u64, "price_change", "buy", 0.1 * (i as f64), 1.0, "", "", 0.01, "")
        })
        .collect();
    let bytes = write_family_parquet(&rows, Some(2));
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    // Unpruned reference: decode every row group, filter in Rust exactly like the pure decode
    // fn does, over the SAME bytes.
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap();
    let mut reference = Vec::new();
    for batch in reader {
        reference
            .extend(book_updates_from_batch(&batch.unwrap(), "TOK_B", TsRange::all()).unwrap());
    }
    reference.sort_by(|a: &BookUpdate, b: &BookUpdate| a.ts.cmp(&b.ts).then(a.seq.cmp(&b.seq)));

    let pruned = store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap();
    assert_eq!(format!("{pruned:?}"), format!("{reference:?}"));
    assert_eq!(pruned.len(), 4, "12 rows / 3 tokens round-robin -> 4 TOK_B rows");
}

// ---- construction ------------------------------------------------------------------------

#[test]
fn from_dir_picks_up_every_parquet_file_sorted_and_merges_across_files() {
    let dir = tempfile::tempdir().unwrap();
    let day1: Vec<Row<'_>> =
        vec![("TOK_A", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,1.0]]", "[]", 0.01, "")];
    let day2: Vec<Row<'_>> =
        vec![("TOK_A", 200, 200, 1, "price_change", "buy", 0.6, 2.0, "", "", 0.01, "")];
    write_temp_parquet(dir.path(), "btc5m_2026-07-27.parquet", &write_family_parquet(&day1, None));
    write_temp_parquet(dir.path(), "btc5m_2026-07-28.parquet", &write_family_parquet(&day2, None));
    // A non-Parquet file in the same directory must be ignored.
    std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();

    let store = ArchiveParquetHistStore::from_dir(dir.path()).unwrap();
    assert_eq!(store.files().len(), 2, "only the two .parquet files, not manifest.json");

    let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(out.len(), 2, "rows merged across both day files");
    assert_eq!(out[0].ts, 100);
    assert_eq!(out[1].ts, 200);
}

#[test]
fn writes_are_rejected() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    assert!(store.append_bars(VENUE, "T", "1m", &[], None).is_err());
    assert!(store.append_quotes(VENUE, "T", &[], None).is_err());
    assert!(store.append_trades(VENUE, "T", &[], None).is_err());
    assert!(store.append_book_updates(VENUE, "T", &[], None).is_err());
    assert!(store.resample_quotes_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
    assert!(store.resample_trades_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
}

/// Every lane this store CANNOT hold says so, says WHICH, and says it on the WRITE side too.
///
/// ⚠ `scan_funding` was the first (`docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`
/// verdict 6, 2026-09-21) and stood alone here under a sibling test that PINNED the other four
/// as still answering `[]`. That pin is now deleted, because the four joined it: the three
/// account arms are this store's own (`ArchiveParquetHistStore::scan_equity`'s doc carries the
/// argument) and `scan_chain` came through the trait default, which refuses for every impl that
/// never wrote an arm.
///
/// The message must NAME the kind in the store's own `kind=` vocabulary — an operator reading
/// `hist unsupported: …` has to learn which question was refused, since the cure is a different
/// store rather than a fix here.
///
/// ⚠ **The two APPEND cases are here because for one review round NOTHING IN THE TREE executed
/// those two default bodies**, and they are the more dangerous half rather than a symmetry:
/// `crate::ChainRecorder`'s `record` logs a store error and drops the batch, so under the old
/// `Ok(0)` a recorder aimed at a chain-less store had nothing to log — it recorded for hours and
/// reported success every cadence bucket. The tree declares exactly THREE overrides of either
/// verb — `crate::DataFusionHist`, `crate::test_support::MemHistStore` and
/// `crates/vike-datahub-client/src/remote.rs`'s `RemoteHistStore` — and every existing test
/// drove one of them, while `writes_are_rejected` above drives this store's OWN read-only
/// rejections and names neither of these two. This store inherits both defaults whole and this
/// test calls them, so reverting either body to `Ok(0)` fails right here.
///
/// ⚠ **The ACCOUNT-plane rows are a COMPLETENESS check over `vike_model::ACCOUNT_KINDS`, not a
/// hand-typed list** — the per-roster playbook's shape. Two breaks it exists to catch, neither
/// of which reddened anything while the list was free literals: a FIFTH account kind landing on
/// an empty trait default, which this store would fabricate `[]` for; and a RENAME, which this
/// family has already had once (0079's `funding` → `exec_funding`) and which would otherwise
/// leave `no_account_plane` naming a kind that no longer exists while a test carrying the same
/// stale literal still passed.
#[test]
fn every_kind_this_store_cannot_hold_refuses_and_names_itself() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    let cases: Vec<(&str, Result<(), DataError>)> = vec![
        ("equity", store.scan_equity(VENUE, "T", TsRange::all()).map(|_| ())),
        ("exec_fill", store.scan_exec_fills(VENUE, "T").map(|_| ())),
        ("exec_order", store.scan_exec_orders(VENUE, "T").map(|_| ())),
        ("exec_funding", store.scan_funding(VENUE, "T", TsRange::all()).map(|_| ())),
        ("chain", store.scan_chain(VENUE, "T", TsRange::all()).map(|_| ())),
        // The two WRITE halves, both inherited from the trait — this store declares no arm for
        // either, which is why the default bodies execute here and nowhere else.
        ("exec_funding", store.append_funding(VENUE, "T", &[], None).map(|_| ())),
        ("chain", store.append_chain_snapshot(VENUE, "T", &[], None).map(|_| ())),
        // Derived over `scan_chain`, so it must PROPAGATE the refusal rather than fold an
        // inherited empty into a confident "nothing was recorded at or before ts".
        ("chain", store.chain_as_of(VENUE, "T", 1_000).map(|_| ())),
    ];

    // THE ROSTER TIE. `vike_model::ACCOUNT_KINDS` is the one declaration of which kinds hold the
    // operator's own activity, and `crates/vike-data/tests/store_kind_gate.rs`'s
    // `the_store_agrees_with_the_declared_account_plane` already holds it equal to
    // `STORE_KINDS`. Every entry must be driven above; an undriven one is this store answering
    // `[]` for an account kind with nothing noticing, which is the whole defect below.
    let driven: std::collections::BTreeSet<&str> = cases.iter().map(|(k, _)| *k).collect();
    let undriven: Vec<&str> =
        vike_model::ACCOUNT_KINDS.iter().copied().filter(|k| !driven.contains(k)).collect();
    assert!(
        undriven.is_empty(),
        "the account plane grew or was renamed: {undriven:?} is in vike_model::ACCOUNT_KINDS \
             and is driven by no case here. Add its verb — and check this store REFUSES it rather \
             than inheriting an empty trait default, which is what an unlisted kind gets."
    );

    for (kind, got) in cases {
        // Composed, not spelled: `no_account_plane`'s doc argues why a `"kind=` literal must
        // not appear in this crate outside the path builders.
        let needle = ["kind", kind].join("=");
        match got {
            Err(DataError::Unsupported(msg)) => assert!(
                msg.contains(&needle),
                "the refusal must name the kind it refused as `{needle}`; got: {msg}"
            ),
            other => panic!(
                "an archive of market Parquet must SAY it cannot hold {needle}, not answer \
                     empty; got {other:?}"
            ),
        }
    }

    // ---- THE ANTI-VACUITY CONTROL, and it is INSIDE this test on purpose -------------------
    //
    // What the refusals above are worth depends entirely on this store not refusing
    // EVERYTHING: the claim is that they are a property of the account plane and of the kinds
    // this backend has no lane for, NOT of a handle built from zero files. Only an `is_ok()` on
    // the SAME `store` binding carries that.
    //
    // ⚠ For one review round it did not. The control was a sibling `#[test]` —
    // `bars_and_properties_are_empty_and_that_empty_is_a_real_answer`, itself the survivor of
    // `bars_and_account_series_are_always_empty_not_faked`, which asserted seven empties and
    // lost five as each lane stopped faking one (`scan_quotes` first — a real derived-L1 verb
    // now, and the module doc carries what believing its empty cost — then `scan_funding`, then
    // the three account lanes and `scan_chain`). It built its OWN `from_files(Vec::new())`, so
    // the pairing its doc claimed was two handles and nothing joining them: seeding either one
    // would have left the claim written and held by nothing, both tests green.
    //
    // These two lanes keep their empty `Ok` deliberately —
    // `ArchiveParquetHistStore::scan_equity`'s doc argues the asymmetry, and
    // `crates/vike-backtest/src/hist_replay.rs`'s per-tick `properties_source` is the caller a
    // refusal would make WORSE.
    assert_eq!(store.load_bars(VENUE, "T", "1m", TsRange::all()).unwrap(), vec![]);
    assert_eq!(store.scan_symbol_properties(VENUE, "T", TsRange::all()).unwrap(), vec![]);
    // Derived over `scan_symbol_properties`, so it inherits that empty rather than a refusal.
    assert_eq!(store.properties_as_of(VENUE, "T", 1_000).unwrap(), None);
}

/// ⚠ **The same latent defect is still live on the LAST TWO kinds**, and this test records it
/// rather than leaving the asymmetry to be rediscovered — it is the successor of the pin that
/// covered the four lanes the test above now proves refuse.
///
/// `crate::hist`'s cohort section carries the argument for why `cohort` and `perp_metrics` were
/// left: their defaults are FORWARDED by `crates/vike-user-research/src/contract.rs`'s
/// sanctioned study surface, so the refusal reaches user code, which is a different decision
/// from the one made here and wants its own evidence.
///
/// This is a PIN, not a fix: it fails the day somebody makes one of them refuse, which is the
/// day to delete its line here and note the record that did it.
#[test]
fn the_last_two_kinds_still_answer_empty_rather_than_refusing() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    assert!(store.scan_cohort(VENUE, "T", TsRange::all()).is_ok());
    assert!(store.scan_perp_metrics(VENUE, "T", TsRange::all()).is_ok());
}

/// The core of the derived-L1 fold: a snapshot seeds the book, deltas move it, and ONE
/// `QuoteTick` is emitted per update whose top of book actually changed — carrying real sizes
/// off the ladder, never a fabricated one.
#[test]
fn derived_l1_emits_a_quote_per_top_of_book_change_with_real_sizes() {
    let rows: Vec<Row<'_>> = vec![
        // snapshot: L1 = 0.50 x 10 / 0.51 x 12
        (
            "TOK",
            100,
            101,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,10.0],[0.49,50.0]]",
            "[[0.51,12.0],[0.52,60.0]]",
            0.01,
            "",
        ),
        // delta on a DEEPER bid level (0.49) — L1 unchanged, must emit NOTHING
        ("TOK", 200, 201, 2, "price_change", "buy", 0.49, 99.0, "", "", 0.01, ""),
        // delta REMOVING the best ask (qty 0) — best ask steps 0.51 -> 0.52, L1 changed
        ("TOK", 300, 301, 3, "price_change", "sell", 0.51, 0.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert_eq!(q.len(), 2, "snapshot + the L1-moving delta only, not the deep-level one: {q:?}");

    assert_eq!(q[0].ts, 100);
    assert_eq!(q[0].local_ts, 101, "the update's own local_ts, not its ts");
    near(q[0].bid, 0.5, "snapshot bid");
    near(q[0].ask, 0.51, "snapshot ask");
    assert_eq!(q[0].bid_size, 10.0, "size comes off the ladder, never fabricated");
    assert_eq!(q[0].ask_size, 12.0);
    assert_eq!(q[0].symbol, "TOK");

    assert_eq!(q[1].ts, 300, "the deep-level delta at ts=200 emitted nothing");
    near(q[1].bid, 0.5, "bid untouched by an ask-side delta");
    assert_eq!(q[1].bid_size, 10.0);
    near(q[1].ask, 0.52, "best ask stepped up when 0.51 was removed");
    assert_eq!(q[1].ask_size, 60.0);
}

/// Price comparison for the derived-L1 fold. Prices round-trip through `L2Book`'s tick INDEX
/// (`price_of(tick) = tick * tick_size`), so `51 * 0.01` need not be bit-identical to the
/// literal `0.51` — an exact `assert_eq!` here would be testing f64 representation, not the
/// fold. Half a tick is the meaningful tolerance: anything larger is a real mis-price.
fn near(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() < 0.005, "{what}: got {got}, want {want}");
}

/// A one-sided book emits NO quote. A `QuoteTick` with a 0.0 leg is not a quote, and a strategy
/// that priced against it would be quoting into a phantom side.
#[test]
fn derived_l1_skips_one_sided_books_rather_than_emitting_a_zero_leg() {
    let rows: Vec<Row<'_>> = vec![
        // bids only — no ask at all
        ("TOK", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[]", 0.01, ""),
        // the ask side arrives; NOW there is a two-sided quote
        ("TOK", 200, 200, 2, "price_change", "sell", 0.55, 4.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert_eq!(q.len(), 1, "only the two-sided state quotes: {q:?}");
    assert_eq!(q[0].ts, 200);
    near(q[0].bid, 0.5, "bid");
    near(q[0].ask, 0.55, "ask");
    assert_eq!((q[0].bid_size, q[0].ask_size), (10.0, 4.0));
}

/// Write an `l1_quotes`-shaped Parquet file (the archive's own L1 stream schema:
/// `token_id, condition_id, ts, local_ts, bid, ask, bid_size, ask_size`).
fn write_l1_quotes_parquet(rows: &[(&str, i64, i64, f64, f64, f64, f64)]) -> Vec<u8> {
    use datafusion::arrow::array::{Decimal128Array, Int64Array, StringArray};
    let schema = Arc::new(Schema::new(vec![
        Field::new("token_id", DataType::Utf8, false),
        Field::new("condition_id", DataType::Utf8, false),
        Field::new("ts", DataType::Int64, false),
        Field::new("local_ts", DataType::Int64, false),
        Field::new("bid", DataType::Decimal128(9, 4), false),
        Field::new("ask", DataType::Decimal128(9, 4), false),
        Field::new("bid_size", DataType::Decimal128(18, 6), false),
        Field::new("ask_size", DataType::Decimal128(18, 6), false),
    ]));
    // Scale a column of f64s into the archive's fixed-point Decimal128 encoding.
    let dec_col = |vals: Vec<f64>, scale: f64, precision: u8, q: i8| {
        Decimal128Array::from(
            vals.iter().map(|v| (v * scale).round() as i128).collect::<Vec<i128>>(),
        )
        .with_precision_and_scale(precision, q)
        .unwrap()
    };
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(rows.iter().map(|r| r.0).collect::<Vec<&str>>())),
        Arc::new(StringArray::from(rows.iter().map(|_| "cond").collect::<Vec<&str>>())),
        Arc::new(Int64Array::from(rows.iter().map(|r| r.1).collect::<Vec<i64>>())),
        Arc::new(Int64Array::from(rows.iter().map(|r| r.2).collect::<Vec<i64>>())),
        Arc::new(dec_col(rows.iter().map(|r| r.3).collect(), 10_000.0, 9, 4)),
        Arc::new(dec_col(rows.iter().map(|r| r.4).collect(), 10_000.0, 9, 4)),
        Arc::new(dec_col(rows.iter().map(|r| r.5).collect(), 1_000_000.0, 18, 6)),
        Arc::new(dec_col(rows.iter().map(|r| r.6).collect(), 1_000_000.0, 18, 6)),
    ];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    let mut buf = Vec::new();
    let mut w =
        ArrowWriter::try_new(&mut buf, schema, Some(WriterProperties::builder().build())).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
    buf
}

/// **The archive's own `l1_quotes` stream WINS over deriving from book depth.**
///
/// Every date partition — flat and family alike — ships `l1_quotes.parquet` beside
/// `book_events.parquet`, because the recorder writes L1 and L2 as separate streams. When both
/// are present, the recorded stream is what the venue actually published; derived L1 is a
/// reconstruction and is the fallback for holding only the book file.
///
/// The values here are deliberately IMPOSSIBLE to derive from the book (bid 0.11/ask 0.12 with
/// sizes 1/2, against a book of 0.5/0.51 x10/x12), so the assertion can only pass if the
/// recorded file was actually used.
#[test]
fn recorded_l1_quotes_win_over_derived_when_both_files_are_present() {
    let book: Vec<Row<'_>> = vec![(
        "TOK",
        100,
        100,
        1,
        "book",
        "none",
        0.0,
        0.0,
        "[[0.5,10.0]]",
        "[[0.51,12.0]]",
        0.01,
        "",
    )];
    let dir = tempfile::tempdir().unwrap();
    let book_path =
        write_temp_parquet(dir.path(), "book_events.parquet", &write_family_parquet(&book, None));
    let q_path = write_temp_parquet(
        dir.path(),
        "l1_quotes.parquet",
        &write_l1_quotes_parquet(&[("TOK", 100, 101, 0.11, 0.12, 1.0, 2.0)]),
    );

    // Book only -> derived L1 (the fallback).
    let derived = ArchiveParquetHistStore::from_files([book_path.clone()]);
    let d = derived.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert_eq!(d.len(), 1);
    near(d[0].bid, 0.5, "derived bid");

    // Both files -> the RECORDED stream, not the book-derived one.
    let both = ArchiveParquetHistStore::from_files([book_path, q_path]);
    let r = both.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert_eq!(r.len(), 1, "{r:?}");
    near(r[0].bid, 0.11, "recorded bid — not the book's 0.5");
    near(r[0].ask, 0.12, "recorded ask — not the book's 0.51");
    assert_eq!(r[0].bid_size, 1.0);
    assert_eq!(r[0].ask_size, 2.0);
    assert_eq!(r[0].local_ts, 101, "the recorded stream's own local_ts");

    // And the book lane still works with a quote file alongside — the l1_quotes file must not
    // be fed to the book decoder, which would be a hard schema error, not an empty result.
    assert_eq!(both.scan_book_updates(VENUE, "TOK", TsRange::all()).unwrap().len(), 1);
    assert!(both.scan_trades(VENUE, "TOK", TsRange::all()).unwrap().is_empty());
}

/// **The anchor gate.** Deltas alone never produce a quote: a range scan of a delta stream
/// starts with an EMPTY book, so its "top of book" is the best of a partial ladder, not the
/// real one.
///
/// This is the fix for a measured, real divergence (`derived_l1_divergence_report`). Scanning a
/// live market's opening 300s window, the fold reported 0.42/0.65, 0.42/0.56, 0.49/0.56,
/// 0.49/0.55 while the recorder's own L1 said 0.50/0.51 — and became exact at the very
/// millisecond the first snapshot landed (ts=…000179), agreeing event-for-event on all 12,687
/// quotes after it. Four confident, wrong quotes at the open of every market.
#[test]
fn derived_l1_emits_nothing_until_a_snapshot_anchors_the_book() {
    let rows: Vec<Row<'_>> = vec![
        // deltas only — a two-sided book forms, but it was never anchored
        ("TOK", 100, 100, 1, "price_change", "buy", 0.42, 30.0, "", "", 0.01, ""),
        ("TOK", 110, 110, 2, "price_change", "sell", 0.65, 30.0, "", "", 0.01, ""),
        ("TOK", 120, 120, 3, "price_change", "sell", 0.55, 28.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);
    let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert!(
        q.is_empty(),
        "an un-anchored book must emit nothing — a partial ladder's best is not the L1: {q:?}"
    );

    // Same stream, with a snapshot in front: now it anchors and emits from there.
    let rows: Vec<Row<'_>> = vec![
        ("TOK", 90, 90, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,12.0]]", 0.01, ""),
        ("TOK", 100, 100, 2, "price_change", "buy", 0.42, 30.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir2 = tempfile::tempdir().unwrap();
    let path2 = write_temp_parquet(dir2.path(), "day.parquet", &bytes);
    let store2 = ArchiveParquetHistStore::from_files([path2]);
    let q2 = store2.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    assert_eq!(q2.len(), 1, "the snapshot anchors; the deeper 0.42 bid does not move L1: {q2:?}");
    near(q2[0].bid, 0.5, "anchored bid");
    near(q2[0].ask, 0.51, "anchored ask");
}

/// A mid-stream `GapStart` UN-anchors the book: no quotes until the next `Snapshot` re-seeds
/// it, because data between the two is by definition missing.
///
/// Found the same way as the cold-start gate — by drilling one real market. Token 9119933407…
/// carries `gap_start` at ts=1785026504199, and the derived stream's first disagreement with
/// the recorder's L1 lands 1.2s later at ts=1785026505435, with `live_resume` at
/// ts=1785026505552. Folding deltas across a gap keeps applying them to a book missing whatever
/// was lost, so the L1 is confidently wrong until an anchor repairs it.
///
/// `LiveResume` must NOT re-anchor on its own: its doc says "the re-seed `Snapshot` follows",
/// so the transport is healthy again while the book still is not.
#[test]
fn derived_l1_gap_unanchors_until_the_next_snapshot() {
    let rows: Vec<Row<'_>> = vec![
        (
            "TOK",
            100,
            100,
            1,
            "book",
            "none",
            0.0,
            0.0,
            "[[0.5,10.0]]",
            "[[0.51,12.0],[0.52,20.0]]",
            0.01,
            "",
        ),
        // anchored: removing the best ask steps L1 to 0.52 — still two-sided, so it emits
        ("TOK", 200, 200, 2, "price_change", "sell", 0.51, 0.0, "", "", 0.01, ""),
        // transport lost — everything from here is untrustworthy
        ("TOK", 300, 300, 0, "status", "none", 0.0, 0.0, "", "", 0.01, "gap_start"),
        // a delta across the gap must NOT emit (the book is missing whatever was lost)
        ("TOK", 400, 400, 3, "price_change", "buy", 0.53, 5.0, "", "", 0.01, ""),
        // live again — but the book is still not valid, so still nothing
        ("TOK", 500, 500, 0, "status", "none", 0.0, 0.0, "", "", 0.01, "live_resume"),
        ("TOK", 600, 600, 4, "price_change", "buy", 0.54, 6.0, "", "", 0.01, ""),
        // the re-seed snapshot: anchored again, emits from here
        ("TOK", 700, 700, 5, "book", "none", 0.0, 0.0, "[[0.6,20.0]]", "[[0.61,22.0]]", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    let q = store.scan_quotes(VENUE, "TOK", TsRange::all()).unwrap();
    let ts: Vec<i64> = q.iter().map(|x| x.ts).collect();
    assert_eq!(
        ts,
        vec![100, 200, 700],
        "emit while anchored (100, 200), nothing across the gap (400, 600 — even after \
             live_resume at 500), then again from the re-seed snapshot (700): {q:?}"
    );
    near(q[2].bid, 0.6, "re-anchored bid");
    near(q[2].ask, 0.61, "re-anchored ask");
}

/// The quote lane honors venue and range exactly like the book lane it folds.
#[test]
fn derived_l1_respects_venue_and_range() {
    let rows: Vec<Row<'_>> = vec![
        ("TOK", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK", 9_000, 9_000, 2, "price_change", "buy", 0.52, 3.0, "", "", 0.01, ""),
    ];
    let bytes = write_family_parquet(&rows, None);
    let dir = tempfile::tempdir().unwrap();
    let path = write_temp_parquet(dir.path(), "day.parquet", &bytes);
    let store = ArchiveParquetHistStore::from_files([path]);

    assert!(
        store.scan_quotes("binance", "TOK", TsRange::all()).unwrap().is_empty(),
        "a non-polymarket venue has nothing here"
    );
    let q = store.scan_quotes(VENUE, "TOK", TsRange::of(0, 1_000)).unwrap();
    assert_eq!(q.len(), 1, "the ts=9000 update is out of range: {q:?}");
    assert_eq!(q[0].ts, 100);
}

/// `Arc<dyn HistStore + Send + Sync>` must accept this store — the exact shape
/// `harness::run_backtest`/`hist_replay::replay_ticks` take.
#[test]
fn is_usable_as_a_dyn_hist_store_trait_object() {
    let store: std::sync::Arc<dyn HistStore + Send + Sync> =
        std::sync::Arc::new(ArchiveParquetHistStore::from_files(Vec::new()));
    assert!(store.scan_book_updates(VENUE, "T", TsRange::all()).unwrap().is_empty());
}

// ---- derived-L1 vs recorded-l1_quotes divergence report (never run in CI) --------------------
//
// The backtest over 580 BTC-5m markets agreed to within ~0.26pp between this store's DERIVED L1
// and a `DataFusionHist` whose quote lane came from the recorder's own `l1_quotes` (via
// ClickHouse). Close — but not zero, and "close" is not an explanation. This walks the SAME
// universe market by market and localises the residual: which markets diverge, by how much, and
// then event by event inside the worst one.
//
// The two sources are deliberately different, so SOME divergence is expected; the question is
// only whether it is the expected kind:
//   - derived L1 = fold this file's own book depth, emit on top-of-book change
//   - recorded l1_quotes = the live recorder's own L1 capture, its own timestamps, its own dedup
//
// Self-skips when any input is absent — same idiom as the live measure below. Run manually:
// `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::derived_l1_divergence_report -- --ignored --nocapture`
#[test]
#[ignore]
fn derived_l1_divergence_report() {
    const ARCHIVE: &str = "/var/lib/vike/dl/btc5m_2026-07-26.parquet";
    const STORE: &str = "/var/lib/vike/btc_store";
    const UNIVERSE: &str = "/tmp/uni_final.tsv";
    const TENOR_MS: i64 = 300_000;
    // How many markets to walk. ONE by default: the point is to LOCALISE a divergence, and one
    // market answers "is there one, and what shape is it" in ~14s where the full 580 costs ~20
    // minutes. Widen by writing a count into `<UNIVERSE>.markets` — libtest REJECTS unknown CLI
    // flags ("Unrecognized option"), and an env var would need a settings-registry row for what
    // is a test-only knob.
    let max_markets: usize = std::fs::read_to_string(format!("{UNIVERSE}.markets"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);
    for p in [ARCHIVE, STORE, UNIVERSE] {
        if !Path::new(p).exists() {
            eprintln!("{p} not present — skipping derived-L1 divergence report");
            return;
        }
    }
    let universe = std::fs::read_to_string(UNIVERSE).unwrap();
    let markets: Vec<(String, i64)> = universe
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let _family = f.next()?;
            let token = f.next()?.to_string();
            let end: i64 = f.next()?.trim().parse().ok()?;
            Some((token, end))
        })
        .collect();
    assert!(!markets.is_empty(), "universe parsed empty");

    let archive = ArchiveParquetHistStore::from_files([PathBuf::from(ARCHIVE)]);
    let store = crate::DataFusionHist::open(STORE).expect("open reference store");

    // (token, derived_count, recorded_count, market_end, first disagreeing ts)
    let mut rows: Vec<(String, usize, usize, i64, Option<i64>)> = Vec::new();
    for (token, end) in markets.iter() {
        if rows.len() >= max_markets {
            break;
        }
        let range = TsRange::of(end - TENOR_MS, *end);
        let d = archive.scan_quotes(VENUE, token, range).unwrap();
        let r = store.scan_quotes(VENUE, token, range).unwrap();
        if d.is_empty() && r.is_empty() {
            continue; // market outside this file's day — not a divergence
        }
        let first_bad = first_l1_disagreement(&d, &r);
        rows.push((token.clone(), d.len(), r.len(), *end, first_bad));
    }
    assert!(!rows.is_empty(), "no market had quotes on either side");

    let with_data = rows.len();
    let agreeing = rows.iter().filter(|r| r.4.is_none()).count();
    let total_d: usize = rows.iter().map(|r| r.1).sum();
    let total_r: usize = rows.iter().map(|r| r.2).sum();
    let mut by_gap: Vec<&(String, usize, usize, i64, Option<i64>)> = rows.iter().collect();
    by_gap.sort_by_key(|r| -((r.1 as i64 - r.2 as i64).abs()));

    eprintln!("\n=== derived-L1 vs recorded-l1_quotes, {with_data} markets with data ===");
    eprintln!("markets whose L1 step function NEVER disagrees: {agreeing}/{with_data}");
    eprintln!("total quotes: derived={total_d} recorded={total_r}");
    eprintln!("\nworst 10 by |count gap|:");
    eprintln!(
        "{:<22} {:>9} {:>9} {:>8}  first-disagreement-ts",
        "token", "derived", "recorded", "gap"
    );
    for r in by_gap.iter().take(10) {
        eprintln!(
            "{:<22} {:>9} {:>9} {:>8}  {}",
            &r.0[..22.min(r.0.len())],
            r.1,
            r.2,
            r.1 as i64 - r.2 as i64,
            r.4.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
        );
    }

    // Event-by-event inside the worst market, so the SHAPE of the divergence is visible rather
    // than inferred from counts.
    // Dump the first market that actually DISAGREES, falling back to the worst count gap when
    // none does. A count gap alone is usually just the anchor gate suppressing the pre-anchor
    // head start — benign and already understood — whereas a step-function disagreement is the
    // thing still unexplained, so that is what deserves the event-by-event look.
    if let Some(worst) = by_gap.iter().find(|r| r.4.is_some()).or_else(|| by_gap.first()) {
        let range = TsRange::of(worst.3 - TENOR_MS, worst.3);
        let d = archive.scan_quotes(VENUE, &worst.0, range).unwrap();
        let r = store.scan_quotes(VENUE, &worst.0, range).unwrap();
        if let Some(bad) = worst.4 {
            eprintln!("\nfirst disagreement at ts={bad}; window opens at {}", worst.3 - TENOR_MS);
            // STRADDLE the disagreement: the last 3 events at or before it and the first 5 at
            // or after, per side. "First N within a window" truncates before the interesting
            // moment whenever the stream is dense — which is exactly when it matters.
            let straddle = |v: &[QuoteTick], label: &str| {
                let split = v.partition_point(|q| q.ts < bad);
                eprintln!("--- {label}, straddling the disagreement ---");
                for q in v[split.saturating_sub(3)..(split + 5).min(v.len())].iter() {
                    eprintln!(
                        "  {}ts={} bid={} x{} ask={} x{}",
                        if q.ts >= bad { ">" } else { " " },
                        q.ts,
                        q.bid,
                        q.bid_size,
                        q.ask,
                        q.ask_size
                    );
                }
            };
            straddle(&d, "derived");
            straddle(&r, "recorded");
        }
        eprintln!("\n=== event-by-event, worst market {} ===", &worst.0[..22.min(worst.0.len())]);
        eprintln!("--- derived (first 20) ---");
        for q in d.iter().take(20) {
            eprintln!("  ts={} bid={} x{} ask={} x{}", q.ts, q.bid, q.bid_size, q.ask, q.ask_size);
        }
        eprintln!("--- recorded (first 20) ---");
        for q in r.iter().take(20) {
            eprintln!("  ts={} bid={} x{} ask={} x{}", q.ts, q.bid, q.bid_size, q.ask, q.ask_size);
        }
    }
}

/// First ts at which the two quote streams disagree about the L1 **in force at that instant**.
///
/// Compared as STEP FUNCTIONS, not element-wise: the two sources are allowed to emit at
/// different moments (different dedup, different capture), so zipping them by index would
/// report every stream as totally divergent and explain nothing. Instead, at each event ts in
/// either stream, ask both "what is your latest quote at or before this ts?" and compare those.
/// Prices compare within half a tick, for the same reason [`near`] does. `None` = the two never
/// disagree anywhere in the window.
fn first_l1_disagreement(a: &[QuoteTick], b: &[QuoteTick]) -> Option<i64> {
    // TIE-TOLERANT: when several events share a timestamp the feed defines NO order among
    // them, and the two pipelines sort independently, so "the state after ts" can legitimately
    // differ by which same-ts event happens to be last. Measured (token 1151341668…, ts=364):
    // both streams carried the same two events, `bid=0.41 x13.3` and `bid=0.4 x208.58`, in
    // opposite order — identical before, identical after. Comparing the state-after-ts alone
    // reports that as a divergence, which is an artifact of the comparator, not of the data.
    //
    // So a ts agrees if the two sides carry the same SET of L1 states at it, or if the state
    // after it matches. Only a genuine difference in what was seen survives both.
    let states_at = |v: &[QuoteTick], ts: i64| -> Vec<(u64, u64)> {
        let mut s: Vec<(u64, u64)> = v
            .iter()
            .filter(|q| q.ts == ts)
            .map(|q| ((q.bid * 1000.0).round() as u64, (q.ask * 1000.0).round() as u64))
            .collect();
        s.sort_unstable();
        s
    };
    let latest_at = |v: &[QuoteTick], ts: i64| -> Option<(f64, f64)> {
        v.iter().rev().find(|q| q.ts <= ts).map(|q| (q.bid, q.ask))
    };
    let mut times: Vec<i64> = a.iter().map(|q| q.ts).chain(b.iter().map(|q| q.ts)).collect();
    times.sort_unstable();
    times.dedup();
    // Only compare once BOTH streams have started; a pure head-start is a count difference,
    // reported by the count columns, not a mid-stream divergence.
    let both_live = match (a.first(), b.first()) {
        (Some(x), Some(y)) => x.ts.max(y.ts),
        _ => return None,
    };
    for ts in times {
        if ts < both_live {
            continue;
        }
        if let (Some((ab, aa)), Some((bb, ba))) = (latest_at(a, ts), latest_at(b, ts)) {
            let after_matches = (ab - bb).abs() < 0.005 && (aa - ba).abs() < 0.005;
            if !after_matches && states_at(a, ts) != states_at(b, ts) {
                return Some(ts);
            }
        }
    }
    None
}

// ---- live measurement smoke (never run in CI; needs the real the CI box archive file) ------------
//
// Reports the MEASURE deliverable numbers this module's report is built on: wall clock, bytes
// read (via `plan`'s row-group byte accounting), row groups touched vs total, rows returned, and
// peak RSS (`/proc/self/status` `VmHWM`, Linux-only, best-effort) for ONE token's own market
// window vs the same query repeated over ~20 tokens. Self-skips when the file is absent — same
// idiom as `vike_archive.rs`'s `#[ignore]`d live smokes. Run manually:
// `cargo test -p vike-backfill --features vike-archive --lib archive_store::tests::live_measure_against_real_archive_file -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_measure_against_real_archive_file() {
    const FILE: &str = "/var/lib/vike/dl/btc5m_2026-07-28.parquet";
    if !Path::new(FILE).exists() {
        eprintln!("{FILE} not present — skipping live archive-store measurement");
        return;
    }

    fn vm_hwm_kb() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status.lines().find_map(|l| {
            l.strip_prefix("VmHWM:")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kb| kb.parse().ok())
        })
    }

    // 25 real token ids from this file's own day, oldest-window-first (harvested via
    // `clickhouse-local`'s standalone `GROUP BY token_id` against the same local file — no
    // server, no network). The first is used for the single-token case; all 25 for the
    // "~20 tokens" case (a realistic per-series backtest replay loop: one `scan_book_updates`
    // call per token, exactly like `hist_replay::replay_ticks` issues per series).
    const TOKENS: &[&str] = &[
        "23152407599655538885623805493900043467958601874029530629927915276594719410439",
        "104571443997234897304141858465981034372030063880357705082249379826847038542378",
        "14301790185379892771571254247303545757717650772133892621663602964384542523992",
        "78321747154810254051861975218789122051845823043795362938649577164074888086089",
        "97671438448529185350787963197240804605805884897399485407503693494408417168470",
        "26256870810957662813774985645741265151480872553185993802730211703418628116159",
        "53027238618278186187577262403781830403994334114229466701325039465002184402157",
        "96667851774266765992556672614454228715564184201094718939745824592319584816112",
        "104086802706154418012875514448736113322113701846888813699955218638729179713845",
        "69050896154587792492371870680355670647404731895517712766508577896915780266670",
        "21413240782782670867191403316273233750669966867951507759590252952907554173144",
        "64363682592539029771920593705361530319135028911791177669001455328725856174386",
        "57997709997188651018796419454040666912401809435049910488400909521576768666389",
        "36325126872656430136186868653334900065037869194804379074055328330745125586278",
        "79393919153640374967889303608748517110647328768868919134062021800595655663540",
        "67708259947866665757301650057559387706903035949289005175172523672038772089859",
        "9783993234210266699347245365566496415323137082179260422643701204848379402442",
        "80887692812220726900805645119609289512621731695120160205175177755807711515699",
        "47151877687402178187507313687642483587217295197325736942711148947667700139749",
        "98251074153187170609910333754018623687412278037670001730907786212785897353274",
    ];

    let store = ArchiveParquetHistStore::from_files([PathBuf::from(FILE)]);

    // ---- single token, whole file's ts range (its own window is well inside it) -----------
    let one = TOKENS[0];
    let t0 = std::time::Instant::now();
    let plan = store.plan(one, TsRange::all()).unwrap();
    let plan_ms = t0.elapsed().as_millis();
    let t1 = std::time::Instant::now();
    let rows = store.scan_book_updates(VENUE, one, TsRange::all()).unwrap();
    let scan_ms = t1.elapsed().as_millis();
    let rss = vm_hwm_kb();
    println!(
        "SINGLE token={one} plan_ms={plan_ms} scan_ms={scan_ms} rows={} \
             row_groups={}/{} bytes={}/{} vm_hwm_kb={rss:?}",
        rows.len(),
        plan[0].selected_row_groups,
        plan[0].total_row_groups,
        plan[0].selected_compressed_bytes,
        plan[0].total_compressed_bytes,
    );

    // ---- ~20 tokens, one scan_book_updates call each (a realistic replay loop) -------------
    let t2 = std::time::Instant::now();
    let mut total_rows = 0usize;
    let mut total_selected_rg = 0usize;
    let mut total_rg = 0usize;
    let mut total_selected_bytes: i64 = 0;
    let mut total_bytes: i64 = 0;
    for &tok in TOKENS {
        let p = store.plan(tok, TsRange::all()).unwrap();
        total_selected_rg += p[0].selected_row_groups;
        total_rg += p[0].total_row_groups;
        total_selected_bytes += p[0].selected_compressed_bytes;
        total_bytes += p[0].total_compressed_bytes;
        total_rows += store.scan_book_updates(VENUE, tok, TsRange::all()).unwrap().len();
    }
    let many_ms = t2.elapsed().as_millis();
    let rss_many = vm_hwm_kb();
    println!(
        "MANY tokens={} total_ms={many_ms} avg_ms={:.1} total_rows={total_rows} \
             row_groups_selected_sum={total_selected_rg} row_groups_total_sum={total_rg} \
             bytes_selected_sum={total_selected_bytes} bytes_total_sum={total_bytes} \
             vm_hwm_kb={rss_many:?}",
        TOKENS.len(),
        many_ms as f64 / TOKENS.len() as f64,
    );
}
