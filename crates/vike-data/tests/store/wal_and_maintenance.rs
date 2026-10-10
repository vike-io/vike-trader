//! WAL crash-recovery, whole-store maintenance (`run_maintenance`) and the scheduler.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_data::{
    CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, MaintenanceScheduler,
    RetentionPolicy, SeriesId, TsRange,
};
use vike_model::test_support::bars::assert_bars_bit_eq;
use vike_model::{Bar, QuoteTick, TradeTick};

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
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true); // crash right after the WAL fsync, before manifest publish
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap();
        // the unpublished append is invisible on the crashed store (manifest never advanced)
        assert!(
            df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty(),
            "pre-recovery: an unpublished append is invisible"
        );
        // drop df == process crash (no clean shutdown, no manifest publish)
    }
    // reopen the SAME dir → open() runs WAL recovery
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got); // rows recovered, every field bit-identical
    // the recovered commit key is now durable → re-appending it is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("crash-batch")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 7);

    // a second reopen recovers nothing (the WAL was cleared once applied)
    drop(df2);
    let df3 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 7);
}

#[test]
fn wal_replay_is_idempotent() {
    // Two WAL records carrying the SAME commit key (an in-flight batch logged, then retried, both
    // before any publish) must recover to ONE copy of the rows — the commit-key guard makes replay
    // idempotent — and a normal re-append of that key afterwards is still a no-op.
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..5).map(|i| bar(i * 60_000, 50.0 + i as f64, None)).collect();
    {
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.set_skip_publish_for_test(true);
        // same key logged twice with no publish in between → two WAL records, key not yet committed
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap();
    }
    // recovery replays the first record and skips the second (key already committed) → 5 rows, not 10
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 5, "duplicate WAL records for one key recover exactly once");
    assert_bars_bit_eq(&bars, &got);

    // recovery cleared the WAL + published the key → a normal re-append is the usual idempotent no-op
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("dup-key")).unwrap(), 0);
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 5);

    // and re-running recovery (reopen) is a no-op — no re-duplication
    drop(df2);
    let df3 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 5);
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
    let df = DataFusionHist::open(dir.path()).unwrap();

    // day `day`, fragment `c`: 5 ts-distinct bars within that UTC day (fragments never overlap in ts).
    let frag = |day: i64, c: i64| -> Vec<Bar> {
        let base = day * DAY + c * 5 * 1000;
        (0..5).map(move |i| bar(base + i * 1000, 100.0 + (c * 5 + i) as f64, None)).collect()
    };

    for sym in ["BTCUSDT", "ETHUSDT"] {
        for c in 0..2i64 {
            // OLD date (day 0): 2 fragments — below min_parts=3, later pruned
            df.append_bars("binance", sym, "1m", &frag(0, c), Some(&format!("{sym}-old-{c}")))
                .unwrap();
        }
        for c in 0..4i64 {
            // NEW date (day 10): 4 fragments — above min_parts=3, merged then kept
            df.append_bars("binance", sym, "1m", &frag(10, c), Some(&format!("{sym}-new-{c}")))
                .unwrap();
        }
        assert_eq!(count_parquets(&bars_series_of(dir.path(), sym)), 6, "6 fragments before {sym}");
    }

    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: Some(RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None }),
    };
    let report = df.run_maintenance(&cfg).unwrap();

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
    for sym in ["BTCUSDT", "ETHUSDT"] {
        let series = bars_series_of(dir.path(), sym);
        assert_eq!(count_parquets(&series), 1, "one merged part remains for {sym}");
        assert!(!series.join("date=1970-01-01").exists(), "old date dir unlinked for {sym}");
        let want: Vec<Bar> = (0..4).flat_map(|c| frag(10, c)).collect();
        let got = df.load_bars("binance", sym, "1m", TsRange::all()).unwrap();
        assert_bars_bit_eq(&want, &got);
    }
}

#[test]
fn list_series_roundtrips_the_tree() {
    // A handful of series across kinds/venues/symbols/intervals (including tick series, interval=None)
    // must enumerate back EXACTLY — with (kind, venue, symbol, interval) parsed straight from the path.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    let bars: Vec<Bar> = (0..4).map(|i| bar(i * 60_000, 100.0, None)).collect();
    let trades: Vec<TradeTick> = (0..4)
        .map(|i| TradeTick {
            ts: i * 1000,
            local_ts: 0,
            price: 10.0,
            size: 1.0,
            is_buyer_maker: false,
            symbol: String::new(),
        })
        .collect();
    let quotes: Vec<QuoteTick> = (0..4)
        .map(|i| QuoteTick {
            ts: i * 1000,
            local_ts: 0,
            bid: 1.0,
            ask: 2.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        })
        .collect();

    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    df.append_bars("binance", "BTCUSDT", "5m", &bars, None).unwrap(); // same symbol, other interval
    df.append_bars("okx", "ETHUSDT", "1m", &bars, None).unwrap(); // other venue + symbol
    df.append_trades("binance", "BTCUSDT", &trades, None).unwrap(); // tick series → interval None
    df.append_quotes("binance", "BTCUSDT", &quotes, None).unwrap();

    let sid = |kind: &str, venue: &str, symbol: &str, interval: Option<&str>| SeriesId {
        kind: kind.into(),
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.map(str::to_string),
        group: None,
        source: None,
    };
    let mut want = vec![
        sid("bar", "binance", "BTCUSDT", Some("1m")),
        sid("bar", "binance", "BTCUSDT", Some("5m")),
        sid("bar", "okx", "ETHUSDT", Some("1m")),
        sid("trade", "binance", "BTCUSDT", None),
        sid("quote", "binance", "BTCUSDT", None),
    ];
    want.sort();

    let got = df.list_series().unwrap(); // list_series returns sorted
    assert_eq!(got, want, "every series enumerates back with the right identity");
}

#[test]
fn scheduler_runs_then_stops_clean() {
    // Seed a store that WILL compact (4 same-day fragments), start the scheduler on a short interval,
    // poll (bounded) until it reports a compaction, then stop() — which must join cleanly — and assert
    // the store is consistent: merged rows bit-identical + fewer part files. Deterministic: the store
    // is fully seeded BEFORE start, so the first pass compacts; the poll is a monotonic counter with a
    // generous 10s ceiling (no fixed sleeps racing the worker).
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    for c in 0..4i64 {
        let b: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("s{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    assert_eq!(count_parquets(&series), 4, "4 fragments seeded");
    let expected = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    // No retention → the scheduler only compacts; every row survives (a clean consistency check).
    let cfg = MaintenanceConfig {
        compaction: CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() },
        retention: None,
    };
    let mut sched = MaintenanceScheduler::start(df.clone(), cfg, Duration::from_millis(5));

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
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&expected, &got); // 20 rows preserved bit-for-bit through compaction
}
