//! A bar time stored twice is READ once, the NEWEST write's (`SeriesCodec::ONE_ROW_PER_KEY`), by every
//! read and every compaction — while ticks of one timestamp stay rows, every one kept.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use vike_data::{HistoryStore, TimestampRange};
use vike_model::StoreKindId;
use vike_store::{CompactionConfig, HistoryStoreDataFusion};

use crate::common::{bar, qt, tt};

const MIN: i64 = 60_000;

fn closes(df: &HistoryStoreDataFusion) -> Vec<(i64, f64)> {
    let bars = df.load_bars("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap();
    bars.iter().map(|b| (b.timestamp, b.close)).collect()
}

fn config() -> CompactionConfig {
    CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() }
}

/// **The newer write wins**: across commits, inside one batch (its later row), through the head read and
/// the edges read, and across a compaction, which keeps exactly the rows the reads answered (reporting the
/// rest as dropped) — and a write after the pass still wins over the merged part.
#[test]
fn a_newer_write_of_a_bar_time_wins_every_read_and_a_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let put = |bars: &[(i64, f64)], key: &str| {
        let bars: Vec<_> = bars.iter().map(|&(ts, close)| bar(ts, close, None)).collect();
        df.append_bars("oanda", "EURUSD", "1m", &bars, Some(key)).unwrap()
    };
    put(&[(0, 1.0), (MIN, 1.0), (2 * MIN, 1.0)], "a");
    put(&[(MIN, 2.0)], "b");
    put(&[(2 * MIN, 3.0), (2 * MIN, 4.0)], "c");
    let want = vec![(0, 1.0), (MIN, 2.0), (2 * MIN, 4.0)];
    assert_eq!(closes(&df), want, "the newest write of each time, the later row of a batch");

    let head = df.load_bars_head("oanda", "EURUSD", "1m", TimestampRange::all(), 2).unwrap();
    let head: Vec<(i64, f64)> = head.iter().map(|b| (b.timestamp, b.close)).collect();
    assert_eq!(head, want, "the head reads the three parts as one block, newest of each time");
    let edges = df.bar_edges("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap();
    assert_eq!(
        (edges.first_timestamp, edges.last_timestamp, edges.rows),
        (Some(0), Some(2 * MIN), 3)
    );

    let report =
        df.compact_series(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &config()).unwrap();
    assert_eq!((report.parts_merged, report.rows, report.rows_superseded), (3, 3, 3), "{report:?}");
    assert_eq!(closes(&df), want, "a compaction changes no answer");

    put(&[(MIN, 5.0)], "d");
    assert_eq!(
        closes(&df),
        vec![(0, 1.0), (MIN, 5.0), (2 * MIN, 4.0)],
        "newer than the merged part"
    );
    assert_eq!(df.bar_edges("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap().rows, 3);
}

/// **A bar appended while a compaction merges unlocked stays the newest.** The merge output joins the
/// manifest at publish, after the append; were it simply appended last, the merged (older) bar would read as
/// the newer. The publish puts the late part after it, and a REOPEN — the base plus the delta log replayed —
/// answers the same, so the frame carries that order and not only the publishing process.
#[test]
fn a_bar_appended_while_a_compaction_merges_stays_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(HistoryStoreDataFusion::open(dir.path()).unwrap());
    for i in 0..3i64 {
        df.append_bars("oanda", "EURUSD", "1m", &[bar(i * MIN, 1.0, None)], Some(&format!("a{i}")))
            .unwrap();
    }
    let (parked_tx, parked_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    df.set_pause_in_merge_for_test(parked_tx, resume_rx);
    let c = Arc::clone(&df);
    let compactor = std::thread::spawn(move || {
        c.compact_series(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &config())
    });
    parked_rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the compactor reached its merge window");

    df.append_bars("oanda", "EURUSD", "1m", &[bar(MIN, 9.0, None)], Some("late")).unwrap();
    let _ = resume_tx.send(());
    let report = compactor.join().unwrap().unwrap();
    assert_eq!(report.parts_merged, 3, "the merge published across the late append: {report:?}");

    let want = vec![(0, 1.0), (MIN, 9.0), (2 * MIN, 1.0)];
    assert_eq!(closes(&df), want, "the late bar is newer than every merged one");
    drop(df);
    let reopened = HistoryStoreDataFusion::open(dir.path()).unwrap();
    assert_eq!(closes(&reopened), want, "the replayed manifest keeps the same order");
}

/// **Ticks are rows**: trades and quotes sharing one timestamp — identical ones included, inside a batch
/// and across commits — are all kept by the read and by a compaction.
#[test]
fn ticks_of_one_timestamp_are_all_kept() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    df.append_trades("binance", "BTCUSDT", &[tt(1_000, 1.0, 1.0), tt(1_000, 1.0, 1.0)], Some("t1"))
        .unwrap();
    df.append_trades("binance", "BTCUSDT", &[tt(1_000, 1.0, 1.0)], Some("t2")).unwrap();
    df.append_quotes("binance", "BTCUSDT", &[qt(1_000, 1.0), qt(1_000, 1.0)], Some("q1")).unwrap();
    df.append_quotes("binance", "BTCUSDT", &[qt(1_000, 1.0)], Some("q2")).unwrap();
    let counts = |df: &HistoryStoreDataFusion| {
        (
            df.scan_trades("binance", "BTCUSDT", TimestampRange::all()).unwrap().len(),
            df.scan_quotes("binance", "BTCUSDT", TimestampRange::all()).unwrap().len(),
        )
    };
    assert_eq!(counts(&df), (3, 3));
    for kind in [StoreKindId::Trade, StoreKindId::Quote] {
        let report = df.compact_series(kind, "binance", "BTCUSDT", None, &config()).unwrap();
        assert_eq!((report.rows, report.rows_superseded), (3, 0), "{kind}: {report:?}");
    }
    assert_eq!(counts(&df), (3, 3), "a compaction keeps every tick");
}
