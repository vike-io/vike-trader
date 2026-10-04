//! `HistStore::load_bars_head` on the real DataFusion backend: the override must answer a COMPLETE
//! PREFIX of `load_bars` holding AT LEAST `n` rows unless it is the whole range — and must answer it
//! without reading the rest of the range.
//!
//! # Why this file exists
//!
//! The data daemon's `LoadBars` verb answered a capped request by loading the client's whole range
//! and cutting the reply afterwards, so a 10,000-row page of a series holding tens of millions of
//! bars cost the series (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`).
//! `HistStore::load_bars_head` is the read that lets it stop early, and this file holds the
//! backend's answer to the two things that make it safe to answer from:
//!
//! 1. **It meets the contract.** [`assert_head_contract`] judges the override against the store's
//!    own `load_bars` for the same arguments, over a grid of `n` and of ranges on a series built to
//!    be awkward: several UTC days, a re-fetched part overlapping one of them, a timestamp stored
//!    twice inside one part and several across two, and ranges that start in the MIDDLE of a part.
//!    That last shape is the one the weaker `_capped` contract fails — a block selected by its
//!    parts' total rows can hold a handful of rows of the range while the range goes on — so a
//!    head that stopped after one block passes every tidy range and fails there. The grid runs
//!    again after compaction, when one part holds a timestamp more than once.
//! 2. **It is NOT a read of the whole range.** [`a_head_never_opens_a_part_past_the_one_that_completed_it`]
//!    replaces the LAST day's part with garbage: `load_bars` over the range now fails, and a head
//!    that the earlier days can fill still answers. Rows alone cannot tell a bounded read from a
//!    load that truncates afterwards — they return the same rows — but a store that cannot be
//!    loaded whole can. [`a_head_of_nothing_reads_nothing`] is the same proof for `n == 0`.
//!
//! Only compiled/run with `--features hist-datafusion`, like `tests/bar_edges.rs`.
#![cfg(feature = "hist-datafusion")]

use std::path::{Path, PathBuf};

use vike_data::{CompactionConfig, DataFusionHist, HistStore, TsRange};
use vike_model::Bar;

const VENUE: &str = "oanda";
const SYMBOL: &str = "HEAD_USD";
const INTERVAL: &str = "1m";
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close - 1.0,
        high: close + 1.0,
        low: close - 2.0,
        close,
        volume: 7.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn open() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    (dir, store)
}

/// Rows of each day's own part: one bar every two hours.
const PER_DAY: i64 = 12;

/// The awkward series, as parts:
///
/// * days 0..=3, one part each, `PER_DAY` bars every two hours (`close` = hour + 100 · day);
/// * day 1 RE-FETCHED over 06:00..=16:00 under a second key — a second part of the same date whose
///   span sits INSIDE the first one's, every one of its six timestamps stored a second time with a
///   different `close`, so the ORDER of a timestamp's rows is observable;
/// * day 2's own batch carrying 04:00 twice, so one part holds a timestamp twice from the start.
///
/// 55 rows in all: 48 + 6 + 1.
fn plant(store: &DataFusionHist) -> usize {
    for day in 0..4i64 {
        let mut bars: Vec<Bar> = (0..PER_DAY)
            .map(|k| bar(day * DAY_MS + 2 * k * HOUR_MS, (2 * k + 100 * day) as f64))
            .collect();
        if day == 2 {
            bars.push(bar(2 * DAY_MS + 4 * HOUR_MS, 999.0));
        }
        store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some(&format!("day{day}"))).unwrap();
    }
    let refetch: Vec<Bar> =
        (3..=8i64).map(|k| bar(DAY_MS + 2 * k * HOUR_MS, (2 * k + 100) as f64 + 0.5)).collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &refetch, Some("day1-refetch")).unwrap();
    let all = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();
    assert_eq!(all.len(), 55, "the fixture must hold what its doc says, or the grid proves less");
    all.len()
}

/// The contract, judged against the same store's own `load_bars` for the same arguments. Written out
/// here rather than taken from any production helper, so a defect in the override cannot be
/// repeated in the thing that judges it; `src/test_support_tests.rs` carries the default-build twin,
/// and the two judge the same three things.
fn assert_head_contract(store: &DataFusionHist, range: TsRange, n: usize) -> Vec<Bar> {
    let full = store.load_bars(VENUE, SYMBOL, INTERVAL, range).unwrap();
    let head = store.load_bars_head(VENUE, SYMBOL, INTERVAL, range, n).unwrap();
    judge(&head, &full, range, n);
    head
}

/// [`assert_head_contract`]'s three judgements, against an already-loaded `full`.
fn judge(head: &[Bar], full: &[Bar], range: TsRange, n: usize) {
    let at = format!("range {range:?}, n {n}");
    assert!(head.len() <= full.len(), "{at}: a head cannot hold rows the range does not");
    assert_eq!(head, &full[..head.len()], "{at}: a PREFIX of load_bars, rows and order alike");
    if let (Some(last), Some(next)) = (head.last(), full.get(head.len())) {
        assert!(
            next.ts > last.ts,
            "{at}: COMPLETE — the cut split ts {}, so a caller continuing past it loses rows",
            last.ts
        );
    }
    assert!(
        head.len() >= n || head.len() == full.len(),
        "{at}: AT LEAST n — {} rows of {} while the range goes on",
        head.len(),
        full.len()
    );
}

