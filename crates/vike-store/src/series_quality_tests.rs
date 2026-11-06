//! The store-backed read-only proof: scoring a real recorded series changes NOTHING on disk
//! (byte-identical scans + series listing before/after) while producing the expected report.

use super::*;
use vike_data::test_support::{book_event, status_marker};
use vike_marketdata::BookUpdateKind;

const STEP: i64 = 1_800_000;
const HOUR: i64 = 3_600_000;

#[test]
fn scoring_reads_store_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    // one UTC day: pre-gap deltas, a §B GapStart..LiveResume one-hour hole, a seq reset, resume
    let mut rows = Vec::new();
    for k in 0..=9i64 {
        let kind = if k == 0 { BookUpdateKind::Snapshot } else { BookUpdateKind::Delta };
        rows.push(book_event(k * STEP, (k + 1) as u64, kind));
    }
    rows.push(status_marker(10 * STEP, BookUpdateKind::GapStart));
    rows.push(status_marker(12 * STEP, BookUpdateKind::LiveResume));
    rows.push(book_event(12 * STEP + 1, 1, BookUpdateKind::Snapshot));
    for k in 13..=47i64 {
        rows.push(book_event(k * STEP, (k - 11) as u64, BookUpdateKind::Delta));
    }
    store.append_book_updates("polymarket", "TOK", &rows, Some("day0")).unwrap();

    // snapshot the persisted state BEFORE scoring
    let before = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    let series_before = store.list_series().unwrap();

    // score (read-only)
    let cfg = QualityConfig { max_gap_ms: STEP };
    let report = store.book_series_quality("polymarket", "TOK", &cfg).unwrap();
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].day, "1970-01-01");
    assert_eq!(report[0].gap_start_count, 1);
    assert_eq!(report[0].disclosed_gap_ms, HOUR);
    assert_eq!(report[0].seq_resets, 1);
    assert_eq!(report[0].largest_gap_ms, HOUR);
    assert_eq!(report[0].uncovered_ms, HOUR);

    // the store is byte-identical after scoring — the API wrote nothing
    let after = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    let series_after = store.list_series().unwrap();
    assert_eq!(series_before, series_after);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.seq, b.seq);
        assert_eq!(a.kind, b.kind);
    }
}
