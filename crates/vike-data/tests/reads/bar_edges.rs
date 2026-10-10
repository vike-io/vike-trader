//! `HistStore::bar_edges` on the real DataFusion backend: the projection-only override must answer
//! EXACTLY what the edges of a `load_bars` would say — and must answer it without decoding rows.
//!
//! # Why this file exists
//!
//! The data daemon's `Backfill` verb used to read its whole requested range back with `load_bars`
//! to report two timestamps, which for a multi-year window of a fine interval is gigabytes inside a
//! process that shares one memory cap with the market-data and recorder planes.
//! `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` asks `bar_edges` now, and this file holds
//! the backend's answer to the two things that make that swap safe:
//!
//! 1. **It is the SAME answer.** Every test below compares the override with the edges derived, by
//!    hand and in the test, from the store's own `load_bars` for the same arguments
//!    ([`assert_agrees`]) — the empty range, one bar, many bars over several parts and days,
//!    inclusive edges, one-sided ranges, duplicate timestamps, a range wider than the data, a window
//!    that overlaps a part without holding one of its rows, and a compacted series. Several also pin
//!    the ABSOLUTE numbers, so an override and an oracle that were wrong the same way would still
//!    fail.
//! 2. **It is NOT a decode.** [`an_edges_read_decodes_the_ts_column_and_no_other`] plants a part
//!    whose price columns cannot be decoded and shows `load_bars` fail on it while `bar_edges`
//!    answers. Values alone cannot tell a bounded implementation from one that quietly delegates to
//!    `load_bars`, and delegation is exactly the defect being removed; a part that only a
//!    projection can read is the one observable difference between them.
//!
//! Only compiled/run with `--features hist-datafusion`, like `tests/hist_datafusion.rs`.
#![cfg(feature = "hist-datafusion")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, Int64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;

use vike_data::{BarEdges, CompactionConfig, DataFusionHist, HistStore, TsRange};
use vike_model::Bar;

use crate::common::open;

const VENUE: &str = "binance";
const SYMBOL: &str = "EDGESUSDT";
const INTERVAL: &str = "1m";
const MIN_MS: i64 = 60_000;
const DAY_MS: i64 = 86_400_000;

fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 100.0,
        high: 101.0,
        low: 99.0,
        close: 100.5,
        volume: 7.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The oracle: the edges of what the store's own `load_bars` returns for `range`. Written out here
/// rather than taken from any production helper, so a defect in the trait default or in the override
/// cannot be repeated in the thing that judges it.
fn derived(store: &DataFusionHist, range: TsRange) -> BarEdges {
    let bars = store.load_bars(VENUE, SYMBOL, INTERVAL, range).unwrap();
    BarEdges {
        first_ts: bars.first().map(|b| b.ts),
        last_ts: bars.last().map(|b| b.ts),
        rows: bars.len() as u64,
    }
}

/// Ask the override, hold it equal to the oracle, and hand the answer back so a test can also pin
/// the absolute numbers.
fn assert_agrees(store: &DataFusionHist, range: TsRange, label: &str) -> BarEdges {
    let got = store.bar_edges(VENUE, SYMBOL, INTERVAL, range).unwrap();
    assert_eq!(got, derived(store, range), "{label}: bar_edges must equal load_bars' own edges");
    got
}

#[test]
fn an_unknown_series_has_no_edges() {
    let (_dir, store) = open();
    for range in [TsRange::all(), TsRange::of(0, 10 * MIN_MS)] {
        assert_eq!(
            assert_agrees(&store, range, "unknown series"),
            BarEdges::default(),
            "a series nobody wrote is an honest empty, not an error"
        );
    }
}

