//! WAL crash-recovery, whole-store maintenance (`run_maintenance`) and the scheduler.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use datafusion::arrow::array::{ArrayRef, Int64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::ipc::writer::StreamWriter;
use datafusion::arrow::record_batch::RecordBatch;
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::StoreKindId;
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_store::{
    CompactionConfig, HistoryStoreDataFusion, MaintenanceConfig, MaintenanceScheduler,
    RetentionPolicy,
};

use crate::common::{DAY, bar, bars_series, count_parquets};

// ---- slice 6: WAL crash-recovery for in-flight appends -------------------------------------

#[test]
fn wal_recovers_unpublished_append() {
    // Simulate a crash in the seal->publish window: the store seals the parquet part + fsyncs the
    // WAL record but never publishes the manifest (the test-only crash-injection hook). The append is
    // therefore invisible to reads on that (crashed) store. A FRESH open() must replay the WAL and
    // surface the rows, bit-for-bit — the proof that an accepted-but-unpublished append is not lost.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..7)
        .map(|i| {
            bar(i * 60_000, 100.0 + i as f64, if i % 2 == 0 { Some(0.01 * i as f64) } else { None })
        })
        .collect();
    {
        let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true); // crash right after the WAL fsync, before manifest publish
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap();
        // the unpublished append is invisible on the crashed store (manifest never advanced)
        assert!(
            df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().is_empty(),
            "pre-recovery: an unpublished append is invisible"
        );
        // drop df == process crash (no clean shutdown, no manifest publish)
    }
    // reopen the SAME dir → open() runs WAL recovery
    let df2 = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got); // rows recovered, every field bit-identical
    // the recovered commit key is now durable → re-appending it is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().len(), 7);

    // a second reopen recovers nothing (the WAL was cleared once applied)
    drop(df2);
    let df3 = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().len(), 7);
}

#[test]
fn wal_replay_is_idempotent() {
    // Two WAL records carrying the SAME commit key (an in-flight batch logged, then retried, both
    // before any publish) must recover to ONE copy of the rows — the commit-key guard makes replay
    // idempotent — and a normal re-append of that key afterwards is still a no-op.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..5).map(|i| bar(i * 60_000, 50.0 + i as f64, None)).collect();
    {
        let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true);
        // same key logged twice with no publish in between → two WAL records, key not yet committed
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
    }
    // recovery replays the first record and skips the second (key already committed) → 5 rows, not 10
    let df2 = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap();
    assert_eq!(got.len(), 5, "duplicate WAL records for one key recover exactly once");
    assert_bars_bit_eq(&bars, &got);

    // recovery cleared the WAL + published the key → a normal re-append is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().len(), 5);

    // and re-running recovery (reopen) is a no-op — no re-duplication
    drop(df2);
    let df3 = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().len(), 5);
}

#[test]
fn a_temp_file_left_by_a_kill_does_not_hide_the_wal() {
    // A kill inside the WAL rewrite leaves the whole `_wal.arrow` beside a partial temp file: the
    // next open must replay the former and ignore the latter.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..3).map(|i| bar(i * 60_000, 100.0 + i as f64, None)).collect();
    {
        let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true);
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap();
    }
    let leaf = bars_series_of(dir.path(), "BTCUSDT");
    std::fs::write(leaf.join("_wal.arrow.1.0.tmp"), b"VWAL torn").unwrap();

    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert_bars_bit_eq(
        &bars,
        &df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap(),
    );
}

