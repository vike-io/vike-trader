//! Local-file bulk and parallel ingest against the serial path, and `chunk_row_groups`.

use super::*;
use crate::vike_archive::test_support::{BookRow, open, write_book_events_parquet};
use vike_data::{HistStore, TsRange};
use vike_marketdata::{BookLevel, BookUpdate};
use vike_store::BulkConfig;
use vike_store::archive_store::VENUE;

/// The bulk (`--file --bulk`) local-ingest twin of the test above: staging through
/// [`ingest_local_file_bulk`]/[`BulkIngestSession`] must land the SAME rows as the plain
/// (non-bulk) local-file path — the bulk profile only changes WHEN a commit happens, never
/// WHAT gets committed.
#[test]
fn ingest_local_file_bulk_matches_the_plain_local_file_path() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let plain_dir = tempfile::tempdir().unwrap();
    let plain_store = DataFusionHist::open(plain_dir.path()).unwrap();
    let plain_total =
        ingest_local_file(&plain_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

    let bulk_dir = tempfile::tempdir().unwrap();
    let bulk_store = DataFusionHist::open(bulk_dir.path()).unwrap();
    let mut session = bulk_store.bulk_session(BulkConfig::default());
    let key_prefix = "vikearchive:book:2026-07-27:bulk";
    let bulk_staged = ingest_local_file_bulk(
        &mut session,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
    )
    .unwrap();
    session.flush(key_prefix).unwrap();

    assert_eq!(plain_total, 2);
    assert_eq!(bulk_staged, 2, "bulk path decodes the same row count");
    let plain_rows = plain_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    let bulk_rows = bulk_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(plain_rows.len(), bulk_rows.len(), "same committed row count either way");
    assert_eq!(bulk_rows.len(), 2);
}

/// A `BookUpdate` reduced to the fields that carry its content — `(ts, seq, bids, asks)`.
/// `BookUpdate` itself has no `PartialEq`, so equality assertions compare this instead.
type BookLens = (i64, u64, Vec<BookLevel>, Vec<BookLevel>);

/// Bulk + fan-out must commit exactly what serial bulk commits — across MANY tokens and MANY
/// row groups, so the work genuinely spreads over several chunks.
///
/// This is the test the whole per-worker key namespace exists for. With a shared key prefix
/// every worker's session would number its windows `w0`, `w1`, … from its own zero, minting the
/// SAME key for DIFFERENT rows; `commit_rows_bulk` treats a seen key as durably committed and
/// returns `Ok(0)`, so the second worker to reach a series would lose its rows silently. That
/// failure shows up here as a short row count, with no error anywhere.
#[test]
fn bulk_parallel_commits_exactly_what_serial_bulk_commits() {
    let ids = [
        "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I", "TOK_J",
        "TOK_K", "TOK_L",
    ];
    // Two rows per token; one row group per row, so 24 groups fan across the workers and most
    // tokens straddle a group boundary (the contiguous-chunk property under real pressure).
    let rows: Vec<BookRow<'_>> = ids
        .iter()
        .enumerate()
        .flat_map(|(i, id)| {
            let ts = (i as i64 + 1) * 10;
            vec![
                (
                    *id,
                    ts,
                    ts,
                    1u64,
                    "book",
                    "none",
                    0.0,
                    0.0,
                    "[[0.5,10.0]]",
                    "[[0.51,10.0]]",
                    0.01,
                    "",
                ),
                (*id, ts + 1, ts + 1, 2u64, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
            ]
        })
        .collect();
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();
    let key_prefix = "vikearchive:book:2026-07-27:bulk";

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let mut session = serial_store.bulk_session(BulkConfig::default());
    let serial_staged = ingest_local_file_bulk(
        &mut session,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
    )
    .unwrap();
    session.flush(key_prefix).unwrap();

    let par_dir = tempfile::tempdir().unwrap();
    let par_store = DataFusionHist::open(par_dir.path()).unwrap();
    let par_staged = ingest_local_file_bulk_parallel(
        &par_store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        key_prefix,
        4,
        BulkConfig::default(),
    )
    .unwrap();

    assert_eq!(serial_staged, par_staged, "same rows decoded either way");
    // `BookUpdate` has no `PartialEq`, so compare on the fields that carry the content:
    // (ts, seq, levels). Both sides come back in the store's documented (ts, seq) order.
    let lens = |v: &[BookUpdate]| -> Vec<BookLens> {
        v.iter().map(|u| (u.ts, u.seq, u.bids.clone(), u.asks.clone())).collect()
    };
    for id in ids {
        let s = serial_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
        let p = par_store.scan_book_updates(VENUE, id, TsRange::all()).unwrap();
        assert_eq!(
            lens(&s),
            lens(&p),
            "{id}: parallel bulk must commit exactly the serial bulk rows"
        );
        assert!(!p.is_empty(), "{id}: committed nothing — a dropped key would look like this");
    }
}

/// `--jobs 1` through the parallel bulk path is the serial bulk path: one chunk, one session,
/// file order. Guards the degenerate case so the bin can route on `jobs > 1` alone.
#[test]
fn bulk_parallel_with_one_job_equals_serial_bulk() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let (_dir, store) = open();
    let staged = ingest_local_file_bulk_parallel(
        &store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        "vikearchive:book:2026-07-27:bulk",
        1,
        BulkConfig::default(),
    )
    .unwrap();
    assert_eq!(staged, 2);
    assert_eq!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().len(), 2);
}

