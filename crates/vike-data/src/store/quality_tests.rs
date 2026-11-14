use super::*;
use crate::test_support::{book_event, status_marker};
use vike_marketdata::{L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};

const STEP: i64 = 1_800_000; // 30 min — the test cadence AND the coverage threshold
const HOUR: i64 = 3_600_000;

fn config() -> DayQualityConfig {
    DayQualityConfig { max_gap_ms: STEP }
}

fn q_at(ts: i64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: ts,
        bid: 0.45,
        ask: 0.46,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: String::new(),
    }
}

fn t_at(ts: i64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: ts,
        price: 0.45,
        size: 1.0,
        is_buyer_maker: false,
        symbol: String::new(),
    }
}

/// A single day (`1970-01-01`) that is otherwise fully covered but has a §B `GapStart`..
/// `LiveResume` outage of exactly one hour (no rows in between), a seq reset at the re-anchor,
/// and therefore a one-hour coverage hole.
fn book_day_with_gap() -> Vec<L2BookUpdate> {
    let mut rows = Vec::new();
    // slots 0..=9: a snapshot then deltas, seq 1..10, at the 30-min cadence
    for k in 0..=9i64 {
        let kind = if k == 0 { L2BookUpdateKind::Snapshot } else { L2BookUpdateKind::Delta };
        rows.push(book_event(k * STEP, (k + 1) as u64, kind));
    }
    // slot 10 (ts 18_000_000): §B GapStart; NO rows until recovery a full hour later
    rows.push(status_marker(10 * STEP, L2BookUpdateKind::GapStart));
    // slot 12 (ts 21_600_000): recovery — LiveResume then a re-anchor Snapshot whose seq RESETS
    rows.push(status_marker(12 * STEP, L2BookUpdateKind::LiveResume));
    rows.push(book_event(12 * STEP + 1, 1, L2BookUpdateKind::Snapshot)); // seq 10 -> 1 regression
    // slots 13..=47: post-gap deltas, seq 2..36, back on cadence to the end of the day
    for k in 13..=47i64 {
        rows.push(book_event(k * STEP, (k - 11) as u64, L2BookUpdateKind::Delta));
    }
    rows
}

#[test]
fn clean_continuous_book_day_scores_perfect() {
    // 48 events at the 30-min cadence spanning the whole UTC day, monotonic seq, no markers.
    let rows: Vec<L2BookUpdate> = (0..48i64)
        .map(|k| {
            let kind = if k == 0 { L2BookUpdateKind::Snapshot } else { L2BookUpdateKind::Delta };
            book_event(k * STEP, (k + 1) as u64, kind)
        })
        .collect();
    let q = score_book_day("polymarket", "TOK", 0, &rows, &config());
    assert_eq!(q.lane, DayQualityLane::Book);
    assert_eq!(q.day, "1970-01-01");
    assert_eq!(q.rows, 48);
    assert_eq!(q.first_timestamp, Some(0));
    assert_eq!(q.last_timestamp, Some(47 * STEP));
    assert_eq!(q.uncovered_ms, 0);
    assert_eq!(q.largest_gap_ms, 0);
    assert_eq!(q.gap_start_count, 0);
    assert_eq!(q.stale_count, 0);
    assert_eq!(q.disclosed_gap_ms, 0);
    assert_eq!(q.sequence_resets, 0);
    assert!(q.is_perfect());
    assert!((q.coverage_fraction - 1.0).abs() < 1e-12);
    assert!(q.gap_percent.abs() < 1e-12);
}

#[test]
fn book_day_folds_gap_seqreset_and_hole() {
    let q = score_book_day("polymarket", "TOK", 0, &book_day_with_gap(), &config());
    // §B disclosure: one GapStart, no Stale, one hour of disclosed outage (18.0M -> 21.6M)
    assert_eq!(q.gap_start_count, 1);
    assert_eq!(q.stale_count, 0);
    assert_eq!(q.disclosed_gap_ms, HOUR);
    // seq chain: 1..10 then a reset to 1 then 2..36 -> exactly one regression
    assert_eq!(q.sequence_resets, 1);
    // inferred coverage: exactly one 1-hour hole, nothing else exceeds the 30-min threshold
    assert_eq!(q.largest_gap_ms, HOUR);
    assert_eq!(q.uncovered_ms, HOUR);
    let expect_gap_percent = HOUR as f64 / MS_PER_DAY as f64 * 100.0;
    assert!((q.gap_percent - expect_gap_percent).abs() < 1e-9);
    let expect_cov = (MS_PER_DAY - HOUR) as f64 / MS_PER_DAY as f64;
    assert!((q.coverage_fraction - expect_cov).abs() < 1e-12);
    assert!(!q.is_perfect());
}

#[test]
fn quote_lane_infers_coverage_from_inter_row_deltas() {
    // clean: 48 half-hourly quotes across the day -> perfect (cadence == threshold)
    let clean: Vec<TickQuote> = (0..48i64).map(|k| q_at(k * STEP)).collect();
    let cq = score_quote_day("polymarket", "TOK", 0, &clean, &config());
    assert_eq!(cq.lane, DayQualityLane::Quote);
    assert_eq!(cq.uncovered_ms, 0);
    assert!(cq.is_perfect());

    // holed: drop the k=10 sample -> a 1-hour hole between k=9 (16.2M) and k=11 (19.8M)
    let holed: Vec<TickQuote> = (0..48i64).filter(|&k| k != 10).map(|k| q_at(k * STEP)).collect();
    let hq = score_quote_day("polymarket", "TOK", 0, &holed, &config());
    assert_eq!(hq.rows, 47);
    assert_eq!(hq.largest_gap_ms, HOUR);
    assert_eq!(hq.uncovered_ms, HOUR);
    assert!((hq.gap_percent - HOUR as f64 / MS_PER_DAY as f64 * 100.0).abs() < 1e-9);
    // §B/seq signals are inert on the quote lane (no provenance recorded there)
    assert_eq!(hq.gap_start_count, 0);
    assert_eq!(hq.stale_count, 0);
    assert_eq!(hq.disclosed_gap_ms, 0);
    assert_eq!(hq.sequence_resets, 0);
    assert!(!hq.is_perfect());
}