/// Whole WAL frames (the v1 shape) that a reader cannot use, each named: the zeros a power cut
/// leaves where it kept a frame's header and length but not its payload pages, and an Arrow stream
/// that decodes to rows with no `ts`.
fn bad_wal_frames() -> [(&'static str, Vec<u8>); 2] {
    let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
    let col: ArrayRef = Arc::new(Int64Array::from(vec![7]));
    let no_ts = RecordBatch::try_new(schema.clone(), vec![col]).unwrap();
    let mut ipc = Vec::new();
    {
        let mut w = StreamWriter::try_new(&mut ipc, &schema).unwrap();
        w.write(&no_ts).unwrap();
        w.finish().unwrap();
    }
    [("zeros", vec![0u8; 64]), ("no ts", ipc)].map(|(what, payload)| {
        let mut frame = b"VWAL".to_vec();
        frame.extend_from_slice(&3u32.to_le_bytes());
        frame.extend_from_slice(b"bad");
        frame.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        frame.extend_from_slice(&payload);
        (what, frame)
    })
}

/// The WALs set aside beside `series`'s `_wal.arrow`.
fn set_aside_wals(series: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(series)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("_wal.arrow."))
        .collect()
}

#[test]
fn a_wal_record_that_does_not_decode_sets_that_series_wal_aside_and_the_store_opens() {
    // A power cut that kept a WAL frame's header and length but not its payload pages (they read
    // back as zeros), or a record whose rows carry no `ts`, used to fail `open` for the WHOLE store.
    // Now that series' WAL goes aside whole, the intact record in front of the bad one is KEPT
    // (replayed), and the other series recovers.
    for (what, bad) in bad_wal_frames() {
        let dir = tempfile::tempdir().unwrap();
        let kept: Vec<Bar> = (0..3).map(|i| bar(i * 60_000, 100.0 + i as f64, None)).collect();
        let other: Vec<Bar> = (0..2).map(|i| bar(i * 60_000, 200.0 + i as f64, None)).collect();
        {
            let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
            df.set_skip_publish_for_test(true);
            df.append_bars("binance", "BTCUSDT", "1m", &kept, Some("before-the-bad-one")).unwrap();
            df.append_bars("binance", "ETHUSDT", "1m", &other, Some("other-series")).unwrap();
        }
        let damaged = bars_series_of(dir.path(), "BTCUSDT");
        let mut wal = std::fs::read(damaged.join("_wal.arrow")).unwrap();
        wal.extend_from_slice(&bad);
        std::fs::write(damaged.join("_wal.arrow"), &wal).unwrap();

        for reopen in 0..2 {
            let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
            assert_bars_bit_eq(
                &kept,
                &df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap(),
            );
            assert_bars_bit_eq(
                &other,
                &df.load_bars("binance", "ETHUSDT", "1m", TimestampRange::all()).unwrap(),
            );
            let at = format!("{what}, open {reopen}");
            assert!(!damaged.join("_wal.arrow").exists(), "{at}: nothing left to replay");
            let aside = set_aside_wals(&damaged);
            assert_eq!(aside.len(), 1, "{at}: one set-aside WAL, kept: {aside:?}");
            assert_eq!(std::fs::read(&aside[0]).unwrap(), wal, "{at}: set aside whole");
            assert!(!bars_series_of(dir.path(), "ETHUSDT").join("_wal.arrow").exists());
        }
    }
}