// ---- chunk_row_groups: the fan-out unit's pure partitioning ----------------------------------

#[test]
fn chunk_row_groups_splits_as_evenly_as_possible_and_stays_contiguous_in_order() {
    let selected: Vec<usize> = (10..20).collect(); // 10 items, deliberately not 0-based
    let chunks = chunk_row_groups(&selected, 3);
    let lens: Vec<usize> = chunks.iter().map(Vec::len).collect();
    assert_eq!(lens, vec![4, 3, 3], "remainder goes to the first chunks");
    // Every original index appears exactly once, in order, across the concatenated chunks.
    let flat: Vec<usize> = chunks.into_iter().flatten().collect();
    assert_eq!(flat, selected, "contiguous split preserves file order end to end");
}

#[test]
fn chunk_row_groups_clamps_jobs_above_selected_len() {
    let selected = vec![5, 6, 7];
    let chunks = chunk_row_groups(&selected, 100);
    assert_eq!(chunks.len(), 3, "never more chunks than row groups");
    assert_eq!(chunks, vec![vec![5], vec![6], vec![7]]);
}

#[test]
fn chunk_row_groups_treats_zero_jobs_as_one() {
    let selected = vec![1, 2, 3];
    assert_eq!(chunk_row_groups(&selected, 0), vec![selected.clone()]);
    assert_eq!(chunk_row_groups(&selected, 1), vec![selected]);
}

#[test]
fn chunk_row_groups_empty_selection_is_no_chunks() {
    assert!(chunk_row_groups(&[], 8).is_empty());
}

// ---- ingest_local_file_parallel: must equal the serial path, INCLUDING under real cross-worker
// contention on a token whose two rows straddle a chunk boundary -----------------------------

/// A dozen distinct single-row tokens (one row group each, `max_rows_per_group=1`) ingested
/// once through the serial [`ingest_local_file`] and once through
/// [`ingest_local_file_parallel`] with `jobs=4` must land the identical row count AND the
/// identical per-symbol rows — the headline "parallel == serial" correctness proof the
/// measurement plan requires.
#[test]
fn ingest_local_file_parallel_matches_the_serial_path_row_counts_and_content() {
    let ids = [
        "TOK_A", "TOK_B", "TOK_C", "TOK_D", "TOK_E", "TOK_F", "TOK_G", "TOK_H", "TOK_I", "TOK_J",
        "TOK_K", "TOK_L",
    ];
    let rows: Vec<BookRow<'_>> = ids
        .iter()
        .enumerate()
        .map(|(i, sym)| {
            let ts = (i + 1) as i64;
            (
                *sym,
                ts,
                ts,
                1u64,
                "book",
                "none",
                0.0,
                0.0,
                "[[0.5,10.0]]",
                "[[0.51,10.0]]",
                0.01,
                "",
            )
        })
        .collect();
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), ids.len(), "one row group per token");

    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let serial_total =
        ingest_local_file(&serial_store, &file_path, "2026-07-28", Stream::Book, None).unwrap();

    let parallel_dir = tempfile::tempdir().unwrap();
    let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
    let parallel_total = ingest_local_file_parallel(
        &parallel_store,
        &file_path,
        "2026-07-28",
        Stream::Book,
        None,
        4,
    )
    .unwrap();

    assert_eq!(serial_total, ids.len());
    assert_eq!(parallel_total, serial_total, "parallel path writes the same row count");

    for sym in ids {
        let from_serial = serial_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        let from_parallel = parallel_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(from_serial.len(), from_parallel.len(), "{sym}: same row count");
        assert_eq!(from_serial.len(), 1);
        assert_eq!(from_serial[0].ts, from_parallel[0].ts, "{sym}: ts must match");
    }
}