#[test]
fn trade_lane_infers_coverage_like_quotes() {
    // drop the k=20 sample -> a 1-hour hole between k=19 (34.2M) and k=21 (37.8M)
    let holed: Vec<TickTrade> = (0..48i64).filter(|&k| k != 20).map(|k| t_at(k * STEP)).collect();
    let hq = score_trade_day("polymarket", "TOK", 0, &holed, &config());
    assert_eq!(hq.lane, DayQualityLane::Trade);
    assert_eq!(hq.uncovered_ms, HOUR);
    assert_eq!(hq.largest_gap_ms, HOUR);
    assert_eq!(hq.sequence_resets, 0);
    assert!(!hq.is_perfect());
}

#[test]
fn empty_day_is_fully_uncovered() {
    let q = score_tick_day("v", "s", DayQualityLane::Quote, 0, &[], &config());
    assert_eq!(q.rows, 0);
    assert_eq!(q.first_timestamp, None);
    assert_eq!(q.last_timestamp, None);
    assert_eq!(q.uncovered_ms, MS_PER_DAY);
    assert_eq!(q.largest_gap_ms, MS_PER_DAY);
    assert!(q.coverage_fraction.abs() < 1e-12);
    assert!((q.gap_percent - 100.0).abs() < 1e-9);
    assert!(!q.is_perfect());
}

#[test]
fn stale_marker_counted_and_unclosed_gap_extends_to_day_end() {
    let rows = vec![
        book_event(1_000, 1, L2BookUpdateKind::Snapshot),
        status_marker(HOUR, L2BookUpdateKind::Stale), // data stopped at 1h and never resumed this day
    ];
    let q = score_book_day("v", "s", 0, &rows, &config());
    assert_eq!(q.stale_count, 1);
    assert_eq!(q.gap_start_count, 0);
    // unclosed outage runs from the Stale onset (HOUR) to the end of the UTC day
    assert_eq!(q.disclosed_gap_ms, MS_PER_DAY - HOUR);
    assert!(!q.is_perfect());
}

#[test]
fn sequence_resets_ignore_status_markers() {
    // markers carry seq 0; they must NOT read as a regression in the data seq chain.
    let across_markers = vec![
        book_event(1_000, 5, L2BookUpdateKind::Delta),
        status_marker(2_000, L2BookUpdateKind::GapStart),
        status_marker(3_000, L2BookUpdateKind::LiveResume),
        book_event(4_000, 6, L2BookUpdateKind::Delta),
    ];
    let q = score_book_day("v", "s", 0, &across_markers, &config());
    assert_eq!(q.sequence_resets, 0); // 5 -> 6 across the markers is monotonic

    // a genuine backwards jump in the data chain IS counted
    let regression = vec![
        book_event(1_000, 5, L2BookUpdateKind::Delta),
        book_event(2_000, 3, L2BookUpdateKind::Delta),
    ];
    let q2 = score_book_day("v", "s", 0, &regression, &config());
    assert_eq!(q2.sequence_resets, 1);
}

#[test]
fn quote_series_buckets_into_per_day_records() {
    let quotes = vec![
        q_at(1_000),
        q_at(2_000), // 1970-01-01
        q_at(MS_PER_DAY + 1_000),
        q_at(MS_PER_DAY + 2_000), // 1970-01-02
    ];
    let out = score_quote_series("polymarket", "TOK", &quotes, &config());
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].day, "1970-01-01");
    assert_eq!(out[1].day, "1970-01-02");
    assert_eq!(out[0].rows, 2);
    assert_eq!(out[1].rows, 2);
    assert_eq!(out[0].lane, DayQualityLane::Quote);
}

#[test]
fn book_series_buckets_and_scores_each_day() {
    let mut rows = book_day_with_gap(); // day 0: hole + reset + disclosed gap
    rows.push(book_event(MS_PER_DAY + 1_000, 1, L2BookUpdateKind::Snapshot)); // day 1: clean-ish
    rows.push(book_event(MS_PER_DAY + 2_000, 2, L2BookUpdateKind::Delta));
    let out = score_book_series("polymarket", "TOK", &rows, &config());
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].day, "1970-01-01");
    assert_eq!(out[0].sequence_resets, 1);
    assert_eq!(out[0].gap_start_count, 1);
    assert_eq!(out[0].disclosed_gap_ms, HOUR);
    assert_eq!(out[1].day, "1970-01-02");
    assert_eq!(out[1].sequence_resets, 0);
    assert_eq!(out[1].gap_start_count, 0);
}

#[test]
fn utc_day_start_floors_to_midnight() {
    assert_eq!(utc_day_start_ms(0), 0);
    assert_eq!(utc_day_start_ms(1), 0);
    assert_eq!(utc_day_start_ms(MS_PER_DAY - 1), 0);
    assert_eq!(utc_day_start_ms(MS_PER_DAY), MS_PER_DAY);
    assert_eq!(utc_day_start_ms(MS_PER_DAY + 5), MS_PER_DAY);
    // pre-epoch floors toward negative infinity
    assert_eq!(utc_day_start_ms(-1), -MS_PER_DAY);
}