#[test]
fn a_live_write_that_meets_a_bad_wal_record_sets_that_series_wal_aside_and_succeeds() {
    // A live write rewrites its series' WAL after its publish, and that rewrite failed on a record
    // that does not decode: that write and every later one on the series failed until the next
    // open. Now the write sets the WAL aside and succeeds, the unapplied record in front of the bad
    // one waits for the next open, and no other series is touched.
    let bars = |from: i64, n: i64| -> Vec<Bar> {
        (from..from + n).map(|i| bar(i * 60_000, 100.0 + i as f64, None)).collect()
    };
    for (what, bad) in bad_wal_frames() {
        let dir = tempfile::tempdir().unwrap();
        let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true);
        df.append_bars("binance", "BTCUSDT", "1m", &bars(0, 2), Some("unapplied")).unwrap();
        df.append_bars("binance", "ETHUSDT", "1m", &bars(0, 2), Some("other-series")).unwrap();
        df.set_skip_publish_for_test(false);
        let damaged = bars_series_of(dir.path(), "BTCUSDT");
        let other = bars_series_of(dir.path(), "ETHUSDT");
        let other_wal = std::fs::read(other.join("_wal.arrow")).unwrap();
        let mut wal = std::fs::read(damaged.join("_wal.arrow")).unwrap();
        wal.extend_from_slice(&bad);
        std::fs::write(damaged.join("_wal.arrow"), &wal).unwrap();

        let written = df.append_bars("binance", "BTCUSDT", "1m", &bars(2, 3), Some("live-1"));
        assert_eq!(written.unwrap(), 3, "{what}: the write meeting the bad record");
        let aside = set_aside_wals(&damaged);
        assert_eq!(aside.len(), 1, "{what}: one set-aside WAL, kept: {aside:?}");
        assert!(std::fs::read(&aside[0]).unwrap().starts_with(&wal), "{what}: set aside whole");
        let written = df.append_bars("binance", "BTCUSDT", "1m", &bars(5, 1), Some("live-2"));
        assert_eq!(written.unwrap(), 1, "{what}: the next write");
        assert_eq!(set_aside_wals(&damaged).len(), 1, "{what}: the next write meets no bad record");
        assert_eq!(std::fs::read(other.join("_wal.arrow")).unwrap(), other_wal, "{what}");
        assert!(set_aside_wals(&other).is_empty(), "{what}: the other series is untouched");
        drop(df);

        // The next open replays the two unapplied records, in front of the bad one and elsewhere.
        let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
        assert_bars_bit_eq(
            &bars(0, 6),
            &df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap(),
        );
        assert_bars_bit_eq(
            &bars(0, 2),
            &df.load_bars("binance", "ETHUSDT", "1m", TimestampRange::all()).unwrap(),
        );
        assert!(!damaged.join("_wal.arrow").exists(), "{what}: nothing left to replay");
        assert_eq!(set_aside_wals(&damaged).len(), 1, "{what}: the set-aside WAL stays");
    }
}

// ---- slice 7: whole-store maintenance (list_series + run_maintenance + scheduler) -----------

/// The bar series leaf dir for an arbitrary `symbol` (parts live under `date=…` one level deeper).
fn bars_series_of(root: &Path, symbol: &str) -> PathBuf {
    root.join("kind=bar").join("venue=binance").join(format!("symbol={symbol}")).join("interval=1m")
}