#[test]
fn one_bar_is_both_edges_and_the_range_is_inclusive_at_both_ends() {
    let (_dir, store) = open();
    let t = 5 * MIN_MS;
    store.append_bars(VENUE, SYMBOL, INTERVAL, &[bar(t)], None).unwrap();

    let one = BarEdges { first_ts: Some(t), last_ts: Some(t), rows: 1 };
    assert_eq!(assert_agrees(&store, TsRange::all(), "all"), one);
    assert_eq!(
        assert_agrees(&store, TsRange::of(t, t), "exactly the bar"),
        one,
        "`[t, t]` includes the bar: the range is inclusive at BOTH ends"
    );
    assert_eq!(
        assert_agrees(&store, TsRange::of(t + 1, t + 10), "just after"),
        BarEdges::default()
    );
    assert_eq!(assert_agrees(&store, TsRange::of(0, t - 1), "just before"), BarEdges::default());
}

/// Three UTC days of one bar every ten minutes, appended one day at a time (so three parts, one
/// `date=` each), then asked about from every side.
#[test]
fn many_bars_over_several_parts_and_days_agree_with_load_bars() {
    let (_dir, store) = open();
    let step = 10 * MIN_MS;
    for day in 0..3i64 {
        let bars: Vec<Bar> = (0..144).map(|k| bar(day * DAY_MS + k * step)).collect();
        store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some(&format!("day{day}"))).unwrap();
    }
    let first = 0;
    let last = 2 * DAY_MS + 143 * step;

    let all = BarEdges { first_ts: Some(first), last_ts: Some(last), rows: 3 * 144 };
    assert_eq!(assert_agrees(&store, TsRange::all(), "all"), all);
    assert_eq!(
        assert_agrees(&store, TsRange::of(-DAY_MS, 10 * DAY_MS), "a range wider than the data"),
        all,
        "the ends are the DATA's, not the range's"
    );

    // The window straddles the UTC-day boundary: two parts overlap it and BOTH are cut at the row
    // level. Bars at DAY-20m, DAY-10m, DAY, DAY+10m, DAY+20m.
    assert_eq!(
        assert_agrees(
            &store,
            TsRange::of(DAY_MS - 25 * MIN_MS, DAY_MS + 25 * MIN_MS),
            "across a day boundary"
        ),
        BarEdges {
            first_ts: Some(DAY_MS - 20 * MIN_MS),
            last_ts: Some(DAY_MS + 20 * MIN_MS),
            rows: 5
        },
    );

    // Both edges exactly ON a bar, and on the bar sitting exactly on the day boundary.
    assert_eq!(
        assert_agrees(&store, TsRange::of(first, first), "the first bar alone"),
        BarEdges { first_ts: Some(first), last_ts: Some(first), rows: 1 }
    );
    assert_eq!(
        assert_agrees(&store, TsRange::of(last, last), "the last bar alone"),
        BarEdges { first_ts: Some(last), last_ts: Some(last), rows: 1 }
    );
    assert_eq!(
        assert_agrees(&store, TsRange::of(DAY_MS, DAY_MS), "the bar on the day boundary"),
        BarEdges { first_ts: Some(DAY_MS), last_ts: Some(DAY_MS), rows: 1 }
    );

    // A window strictly inside one part, cut at the row level on both sides.
    assert_eq!(
        assert_agrees(&store, TsRange::of(15 * MIN_MS, 45 * MIN_MS), "interior of one part"),
        BarEdges { first_ts: Some(20 * MIN_MS), last_ts: Some(40 * MIN_MS), rows: 3 }
    );

    // One-sided ranges: an open end is open.
    assert_eq!(
        assert_agrees(
            &store,
            TsRange { start: Some(2 * DAY_MS + 140 * step), end: None },
            "start only"
        ),
        BarEdges { first_ts: Some(2 * DAY_MS + 140 * step), last_ts: Some(last), rows: 4 }
    );
    assert_eq!(
        assert_agrees(&store, TsRange { start: None, end: Some(2 * step) }, "end only"),
        BarEdges { first_ts: Some(first), last_ts: Some(2 * step), rows: 3 }
    );

    // A window that OVERLAPS a part and holds none of its rows: the manifest cannot rule the part
    // out, the row filter empties it, and the aggregate over zero rows is NULL/0 — which must fold
    // to "nothing", never to a row at ts 0.
    assert_eq!(
        assert_agrees(
            &store,
            TsRange::of(DAY_MS + step + 1, DAY_MS + 2 * step - 1),
            "between two bars"
        ),
        BarEdges::default()
    );
    // ...and the same for a window beyond every part, which the manifest alone rules out.
    assert_eq!(
        assert_agrees(&store, TsRange::of(last + 1, last + DAY_MS), "after the data"),
        BarEdges::default()
    );
}

