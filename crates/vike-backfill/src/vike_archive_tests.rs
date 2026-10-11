use super::datasets::{DatasetFilter, default_discovery_base};
use super::readers::HttpRangeReader;
use super::test_support::{BookRow, open, write_book_events_parquet};
use super::*;
use crate::vike_archive::client::ArchiveClient;
use bytes::Bytes;
use datafusion::arrow::array::{Array, ArrayRef, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::parquet::file::reader::Length;
use std::sync::Arc;
use vike_data::TsRange;

// ---- row-group pruning: build a REAL multi-row-group Parquet file, decode metadata back ------

/// Writes `token_ids` (one row per id, one row group per row via `set_max_row_group_size(1)`)
/// as a minimal single-column-plus-companions Parquet file entirely in memory, and returns the
/// bytes — the "build a tiny RecordBatch, write to bytes, decode back" fixture the design brief
/// asked for, exercising the REAL Parquet writer's row-group statistics (not hand-built stats).
fn write_single_column_parquet(token_ids: &[&str]) -> Bytes {
    let schema = Arc::new(Schema::new(vec![Field::new("token_id", DataType::Utf8, false)]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![Arc::new(StringArray::from(token_ids.to_vec())) as ArrayRef],
    )
    .unwrap();
    let props = WriterProperties::builder().set_max_row_group_row_count(Some(1)).build();
    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }
    Bytes::from(buf)
}

#[test]
fn select_row_groups_prunes_to_the_matching_groups_only() {
    // Distinct, sorted token ids -> one row group per id (max_row_group_size=1), each group's
    // min==max==that id — an unambiguous test of the byte-range containment check.
    let ids = ["TOK_A", "TOK_B", "TOK_C", "TOK_D"];
    let bytes = write_single_column_parquet(&ids);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let md = builder.metadata();
    assert_eq!(md.num_row_groups(), 4, "one row group per row (max_row_group_size=1)");

    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());
    let selected = select_row_groups(md, Some(&want));
    assert_eq!(selected, vec![1], "only TOK_B's own row group is selected");

    // No filter -> every row group.
    assert_eq!(select_row_groups(md, None), vec![0, 1, 2, 3]);

    // A token absent from the file -> no row group matches (assuming it sorts outside every
    // group's [min,max] — true here since "TOK_Z" > every group's max byte-wise).
    let mut absent = HashSet::new();
    absent.insert("TOK_Z".to_string());
    assert!(select_row_groups(md, Some(&absent)).is_empty());

    // Empty filter set -> nothing selected (never "everything").
    assert!(select_row_groups(md, Some(&HashSet::new())).is_empty());
}

#[test]
fn select_row_groups_reads_end_to_end_through_the_real_reader() {
    // Full round trip: write -> build reader with ONLY the pruned row groups -> decode -> the
    // resulting rows are exactly (and only) the requested token's.
    let ids = ["TOK_A", "TOK_B", "TOK_C"];
    let bytes = write_single_column_parquet(&ids);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
    let mut want = HashSet::new();
    want.insert("TOK_C".to_string());
    let selected = select_row_groups(builder.metadata(), Some(&want));
    let reader = builder.with_row_groups(selected).build().unwrap();
    let mut seen = Vec::new();
    for batch in reader {
        let batch = batch.unwrap();
        let col = batch.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..batch.num_rows() {
            seen.push(col.value(i).to_string());
        }
    }
    assert_eq!(seen, vec!["TOK_C".to_string()]);
}

// ---- streaming ingest: memory-bounded per-row-group flush (the defect this PR fixes) --------