#[test]
fn run_maintenance_compacts_and_prunes_all_series() {
    // Two bar series, each seeded with an OLD date (day 0) of 2 fragments — below the compaction
    // threshold, so retention prunes them — plus a NEW date (day 10) of 4 fragments — above the
    // threshold, so compaction merges them and retention keeps them. ONE run_maintenance pass must
    // compact every eligible series AND prune the old date, leaving the surviving rows bit-identical.
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();

    // day `day`, fragment `c`: 5 ts-distinct bars within that UTC day (fragments never overlap in ts).
    let frag = |day: i64, c: i64| -> Vec<Bar> {
        let base = day * DAY + c * 5 * 1000;
        (0..5).map(move |i| bar(base + i * 1000, 100.0 + (c * 5 + i) as f64, None)).collect()
    };

    for symbol in ["BTCUSDT", "ETHUSDT"] {
        for c in 0..2i64 {
            // OLD date (day 0): 2 fragments — below min_parts=3, later pruned
            df.append_bars(
                "binance",
                symbol,
                "1m",
                &frag(0, c),
                Some(&format!("{symbol}-old-{c}")),
            )
            .unwrap();
        }
        for c in 0..4i64 {
            // NEW date (day 10): 4 fragments — above min_parts=3, merged then kept
            df.append_bars(
                "binance",
                symbol,
                "1m",
                &frag(10, c),
                Some(&format!("{symbol}-new-{c}")),
            )
            .unwrap();
        }
        assert_eq!(
            count_parquets(&bars_series_of(dir.path(), symbol)),
            6,
            "6 fragments before {symbol}"
        );
    }

    let config = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: Some(RetentionPolicy { before_timestamp: Some(5 * DAY), max_age_ms: None }),
        retention_by_kind: Default::default(),
    };
    let report = df.run_maintenance(&config).unwrap();

    // both series visited; compaction totals = 2 series × (4 fragments merged → 1 sealed, 20 rows)
    assert_eq!(report.series_visited(), 2);
    assert_eq!(report.compaction.parts_merged, 8);
    assert_eq!(report.compaction.parts_written, 2);
    assert_eq!(report.compaction.rows, 40);
    // retention totals = 2 series × (2 old fragments dropped, 1 date dir dropped, 10 rows)
    assert_eq!(report.retention.files_dropped, 4);
    assert_eq!(report.retention.dates_dropped, 2);
    assert_eq!(report.retention.rows_dropped, 20);

    // per-series: EVERY eligible series had its day-10 fragments merged AND its old date pruned.
    for sm in &report.series {
        assert_eq!(sm.compaction.parts_merged, 4, "{:?} merged its 4 day-10 frags", sm.series);
        assert_eq!(sm.compaction.parts_written, 1, "{:?} sealed one part", sm.series);
        assert_eq!(sm.retention.dates_dropped, 1, "{:?} old date pruned", sm.series);
    }

    // filesystem + read-back: each series is now ONE merged part holding exactly the day-10 rows.
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let series = bars_series_of(dir.path(), symbol);
        assert_eq!(count_parquets(&series), 1, "one merged part remains for {symbol}");
        assert!(!series.join("date=1970-01-01").exists(), "old date dir unlinked for {symbol}");
        let want: Vec<Bar> = (0..4).flat_map(|c| frag(10, c)).collect();
        let got = df.load_bars("binance", symbol, "1m", TimestampRange::all()).unwrap();
        assert_bars_bit_eq(&want, &got);
    }
}

// ---- per-kind retention (`MaintenanceConfig::retention_by_kind`) ------------------------------

/// A store with ONE series of each of three kinds (bar, trade, quote) on the same venue and symbol,
/// each holding an OLD part (day 0) and a NEW one (day 10), one row apiece.
fn one_row_old_and_new_in_three_kinds(root: &Path) -> HistoryStoreDataFusion {
    let df = HistoryStoreDataFusion::open(root).unwrap();
    for day in [0, 10] {
        let ts = day * DAY + 1_000;
        let key = |kind: &str| format!("{kind}-day{day}");
        df.append_bars("binance", "BTCUSDT", "1m", &[bar(ts, 100.0, None)], Some(&key("bar")))
            .unwrap();
        let trade = TickTrade {
            timestamp: ts,
            local_timestamp: 0,
            price: 10.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        };
        df.append_trades("binance", "BTCUSDT", &[trade], Some(&key("trade"))).unwrap();
        let quote = TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        };
        df.append_quotes("binance", "BTCUSDT", &[quote], Some(&key("quote"))).unwrap();
    }
    df
}

/// The rows left of each of the three kinds, as `(bars, trades, quotes)`.
fn rows_left_of_three_kinds(df: &HistoryStoreDataFusion) -> (usize, usize, usize) {
    (
        df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap().len(),
        df.scan_trades("binance", "BTCUSDT", TimestampRange::all()).unwrap().len(),
        df.scan_quotes("binance", "BTCUSDT", TimestampRange::all()).unwrap().len(),
    )
}

/// Drop parts whose newest row is before `ts` (an explicit cutoff, so the test is deterministic).
fn before(ts: i64) -> RetentionPolicy {
    RetentionPolicy { before_timestamp: Some(ts), max_age_ms: None }
}