/// A timestamp stored more than once — inside one batch and across two — is more than one ROW, as
/// `load_bars` returns it, and moves neither edge. (`HistStore`'s ingest contract dedups by commit
/// key and never by row value, so these are legitimate stored duplicates.)
#[test]
fn duplicate_timestamps_count_as_rows_and_do_not_move_the_edges() {
    let (_dir, store) = open();
    store
        .append_bars(
            VENUE,
            SYMBOL,
            INTERVAL,
            &[bar(MIN_MS), bar(MIN_MS), bar(2 * MIN_MS)],
            Some("a"),
        )
        .unwrap();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &[bar(MIN_MS), bar(3 * MIN_MS)], Some("b")).unwrap();

    assert_eq!(
        assert_agrees(&store, TsRange::all(), "all"),
        BarEdges { first_ts: Some(MIN_MS), last_ts: Some(3 * MIN_MS), rows: 5 },
        "five rows, three distinct timestamps"
    );
    assert_eq!(
        assert_agrees(&store, TsRange::of(MIN_MS, MIN_MS), "the triplicated ts"),
        BarEdges { first_ts: Some(MIN_MS), last_ts: Some(MIN_MS), rows: 3 },
        "one timestamp, three rows"
    );
}

/// Two parts of ONE series whose `ts` spans interleave — a re-fetched window under a new commit key,
/// an out-of-order backfill — so the extremes of a window are NOT in the extreme parts. Only folding
/// every overlapping part is right here; opening "the first and the last part" would answer the
/// window below wrongly and every tidy single-day layout rightly.
#[test]
fn overlapping_parts_are_all_folded_not_assumed_disjoint() {
    let (_dir, store) = open();
    // Part A spans 10..=90 minutes, part B 20..=100, and they share no timestamp. Same UTC day.
    store
        .append_bars(
            VENUE,
            SYMBOL,
            INTERVAL,
            &[bar(10 * MIN_MS), bar(50 * MIN_MS), bar(90 * MIN_MS)],
            Some("part-a"),
        )
        .unwrap();
    store
        .append_bars(
            VENUE,
            SYMBOL,
            INTERVAL,
            &[bar(20 * MIN_MS), bar(60 * MIN_MS), bar(100 * MIN_MS)],
            Some("part-b"),
        )
        .unwrap();

    assert_eq!(
        assert_agrees(&store, TsRange::all(), "all"),
        BarEdges { first_ts: Some(10 * MIN_MS), last_ts: Some(100 * MIN_MS), rows: 6 }
    );
    // Inside [55, 95]: A holds only 90, B holds only 60. The smallest is B's and the largest is A's,
    // and neither part is the "first" nor the "last" one by its own bounds.
    assert_eq!(
        assert_agrees(&store, TsRange::of(55 * MIN_MS, 95 * MIN_MS), "the interleaved window"),
        BarEdges { first_ts: Some(60 * MIN_MS), last_ts: Some(90 * MIN_MS), rows: 2 }
    );
}