/// The headline proof for this PR: [`ingest_stream_over`] flushes ONE row group at a time — not
/// after draining the whole file. Three distinct tokens, one row group each
/// (`max_rows_per_group=1`): the progress callback must fire exactly once per row group, in
/// order, with a running cumulative count — and, load-bearing, EACH row group's data must
/// already be durable in the store by the time its own callback fires (checked with a real
/// `HistStore::scan_book_updates` from INSIDE the callback) — proving the write happens before
/// the next row group is ever opened, not batched at the end. This is the structural substitute
/// for an in-process RSS assertion (impractical here); the actual peak-RSS measurement lives in
/// the PR's live the CI box proof, not in this unit test.
#[test]
fn ingest_stream_over_flushes_each_row_group_before_moving_to_the_next() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 10, 10, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 20, 20, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
        ("TOK_C", 30, 30, 1, "book", "none", 0.0, 0.0, "[[0.3,30.0]]", "[[0.31,30.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(probe.metadata().num_row_groups(), 3, "one row group per row (limit=1)");

    let (_dir, store) = open();
    let expected_symbol = ["TOK_A", "TOK_B", "TOK_C"];

    let mut seen = Vec::new();
    let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |p| {
        // At the moment THIS callback runs, the row group just processed must already be on
        // disk — scanning here (not after the whole run) is what makes this an incremental,
        // not an end-of-run, assertion.
        let sym = expected_symbol[p.index];
        let scanned = store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(
            scanned.len(),
            1,
            "row group {} ({sym})'s row must already be durable when its own callback fires",
            p.index
        );
        seen.push((p.index, p.total, p.rows_this_group, p.rows_cumulative));
    })
    .unwrap();

    assert_eq!(total, 3);
    assert_eq!(
        seen,
        vec![(0, 3, 1, 1), (1, 3, 1, 2), (2, 3, 1, 3)],
        "one callback per row group, in order, with a running cumulative total"
    );
}

/// The correctness proof for the per-row-group commit-key redesign: TWO rows for the SAME
/// token, forced into TWO separate row groups. A single date-wide commit key (the pre-fix
/// scheme) would make the SECOND row's write a silent no-op — `HistStore`'s `commit_rows`
/// treats a repeated commit key as "already committed" and returns `Ok(0)` without writing
/// anything (see `vike-data`'s `datafusion_hist.rs`) — so a naive per-row-group split with the
/// OLD shared key would have silently dropped every row after a symbol's first row group. This
/// asserts both rows survive.
#[test]
fn ingest_stream_over_keeps_both_rows_when_one_symbol_spans_two_row_groups() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "book", "none", 0.0, 0.0, "[[0.6,11.0]]", "[[0.61,11.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let probe = ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).unwrap();
    assert_eq!(
        probe.metadata().num_row_groups(),
        2,
        "max_rows_per_group=1 over 2 rows -> 2 groups"
    );

    let (_dir, store) = open();
    let mut calls = 0usize;
    let total = ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, None, |_p| {
        calls += 1;
    })
    .unwrap();

    assert_eq!(calls, 2, "one callback per row group");
    assert_eq!(total, 2, "both rows written — neither silently dropped by a shared commit key");
    let scanned = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(
        scanned.len(),
        2,
        "both row groups' rows for TOK_A must be present, not just the first"
    );
}

/// A token filter still prunes row groups (and drops unrelated rows within a kept group) when
/// ingesting through the new streaming path — the filtering behavior itself is unchanged,
/// only WHEN writes happen changed.
#[test]
fn ingest_stream_over_respects_a_token_filter() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_B", 2, 2, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);
    let (_dir, store) = open();
    let mut want = HashSet::new();
    want.insert("TOK_B".to_string());

    let total =
        ingest_stream_over(bytes, &store, "2026-07-27", Stream::Book, Some(&want), |_| {}).unwrap();
    assert_eq!(total, 1, "only TOK_B's row is written");
    assert!(store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap().is_empty());
    assert_eq!(store.scan_book_updates(VENUE, "TOK_B", TsRange::all()).unwrap().len(), 1);
}

// ---- live network smokes (never run in CI; manual only) --------------------------------------