/// The rows a pass dropped from the series of `kind`.
fn rows_dropped_of(report: &vike_store::MaintenanceReport, kind: StoreKindId) -> usize {
    report.series.iter().filter(|s| s.series.kind == kind).map(|s| s.retention.rows_dropped).sum()
}

/// With NO scalar, a kind in the map is pruned and every other kind is left alone: the map is what
/// switches retention on for it.
#[test]
fn a_kind_entry_prunes_that_kind_alone_when_there_is_no_scalar() {
    let dir = tempfile::tempdir().unwrap();
    let df = one_row_old_and_new_in_three_kinds(dir.path());
    assert_eq!(rows_left_of_three_kinds(&df), (2, 2, 2));

    let mut config = MaintenanceConfig::default();
    config.retention_by_kind.insert((StoreKindId::Trade, None), before(5 * DAY));
    let report = df.run_maintenance(&config).unwrap();

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(rows_left_of_three_kinds(&df), (2, 1, 2), "only the trade series lost its old part");
    assert_eq!(rows_dropped_of(&report, StoreKindId::Trade), 1);
    assert_eq!(rows_dropped_of(&report, StoreKindId::Bar), 0);
    assert_eq!(rows_dropped_of(&report, StoreKindId::Quote), 0);
    assert_eq!(report.retention.rows_dropped, 1);
}

/// A kind in the map uses its OWN policy instead of the scalar, longer as well as shorter: the
/// scalar prunes the bars and quotes, and the trades keep their old part under a looser cutoff.
#[test]
fn a_kind_entry_can_keep_longer_than_the_scalar() {
    let dir = tempfile::tempdir().unwrap();
    let df = one_row_old_and_new_in_three_kinds(dir.path());

    let mut config = MaintenanceConfig { retention: Some(before(5 * DAY)), ..Default::default() };
    config.retention_by_kind.insert((StoreKindId::Trade, None), before(0));
    let report = df.run_maintenance(&config).unwrap();

    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(rows_left_of_three_kinds(&df), (1, 2, 1), "the trade series kept its old part");
    assert_eq!(rows_dropped_of(&report, StoreKindId::Trade), 0);
    assert_eq!(rows_dropped_of(&report, StoreKindId::Bar), 1);
    assert_eq!(rows_dropped_of(&report, StoreKindId::Quote), 1);
}

/// A kind in the map can also prune SOONER than the scalar: the scalar's cutoff keeps the day-0 and
/// day-10 parts, and the trade entry's later cutoff drops day 0 and keeps day 10.
#[test]
fn a_kind_entry_can_keep_shorter_than_the_scalar() {
    let dir = tempfile::tempdir().unwrap();
    let df = one_row_old_and_new_in_three_kinds(dir.path());

    let mut config = MaintenanceConfig { retention: Some(before(0)), ..Default::default() };
    config.retention_by_kind.insert((StoreKindId::Trade, None), before(5 * DAY));
    df.run_maintenance(&config).unwrap();

    assert_eq!(rows_left_of_three_kinds(&df), (2, 1, 2), "only the trade series was cut");
}

/// An EMPTY map is the scalar for every kind: the pass is byte-identical to naming each kind with
/// the scalar's own policy, and to the single-policy pass this config had before the map existed.
#[test]
fn an_empty_kind_map_is_the_scalar_for_every_kind() {
    let scalar = before(5 * DAY);

    let dir_a = tempfile::tempdir().unwrap();
    let a = one_row_old_and_new_in_three_kinds(dir_a.path());
    let report_a = a
        .run_maintenance(&MaintenanceConfig { retention: Some(scalar), ..Default::default() })
        .unwrap();

    let dir_b = tempfile::tempdir().unwrap();
    let b = one_row_old_and_new_in_three_kinds(dir_b.path());
    let mut named = MaintenanceConfig { retention: Some(scalar), ..Default::default() };
    for kind in [StoreKindId::Bar, StoreKindId::Trade, StoreKindId::Quote] {
        named.retention_by_kind.insert((kind, None), scalar);
    }
    let report_b = b.run_maintenance(&named).unwrap();

    assert_eq!(rows_left_of_three_kinds(&a), (1, 1, 1));
    assert_eq!(rows_left_of_three_kinds(&a), rows_left_of_three_kinds(&b));
    assert_eq!(report_a.series, report_b.series);
    assert_eq!(report_a.retention, report_b.retention);
    assert_eq!(report_a.retention.rows_dropped, 3);
}