/// Every range the grid asks about. Several START inside a part, which is the shape that separates
/// a head that keeps reading from one that stops after a single block.
fn ranges() -> Vec<TsRange> {
    let h = HOUR_MS;
    vec![
        TsRange::all(),
        // From day 0's last two bars on: the first block is day 0's whole part, which holds two rows
        // of the range — short of every n above two while the range goes on.
        TsRange { start: Some(20 * h), end: None },
        // Starts inside day 1's re-fetched window, ends inside day 3.
        TsRange::of(DAY_MS + 9 * h, 3 * DAY_MS + 5 * h),
        // Starts ON the timestamp day 2 stores twice.
        TsRange { start: Some(2 * DAY_MS + 4 * h), end: None },
        // Starts and ends inside one part.
        TsRange::of(DAY_MS + 7 * h, DAY_MS + 15 * h),
        TsRange { start: None, end: Some(2 * DAY_MS + 5 * h) },
        // Overlaps a part and holds none of its rows.
        TsRange::of(DAY_MS + h, DAY_MS + h + h / 2),
        // Only the last part, and past every part.
        TsRange { start: Some(3 * DAY_MS), end: None },
        TsRange::of(5 * DAY_MS, 6 * DAY_MS),
    ]
}

const NS: [usize; 19] =
    [0, 1, 2, 3, 5, 7, 11, 12, 13, 17, 20, 24, 30, 43, 54, 55, 56, 100, usize::MAX];

/// Every `n` over every range, each head judged by [`judge`] against ONE `load_bars` of its range.
///
/// The oracle is read once per RANGE, not once per `(range, n)`: it depends on the range alone, the
/// store is not written between the reads, and [`assert_head_contract`] re-reading it for each of
/// the [`NS`] was half of this grid's DataFusion queries for no extra judgement — every head is
/// still compared with a whole read of its own range, by the same three assertions.
/// `crates/vike-data/tests/research_head.rs`'s `grid` has always been shaped this way.
fn grid(store: &DataFusionHist) {
    for range in ranges() {
        let full = store.load_bars(VENUE, SYMBOL, INTERVAL, range).unwrap();
        for n in NS {
            let head = store.load_bars_head(VENUE, SYMBOL, INTERVAL, range, n).unwrap();
            judge(&head, &full, range, n);
        }
    }
}

#[test]
fn an_unknown_series_has_an_empty_head() {
    let (_dir, store) = open();
    for n in [0, 1, 100] {
        assert!(
            store.load_bars_head(VENUE, SYMBOL, INTERVAL, TsRange::all(), n).unwrap().is_empty()
        );
    }
}

#[test]
fn the_head_meets_the_contract_over_every_range_and_n() {
    let (_dir, store) = open();
    plant(&store);
    grid(&store);
}

/// The shapes the grid covers, pinned in ABSOLUTE numbers too, so an override and an oracle that
/// were wrong the same way would still fail.
#[test]
fn the_head_is_the_shape_the_contract_names() {
    let (_dir, store) = open();
    plant(&store);
    let h = HOUR_MS;

    // A range starting in the middle of day 0's part: the first block holds two rows of it, and the
    // head must go on to day 1 rather than answer two of the five asked for. Day 1's own part and
    // its re-fetch overlap, so they are read together and every one of day 1's 18 rows comes along.
    let head = assert_head_contract(&store, TsRange { start: Some(20 * h), end: None }, 5);
    assert_eq!(head.len(), 2 + 18, "two of day 0, then all of day 1 (one block, two parts)");
    assert_eq!(head.first().map(|b| b.ts), Some(20 * h));
    assert_eq!(head.last().map(|b| b.ts), Some(DAY_MS + 22 * h));

    // A timestamp stored twice across the two day-1 parts comes back twice, in part order: the
    // day's own part first (it starts earlier), then the re-fetch.
    let at_8h: Vec<f64> = head.iter().filter(|b| b.ts == DAY_MS + 8 * h).map(|b| b.close).collect();
    assert_eq!(
        at_8h,
        vec![108.0, 108.5],
        "both rows of a re-fetched ts, in the whole read's order"
    );

    // A head of one starting ON the twice-stored ts of day 2 holds BOTH of its rows.
    let head =
        assert_head_contract(&store, TsRange { start: Some(2 * DAY_MS + 4 * h), end: None }, 1);
    assert!(head.len() >= 2, "a ts is never split, whatever n asks for: {head:?}");
    assert_eq!(head[0].ts, head[1].ts);

    // An n wider than the range answers the whole range, and says so by being short of n.
    let head = assert_head_contract(&store, TsRange::all(), 100);
    assert_eq!(head.len(), 55);

    // n == 0 asks for nothing and gets nothing.
    assert!(assert_head_contract(&store, TsRange::all(), 0).is_empty());
}