/// Proves ranged reads actually work end-to-end against the REAL archive: opens the free,
/// keyless daily sample (`/archive/samples/book_events.parquet`, no `X-API-Key` needed) with
/// [`HttpRangeReader`], reads the footer-only metadata, and decodes ONE row group. Always
/// `#[ignore]`d — needs network, which unit tests must not require — run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_range_read_against_free_sample -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_range_read_against_free_sample() {
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let url = "https://data.vike.io/archive/samples/book_events.parquet".to_string();
    let reader = HttpRangeReader::open(agent, url, None).expect("HEAD the free sample");
    assert!(reader.len() > 0);
    let builder = ParquetRecordBatchReaderBuilder::try_new(reader).expect("footer-only open");
    assert!(builder.metadata().num_row_groups() >= 1);
    let mut reader = builder.build().unwrap();
    let batch = reader.next().expect("at least one batch").unwrap();
    let decoded = book_updates_from_batch(&batch, RowScope::All).expect("decode the real schema");
    println!("live sample: decoded {} book updates from one batch", decoded.len());
}

/// Proves the manifest + a keyed per-date footer-only plan against the REAL archive.
/// Double-gated (network + `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent — the
/// same idiom the venue demo smokes use. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_manifest_and_plan -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_manifest_and_plan() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live archive smoke");
        return;
    };
    let client = ArchiveClient::new(DEFAULT_BASE, Some(key));
    let manifest = client.fetch_manifest().expect("fetch the real manifest");
    assert!(!manifest.dates.is_empty());
    let date = &manifest.dates[0].date;
    let mut want = HashSet::new();
    // An arbitrary sample token id seen in the free sample fixture — a real archive run would
    // narrow to a token the operator actually cares about.
    want.insert(
        "7715333804644496306161804929604508138595580668391178416151948416889614694288".to_string(),
    );
    let plan = client.plan_stream(date, Stream::Book, Some(&want)).expect("plan a real date");
    println!(
        "live plan {date}/book_events: {}/{} row groups, {} / {} compressed bytes",
        plan.selected_row_groups,
        plan.total_row_groups,
        plan.selected_compressed_bytes,
        plan.total_compressed_bytes
    );
}

/// Proves the real `/v1/archive/datasets` + `/v1/archive/url` discovery routes end-to-end
/// against the live production host, and that the URL they hand back is directly openable by
/// [`HttpRangeReader`] (never re-derived by this client). Double-gated (network +
/// `VIKE_ARCHIVE_API_KEY`): self-skips when the key is absent. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib vike_archive::tests::live_discover_and_resolve_btc_5m -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_discover_and_resolve_btc_5m() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live discovery smoke");
        return;
    };
    let client = ArchiveClient::new(DEFAULT_BASE, Some(key.clone()));
    let discovery_base = default_discovery_base(DEFAULT_BASE);

    let filter = DatasetFilter {
        asset: Some("btc".to_string()),
        tenor: Some("5m".to_string()),
        ..Default::default()
    };
    let page = client.discover_datasets(&discovery_base, &filter).expect("discover btc/5m");
    assert!(!page.datasets.is_empty(), "expected at least one btc/5m dataset row");
    assert!(page.datasets.iter().all(|d| d.layout == "family"));
    let date = page.datasets[0].date.clone();

    let resolved = client
        .resolve_dataset(&discovery_base, &date, Stream::Book, Some("btc"), Some("5m"))
        .expect("resolve one btc/5m dataset");
    assert_eq!(resolved.asset.as_deref(), Some("btc"));
    assert_eq!(resolved.tenor.as_deref(), Some("5m"));

    // The resolved URL must be directly openable — prove it with a real ranged HEAD/GET,
    // exactly like `live_range_read_against_free_sample` does for the flat/keyless sample.
    let agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
    let reader = HttpRangeReader::open(agent, resolved.url.clone(), Some(key))
        .expect("open the discovered family URL");
    assert!(reader.len() > 0);
    println!(
        "live discovery: {date}/book_events resolved to {} ({} bytes, layout={})",
        resolved.url, resolved.bytes, resolved.layout
    );
}