/// No scalar and no map prunes nothing, whatever the kind: the default pass is compact-only.
#[test]
fn no_scalar_and_no_kind_map_prunes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let df = one_row_old_and_new_in_three_kinds(dir.path());
    let report = df.run_maintenance(&MaintenanceConfig::default()).unwrap();
    assert_eq!(rows_left_of_three_kinds(&df), (2, 2, 2));
    assert_eq!(report.retention.rows_dropped, 0);
}

#[test]
fn list_series_roundtrips_the_tree() {
    // A handful of series across kinds/venues/symbols/intervals (including tick series, interval=None)
    // must enumerate back EXACTLY — with (kind, venue, symbol, interval) parsed straight from the path.
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();

    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0, None)).collect();
    let trades: Vec<TickTrade> = (0..4)
        .map(|i| TickTrade {
            timestamp: i * 1000,
            local_timestamp: 0,
            price: 10.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: Default::default(),
        })
        .collect();
    let quotes: Vec<TickQuote> = (0..4)
        .map(|i| TickQuote {
            timestamp: i * 1000,
            local_timestamp: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: Default::default(),
        })
        .collect();

    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    df.append_bars("binance", "BTCUSDT", "5m", &bars, None).unwrap(); // same symbol, other interval
    df.append_bars("okx", "ETHUSDT", "1m", &bars, None).unwrap(); // other venue + symbol
    df.append_trades("binance", "BTCUSDT", &trades, None).unwrap(); // tick series → interval None
    df.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();

    let sid = |kind: StoreKindId, venue: &str, symbol: &str, interval: Option<&str>| SeriesId {
        kind,
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.map(str::to_string),
        group: None,
        source: None,
    };
    let mut want = vec![
        sid(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m")),
        sid(StoreKindId::Bar, "binance", "BTCUSDT", Some("5m")),
        sid(StoreKindId::Bar, "okx", "ETHUSDT", Some("1m")),
        sid(StoreKindId::Trade, "binance", "BTCUSDT", None),
        sid(StoreKindId::Quote, "binance", "BTCUSDT", None),
    ];
    want.sort();

    let got = df.list_series().unwrap(); // list_series returns sorted
    assert_eq!(got, want, "every series enumerates back with the right identity");
}

/// A series folder whose manifest can be stat-ed but whose contents cannot be listed FAILS `list_series`, as it did
/// before the walk stopped listing series folders (`series_walk.rs`'s `find_manifest_series_dirs`): a store that
/// exists and cannot be read is an error, never a quieter answer.
#[cfg(unix)]
#[test]
fn a_series_folder_that_cannot_be_listed_fails_list_series() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 100.0, None)], None).unwrap();
    let leaf = bars_series(dir.path());
    let mode =
        |m: u32| std::fs::set_permissions(&leaf, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o300); // search and write, no read: its manifest can be stat-ed, the folder cannot be listed
    let privileged = std::fs::read_dir(&leaf).is_ok();
    let answer = df.list_series();
    mode(0o755);
    if privileged {
        eprintln!("SKIPPED: this user (root) lists any folder, so nothing is proved");
        return;
    }
    assert!(answer.is_err(), "an unlistable series folder must fail the listing: {answer:?}");
}