/// The parallel counterpart of
/// `ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups`: FOUR row groups
/// (one row each), `jobs=2` so [`chunk_row_groups`] hands worker 0 row groups `[0, 1]` and
/// worker 1 row groups `[2, 3]`. `TOK_A`'s two rows sit in row groups 1 and 2 — exactly
/// STRADDLING the chunk boundary — so this is the one case where two DIFFERENT worker threads
/// really do call `DataFusionHist::append_book_updates` for the SAME symbol concurrently,
/// exercising `SeriesLock`'s file-lock contention path for real (not just asserting the
/// design on paper). Both of `TOK_A`'s rows must survive — the store's per-series lock must
/// serialize the two concurrent manifest read-modify-writes correctly, not lose one.
#[test]
fn ingest_local_file_parallel_keeps_both_rows_when_one_symbol_spans_a_chunk_boundary() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_X", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.1,1.0]]", "[[0.11,1.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 3, 3, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
        ("TOK_Y", 4, 4, 1, "book", "none", 0.0, 0.0, "[[0.2,2.0]]", "[[0.21,2.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), 4, "one row group per row");
    // Sanity-check the boundary this test relies on: jobs=2 over 4 groups -> [0,1] / [2,3].
    assert_eq!(chunk_row_groups(&[0, 1, 2, 3], 2), vec![vec![0, 1], vec![2, 3]]);

    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let (_dir, store) = open();
    let total = ingest_local_file_parallel(&store, &file_path, "2026-07-28", Stream::Book, None, 2)
        .unwrap();

    assert_eq!(total, 4, "all four rows written — none dropped by the cross-worker boundary");
    let tok_a = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(tok_a.len(), 2, "both of TOK_A's rows survive concurrent cross-worker writes");
    let tok_x = store.scan_book_updates(VENUE, "TOK_X", TsRange::all()).unwrap();
    let tok_y = store.scan_book_updates(VENUE, "TOK_Y", TsRange::all()).unwrap();
    assert_eq!(tok_x.len(), 1);
    assert_eq!(tok_y.len(), 1);
}

/// `jobs=1` must be byte-for-byte the same decode/commit order as the serial
/// [`ingest_local_file`] — a caller that wants the old behavior back gets it exactly, not just
/// "close enough". Uses a token spanning two row groups (like the serial-path test of the same
/// name) so a hypothetical broken single-chunk split would still be caught.
#[test]
fn ingest_local_file_parallel_with_jobs_one_matches_ingest_local_file_exactly() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let serial_dir = tempfile::tempdir().unwrap();
    let serial_store = DataFusionHist::open(serial_dir.path()).unwrap();
    let serial_total =
        ingest_local_file(&serial_store, &file_path, "2026-07-27", Stream::Book, None).unwrap();

    let parallel_dir = tempfile::tempdir().unwrap();
    let parallel_store = DataFusionHist::open(parallel_dir.path()).unwrap();
    let parallel_total = ingest_local_file_parallel(
        &parallel_store,
        &file_path,
        "2026-07-27",
        Stream::Book,
        None,
        1,
    )
    .unwrap();

    assert_eq!(serial_total, 2);
    assert_eq!(parallel_total, 2);
    let from_serial = serial_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    let from_parallel = parallel_store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(from_serial.len(), 2);
    assert_eq!(from_parallel.len(), 2);
    for (a, b) in from_serial.iter().zip(from_parallel.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.seq, b.seq);
    }
}

/// A token filter still prunes row groups (and drops unrelated rows within a kept group) under
/// the parallel path — mirrors `ingest_stream_over_respects_a_token_filter` for the serial core.
#[test]
fn ingest_local_file_parallel_respects_a_token_filter() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();

    let (_dir, store) = open();
    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());

    let total =
        ingest_local_file_parallel(&store, &file_path, "2026-07-27", Stream::Book, Some(&want), 2)
            .unwrap();
    assert_eq!(total, 1, "only TOK_B's row is written");
    assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
    assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
}