/// Maintenance compacts a series' small parts into one; the answer must not depend on how many
/// parts the same rows happen to sit in.
#[test]
fn compaction_does_not_change_the_answer() {
    let (_dir, store) = open();
    for c in 0..4i64 {
        let batch: Vec<Bar> = (0..5).map(|i| bar((c * 5 + i) * MIN_MS)).collect();
        store.append_bars(VENUE, SYMBOL, INTERVAL, &batch, Some(&format!("frag{c}"))).unwrap();
    }
    let windows = [
        TsRange::all(),
        TsRange::of(3 * MIN_MS, 12 * MIN_MS),
        TsRange::of(50 * MIN_MS, 60 * MIN_MS),
    ];
    let before: Vec<BarEdges> =
        windows.iter().map(|w| assert_agrees(&store, *w, "before compaction")).collect();
    assert_eq!(before[0], BarEdges { first_ts: Some(0), last_ts: Some(19 * MIN_MS), rows: 20 });

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let report = store.compact_series("bar", VENUE, SYMBOL, Some(INTERVAL), &cfg).unwrap();
    assert_eq!(report.parts_merged, 4, "the fixture must really compact, or this proves nothing");

    let after: Vec<BarEdges> =
        windows.iter().map(|w| assert_agrees(&store, *w, "after compaction")).collect();
    assert_eq!(before, after, "compaction rewrites the parts and must not move the edges");
}

/// The one observable difference between a bounded edges read and a load that merely returns fewer
/// fields: `bar_edges` decodes the `ts` column and NO other.
///
/// The part `append_bars` sealed is replaced, in place and under the same name, by a file holding
/// only `ts` (the same values the manifest already records) and a `close` column of TEXT. A full
/// decode cannot survive it — the control below proves that — so the only way `bar_edges` can still
/// answer is by never decoding the rest. It is also the declared limit of the proof
/// `HistStore::bar_edges` documents: a read of the ends does not vouch for the other columns.
#[test]
fn an_edges_read_decodes_the_ts_column_and_no_other() {
    let (dir, store) = open();
    let ts: Vec<i64> = (0..10).map(|i| i * MIN_MS).collect();
    let bars: Vec<Bar> = ts.iter().map(|t| bar(*t)).collect();
    store.append_bars(VENUE, SYMBOL, INTERVAL, &bars, None).unwrap();
    assert_eq!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap().len(),
        10,
        "control: the part decodes before it is replaced"
    );

    plant_undecodable_part(&only_part_file(dir.path()), &ts);

    assert!(
        store.load_bars(VENUE, SYMBOL, INTERVAL, TsRange::all()).is_err(),
        "control: the planted part must defeat a full decode, or the assertions below prove nothing"
    );
    assert_eq!(
        store.bar_edges(VENUE, SYMBOL, INTERVAL, TsRange::all()).unwrap(),
        BarEdges { first_ts: Some(0), last_ts: Some(9 * MIN_MS), rows: 10 },
        "the edges come from the ts column alone"
    );
    assert_eq!(
        store.bar_edges(VENUE, SYMBOL, INTERVAL, TsRange::of(3 * MIN_MS, 5 * MIN_MS)).unwrap(),
        BarEdges { first_ts: Some(3 * MIN_MS), last_ts: Some(5 * MIN_MS), rows: 3 },
        "and the exact row filter still applies to it"
    );
}

/// The single sealed `*.parquet` part of the one series these tests write — one append on one UTC
/// day is one part.
fn only_part_file(root: &Path) -> PathBuf {
    let series = root
        .join("kind=bar")
        .join(format!("venue={VENUE}"))
        .join(format!("symbol={SYMBOL}"))
        .join(format!("interval={INTERVAL}"));
    let mut found = Vec::new();
    for day in std::fs::read_dir(&series).unwrap() {
        let day = day.unwrap().path();
        if !day.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("date=")) {
            continue;
        }
        for entry in std::fs::read_dir(&day).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
                found.push(path);
            }
        }
    }
    assert_eq!(found.len(), 1, "one append on one UTC day seals one part: {found:?}");
    found.pop().unwrap()
}

/// Overwrite `path` with a Parquet file holding `ts` and a TEXT `close` — no `open`/`high`/`low`/
/// `volume`, and a price column of the wrong type: nothing `BarCodec` can decode.
fn plant_undecodable_part(path: &Path, ts: &[i64]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Int64, false),
        Field::new("close", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(ts.to_vec())) as ArrayRef,
            Arc::new(StringArray::from(vec!["not-a-price"; ts.len()])) as ArrayRef,
        ],
    )
    .unwrap();
    let file = std::fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}