/// What one `list_series` costs on a tree shaped like the CI box's store (2026-11-19: 2,020 leaves, ~110 `date=` dirs
/// each), the walk a backtest of a USDT symbol on the dollar ledger waits for (`series_walk.rs`'s
/// `find_manifest_series_dirs`), against the same ask answered by the held walk once the tree is older than its
/// settling time (`series_walk.rs`'s `SeriesWalkMemo`: one stat per listed folder and per leaf's manifest).
/// `#[ignore]`d: it makes ~220,000 directories. Run:
/// `cargo test -p vike-store --release --test store list_series_walk_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn list_series_walk_cost() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let (leaves, days) = (2_000, 110);
    for s in 0..leaves {
        let leaf = dir.path().join(format!("kind=bar/venue=v{}/symbol=S{s}/interval=1m", s % 20));
        for d in 0..days {
            std::fs::create_dir_all(leaf.join(format!("date=d{d:03}"))).unwrap();
        }
        std::fs::write(leaf.join("_manifest.json"), b"{}").unwrap();
    }
    // A leaf with no manifest yet (mid-creation) is still walked into and lists nothing.
    std::fs::create_dir_all(dir.path().join("kind=bar/venue=v0/symbol=NEW/interval=1m/date=d000"))
        .unwrap();
    let best = || {
        (0..5)
            .map(|_| {
                let t = Instant::now();
                assert_eq!(df.list_series().unwrap().len(), leaves);
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    // Every folder was written just now, so no walk is kept: each ask walks.
    let walked = best();
    println!("list_series over {leaves} leaves x {days} date dirs: best of 5 = {walked:?}");
    // Every folder above a leaf an hour old: the next ask walks and keeps its answer, each one after checks the key.
    let hour_ago = std::time::SystemTime::now() - Duration::from_secs(3_600);
    let mut stack = vec![dir.path().to_path_buf()];
    while let Some(folder) = stack.pop() {
        if folder.join("_manifest.json").exists() {
            continue;
        }
        stack.extend(
            std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()),
        );
        std::fs::File::open(&folder).unwrap().set_modified(hour_ago).unwrap();
    }
    assert_eq!(df.list_series().unwrap().len(), leaves);
    let held = best();
    println!(
        "list_series held, its key checked: best of 5 = {held:?}, {:.1}x cheaper than the walk",
        walked.as_secs_f64() / held.as_secs_f64()
    );
}

#[test]
fn scheduler_runs_then_stops_clean() {
    // Seed a store that WILL compact (4 same-day fragments), start the scheduler on a short interval,
    // poll (bounded) until it reports a compaction, then stop() — which must join cleanly — and assert
    // the store is consistent: merged rows bit-identical + fewer part files. Deterministic: the store
    // is fully seeded BEFORE start, so the first pass compacts; the poll is a monotonic counter with a
    // generous 10s ceiling (no fixed sleeps racing the worker).
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    for c in 0..4i64 {
        let b: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("s{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    assert_eq!(count_parquets(&series), 4, "4 fragments seeded");
    let expected = df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap();

    // No retention → the scheduler only compacts; every row survives (a clean consistency check).
    let config = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: None,
        retention_by_kind: Default::default(),
    };
    let mut sched = MaintenanceScheduler::start(df.clone(), config, Duration::from_millis(5));

    // bounded wait: poll the monotonic compaction counter until the first merge lands (or time out).
    let start = Instant::now();
    while sched.parts_compacted() == 0 {
        assert!(start.elapsed() < Duration::from_secs(10), "scheduler did not compact within 10s");
        std::thread::sleep(Duration::from_millis(5));
    }

    sched.stop(); // set the flag, wake the sleep, join the thread — deterministic teardown
    sched.stop(); // idempotent: a second stop() (and the eventual Drop) is a harmless no-op

    assert!(sched.passes_completed() >= 1, "at least one full pass completed");
    assert!(count_parquets(&series) < 4, "compaction reduced the part count");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TimestampRange::all()).unwrap();
    assert_bars_bit_eq(&expected, &got); // 20 rows preserved bit-for-bit through compaction
}
