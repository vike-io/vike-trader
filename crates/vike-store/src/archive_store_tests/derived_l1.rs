//! The derived-L1 quote fold: anchor gating, gaps, one-sided books, recorded L1 winning.

use super::*;

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