/// Maintenance merges day 1's two parts into one, which then holds six timestamps twice — the case
/// a row-cutting `LIMIT` could split and this read must not.
#[test]
fn the_contract_survives_compaction() {
    let (_dir, store) = open();
    plant(&store);
    let whole = store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let report = store.compact_series("bar", VENUE, SYMBOL, Some(INTERVAL), &cfg).unwrap();
    assert_eq!(report.parts_merged, 2, "day 1's two parts must merge, or this proves nothing");
    assert_eq!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap(),
        whole,
        "control: compaction keeps every row, duplicates and their order included"
    );

    grid(&store);
}

/// The one observable difference between a head read and a load that truncates afterwards: the
/// head never opens a part past the block that completed its count.
///
/// The LAST day's part is replaced, in place and under the same name, by bytes that are not Parquet
/// at all. A whole-range `load_bars` cannot survive it — the control below proves that — so a head
/// that the first three days can fill must answer without having opened it, and a head that needs
/// the fourth day must fail, which proves the part IS read when the count reaches it.
#[test]
fn a_head_never_opens_a_part_past_the_one_that_completed_it() {
    let (dir, store) = open();
    plant(&store);
    // Snapshot every judged range BEFORE the part is spoiled: the oracle has to stay readable.
    let fulls: Vec<(TsRange, Vec<Bar>)> = ranges()
        .into_iter()
        .map(|r| (r, store.load_bars(VENUE, SYMBOL, INTERVAL, r).unwrap()))
        .collect();

    spoil(&last_day_part(dir.path()));

    assert!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).is_err(),
        "control: the spoiled part must defeat a whole-range read, or nothing below proves a bound"
    );
    // Days 0..=2 hold 12 + 18 + 13 = 43 rows: any n up to that is answered from them alone.
    let full = &fulls[0].1;
    for n in [1, 5, 12, 13, 30, 31, 43] {
        let head = store
            .load_bars_head(VENUE, SYMBOL, INTERVAL, TsRange::all(), n)
            .unwrap_or_else(|e| panic!("n {n}: a head the first three days fill must answer: {e}"));
        judge(&head, full, TsRange::all(), n);
        assert!(head.iter().all(|b| b.ts < 3 * DAY_MS), "n {n}: no row of the spoiled day");
    }
    // ...and starting in the middle of day 0, which takes more than one block to fill.
    let mid = TsRange { start: Some(20 * HOUR_MS), end: None };
    let mid_full = &fulls[1].1;
    assert_eq!(fulls[1].0, mid);
    let head = store.load_bars_head(VENUE, SYMBOL, INTERVAL, mid, 25).unwrap();
    judge(&head, mid_full, mid, 25);

    assert!(
        store.load_bars_head(VENUE, SYMBOL, INTERVAL, TsRange::all(), 44).is_err(),
        "control: a head that NEEDS the fourth day must read it — the bound is the count, not a \
         fixed set of parts"
    );
}

/// `n == 0` answers empty WITHOUT reading: the FIRST part is spoiled, and a head of nothing still
/// answers, while a head of one fails on it.
#[test]
fn a_head_of_nothing_reads_nothing() {
    let (dir, store) = open();
    plant(&store);
    spoil(&day_parts(dir.path(), "date=1970-01-01").pop().unwrap());
    assert!(
        store.load_bars_head(VENUE, SYMBOL, INTERVAL, TsRange::all(), 1).is_err(),
        "control: a head of one must read the spoiled first part"
    );
    assert!(
        store.load_bars_head(VENUE, SYMBOL, INTERVAL, TsRange::all(), 0).unwrap().is_empty(),
        "a zero budget is NO budget to the block selector, so n == 0 must stop before it"
    );
}

fn series_dir(root: &Path) -> PathBuf {
    root.join("kind=bar")
        .join(format!("venue={VENUE}"))
        .join(format!("symbol={SYMBOL}"))
        .join(format!("interval={INTERVAL}"))
}

/// The sealed `*.parquet` parts under one `date=` directory of the series.
fn day_parts(root: &Path, date_dir: &str) -> Vec<PathBuf> {
    let day = series_dir(root).join(date_dir);
    let mut found: Vec<PathBuf> = std::fs::read_dir(&day)
        .unwrap_or_else(|e| panic!("{}: {e}", day.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
        .collect();
    found.sort();
    found
}

/// The one part of the LAST day the fixture writes — day 3 is appended once, so it is one part.
fn last_day_part(root: &Path) -> PathBuf {
    let mut parts = day_parts(root, "date=1970-01-04");
    assert_eq!(parts.len(), 1, "one append on day 3 seals one part: {parts:?}");
    parts.pop().unwrap()
}

/// Overwrite a part with bytes no Parquet reader accepts.
fn spoil(path: &Path) {
    std::fs::write(path, b"this is not a parquet file").unwrap();
}
