//! End to end over a multi-row-group Parquet file: token and range scoping, row-group pruning.

use super::*;

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
