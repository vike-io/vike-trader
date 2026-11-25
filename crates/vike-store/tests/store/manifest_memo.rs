//! The readers' manifest memo (`ManifestMemo`): a lock-free read answers a repeat from the series'
//! parsed manifest while the manifest files' stamp holds, and `series_days` its parts' stats with it, exactly
//! as a store without the memo answers; and EVERY writer moves the stamp, through any handle.
//!
//! The memo is observed from outside by changing what it holds behind the store's back in a way no
//! writer does: a part's mtime moved (what `series_days` stamps), or the log's last frame damaged with its
//! length, mtime and inode kept (what a manifest read parses). A store answering from the memo answers
//! as before; a store without one answers otherwise.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use vike_data::live::DataClientSink;
use vike_data::store::store_kind::RECORDER_LANE;
use vike_data::{BarDay, HistoryStore, SeriesId, TimestampRange};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;
use vike_store::{
    CompactionConfig, HistoryStoreDataFusion, RecorderConfig, RecorderSink, RetentionPolicy,
};

use crate::common::{bar, tt};

/// The days of `oanda EURUSD 1m`, the venue's own candles or `lane`'s.
fn days_on(df: &HistoryStoreDataFusion, lane: Option<&str>, range: TimestampRange) -> Vec<BarDay> {
    df.series_days(StoreKindId::Bar, "oanda", lane, "EURUSD", Some("1m"), range).unwrap()
}

fn put_on(df: &HistoryStoreDataFusion, lane: Option<&str>, ts: &[i64], key: &str) {
    let bars: Vec<_> = ts.iter().map(|&t| bar(t, 1.0, None)).collect();
    match lane {
        None => df.append_bars("oanda", "EURUSD", "1m", &bars, Some(key)),
        Some(lane) => df.append_bars_sourced("oanda", lane, "EURUSD", "1m", &bars, Some(key)),
    }
    .unwrap();
}

fn eurusd() -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, "oanda", "EURUSD", Some("1m".to_string()))
}

fn series_of(root: &Path) -> PathBuf {
    root.join("kind=bar/venue=oanda/symbol=EURUSD/interval=1m")
}

fn fresh(root: &Path) -> HistoryStoreDataFusion {
    HistoryStoreDataFusion::open_read_only(root).unwrap()
}

/// Days 0 (two parts), 1 and 2: a base, then a log of two frames, the last one `plant-c`'s.
fn plant(df: &HistoryStoreDataFusion, lane: Option<&str>) {
    put_on(df, lane, &[0, 60_000], "plant-a");
    put_on(df, lane, &[120_000], "plant-b");
    put_on(df, lane, &[MS_PER_DAY, 2 * MS_PER_DAY], "plant-c");
}

/// Move day 0's first part's mtime behind the store's back.
fn touch_day_zero(root: &Path, lane: Option<&str>) {
    let venue = root.join("kind=bar/venue=oanda");
    let series = match lane {
        Some(lane) => venue.join(format!("source={lane}")),
        None => venue,
    };
    let part = series.join("symbol=EURUSD/interval=1m/date=1970-01-01/part-00001.parquet");
    let file = std::fs::File::options().write(true).open(&part).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)).unwrap();
}

/// Damage the log's last frame in place and put its mtime back: the stamp (length, mtime, inode) does
/// not move, and a read of the log now stops before that frame (it fails its CRC).
fn damage_the_last_frame_keeping_the_stamp(series: &Path) {
    let log = series.join("_manifest.delta");
    let meta = std::fs::metadata(&log).unwrap();
    let mut file = std::fs::File::options().write(true).open(&log).unwrap();
    file.seek(SeekFrom::Start(meta.len() - 2)).unwrap();
    file.write_all(b"##").unwrap();
    file.set_modified(meta.modified().unwrap()).unwrap();
}

/// What the readers answer of the planted series, through every kind of manifest read.
#[derive(Debug, PartialEq)]
struct Answers {
    bars: Vec<i64>,
    trades: Vec<i64>,
    edges: (Option<i64>, Option<i64>),
    commits: Vec<String>,
    holds_plant_c: bool,
    facts: Vec<String>,
    first_last: (i64, i64),
    gaps: Vec<(i64, i64)>,
    days: Vec<BarDay>,
}

fn answers(df: &HistoryStoreDataFusion) -> Answers {
    let all = TimestampRange::all();
    let edges = df.bar_edges("oanda", "EURUSD", "1m", all).unwrap();
    let (coverage, facts) = df.series_facts(&eurusd()).unwrap();
    Answers {
        bars: df
            .load_bars("oanda", "EURUSD", "1m", all)
            .unwrap()
            .iter()
            .map(|b| b.timestamp)
            .collect(),
        trades: df
            .scan_trades("oanda", "EURUSD", all)
            .unwrap()
            .iter()
            .map(|t| t.timestamp)
            .collect(),
        edges: (edges.first_timestamp, edges.last_timestamp),
        commits: df.series_commits(&eurusd()).unwrap(),
        holds_plant_c: df.series_has_commit(&eurusd(), "plant-c").unwrap(),
        facts,
        first_last: (coverage.first_timestamp, coverage.last_timestamp),
        gaps: df.series_gaps(&eurusd()).unwrap(),
        days: days_on(df, None, all),
    }
}

/// A repeat of every reader is the memo's: a log frame damaged behind the store's back with its stamp
/// kept does not move any answer, while a store without the memo stops before the frame.
#[test]
fn every_reader_answers_a_repeat_from_the_memo() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&df, None);
    let first = answers(&df);
    assert_eq!(first.bars, vec![0, 60_000, 120_000, MS_PER_DAY, 2 * MS_PER_DAY]);
    damage_the_last_frame_keeping_the_stamp(&series_of(dir.path()));
    assert_eq!(answers(&df), first, "every repeat is the memo's");
    let cold = answers(&fresh(dir.path()));
    assert_eq!(cold.bars, vec![0, 60_000, 120_000], "a store without the memo misses the frame");
    assert!(!cold.holds_plant_c);
}

/// Every reader sees a write made through another handle (another process, as far as its memo knows).
#[test]
fn every_reader_sees_a_write_by_another_handle() {
    let dir = tempfile::tempdir().unwrap();
    let writer = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&writer, None);
    writer.append_trades("oanda", "EURUSD", &[tt(0, 1.0, 1.0)], Some("trades-a")).unwrap();
    let reader = fresh(dir.path());
    let before = answers(&reader);
    put_on(&writer, None, &[5 * MS_PER_DAY], "later");
    writer.append_trades("oanda", "EURUSD", &[tt(MS_PER_DAY, 1.0, 1.0)], Some("trades-b")).unwrap();
    let after = answers(&reader);
    assert_eq!(after, answers(&fresh(dir.path())));
    assert_eq!(after.bars.last(), Some(&(5 * MS_PER_DAY)));
    assert_eq!(after.trades, vec![0, MS_PER_DAY]);
    assert!(after.commits.contains(&"later".to_string()) && after.facts.len() > before.facts.len());
    assert_eq!(after.gaps.len(), 1, "days 3 and 4: {:?}", after.gaps);
}

/// `series_days` stats no part on a repeat: the touched part does not move the answer.
#[test]
fn a_repeat_of_bar_days_stats_no_part() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&df, None);
    let first = days_on(&df, None, TimestampRange::all());
    touch_day_zero(dir.path(), None);
    assert_eq!(days_on(&df, None, TimestampRange::all()), first, "the repeat is the memo's");
    let cold = days_on(&fresh(dir.path()), None, TimestampRange::all());
    assert_ne!(cold[0].stamp, first[0].stamp, "a store without the memo stats the touched part");
    assert_eq!(cold[1..], first[1..]);
}

/// Bit for bit, memo on and off: a memo filled by a narrow range (most parts never stat-ed) answers
/// every range, then every range again, as a store with no memo answers it.
#[test]
fn a_memo_answers_every_range_as_a_store_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&df, None);
    put_on(&df, None, &[2 * MS_PER_DAY + 60_000, 4 * MS_PER_DAY], "more");
    let ranges = [
        TimestampRange::of(MS_PER_DAY, MS_PER_DAY),
        TimestampRange::all(),
        TimestampRange::of(60_000, 2 * MS_PER_DAY),
        TimestampRange::of(90_000, 3 * MS_PER_DAY),
        TimestampRange { start: Some(2 * MS_PER_DAY + 1), end: None },
        TimestampRange::of(5 * MS_PER_DAY, 6 * MS_PER_DAY),
    ];
    for _ in 0..2 {
        for range in ranges {
            let cold = days_on(&fresh(dir.path()), None, range);
            assert_eq!(days_on(&df, None, range), cold, "{range:?}");
        }
    }
}

// ---- every writer moves the stamp ---------------------------------------------------------------

/// Plant the series through one handle, fill a second handle's memo (the datahub's), make that memo
/// stale behind the store's back, run `write` through the first (any process: an import, the
/// recorder, a repair) and hold the reader's answer then to a store with no memo. A write that left
/// the series' manifest stamp where it was would leave the reader answering what it answered before,
/// which the touched part makes a wrong answer. `series_days` is the reader asked: its answer covers the
/// manifest AND the part stats. Returns the days after the write.
fn the_write_moves_the_memo(
    lane: Option<&str>,
    write: impl FnOnce(&HistoryStoreDataFusion, &Path),
) -> Vec<BarDay> {
    let dir = tempfile::tempdir().unwrap();
    let writer = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&writer, lane);
    let reader = fresh(dir.path());
    let all = TimestampRange::all();
    let before = days_on(&reader, lane, all);
    touch_day_zero(dir.path(), lane);
    assert_eq!(days_on(&reader, lane, all), before, "the memo answers until a write");
    write(&writer, dir.path());
    let cold = days_on(&fresh(dir.path()), lane, all);
    assert_ne!(cold, before, "a memo the write left alone would answer otherwise");
    assert_eq!(days_on(&reader, lane, all), cold, "the write moved the reader's memo");
    cold
}

/// A bar import, a backfill, a starter dataset and the datahub's start-history saver all append.
#[test]
fn an_append_moves_the_memo() {
    let after = the_write_moves_the_memo(None, |w, _| put_on(w, None, &[5 * MS_PER_DAY], "later"));
    assert_eq!(after.last().map(|d| d.day), Some(5));
}

#[test]
fn a_csv_user_lane_moves_the_memo() {
    let lane = Some("user-csv");
    the_write_moves_the_memo(lane, |w, _| put_on(w, lane, &[5 * MS_PER_DAY], "csv-later"));
}

/// The datahub's live-bar recorder, through its real sink: a closed bar flushed into its lane.
#[test]
fn the_live_bar_recorder_moves_the_memo() {
    let after = the_write_moves_the_memo(Some(RECORDER_LANE), |_, root| {
        let store = Arc::new(HistoryStoreDataFusion::open(root).unwrap());
        let config = RecorderConfig { record_bars: true, ..RecorderConfig::default() };
        let (sink, handle) = RecorderSink::spawn(store, config).unwrap();
        sink.close_bar("oanda", "EURUSD", "1m", bar(5 * MS_PER_DAY, 1.0, None));
        handle.shutdown();
    });
    assert_eq!(after.last().map(|d| d.day), Some(5));
}

/// The archive import's day replacement: a day restamped by a new part under every old key.
#[test]
fn a_replaced_day_moves_the_memo() {
    the_write_moves_the_memo(None, |w, _| {
        let day = [bar(0, 2.0, None)];
        w.replace_bars_day("oanda", "EURUSD", "1m", "1970-01-01", &day, "replace").unwrap();
    });
}

/// `hist rename`: the old name empties ...
#[test]
fn a_rename_moves_the_memo_of_the_old_name() {
    let after = the_write_moves_the_memo(None, |w, _| {
        w.rename_series(&eurusd(), "GBPUSD").unwrap();
    });
    assert!(after.is_empty(), "{after:?}");
}

/// ... and the name it merges into fills.
#[test]
fn a_rename_moves_the_memo_of_the_name_it_merges_into() {
    let dir = tempfile::tempdir().unwrap();
    let writer = HistoryStoreDataFusion::open(dir.path()).unwrap();
    plant(&writer, None);
    let theirs = [bar(3 * MS_PER_DAY, 1.0, None)];
    writer.append_bars("oanda", "GBPUSD", "1m", &theirs, Some("theirs")).unwrap();
    let target = |df: &HistoryStoreDataFusion| {
        df.series_days(StoreKindId::Bar, "oanda", None, "GBPUSD", Some("1m"), TimestampRange::all())
            .unwrap()
    };
    let reader = fresh(dir.path());
    let before = target(&reader);
    writer.rename_series(&eurusd(), "GBPUSD").unwrap();
    let cold = target(&fresh(dir.path()));
    assert_eq!(cold.len(), before.len() + 3, "{cold:?}");
    assert_eq!(target(&reader), cold);
}

#[test]
fn a_deletion_moves_the_memo() {
    let after = the_write_moves_the_memo(None, |w, _| w.delete_series(&eurusd()).unwrap());
    assert!(after.is_empty(), "{after:?}");
}

#[test]
fn a_retention_prune_moves_the_memo() {
    let after = the_write_moves_the_memo(None, |w, _| {
        let policy = RetentionPolicy { before_timestamp: Some(MS_PER_DAY), max_age_ms: None };
        w.apply_retention(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &policy).unwrap();
    });
    assert_eq!(after.iter().map(|d| d.day).collect::<Vec<_>>(), vec![1, 2]);
}

/// A part compaction: day 0's two parts become one.
#[test]
fn a_compaction_moves_the_memo() {
    the_write_moves_the_memo(None, |w, _| {
        let config = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
        let report =
            w.compact_series(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &config).unwrap();
        assert_eq!(report.parts_merged, 2, "{report:?}");
    });
}

/// The change log's compaction: a publish that folds the log into a new base and removes it, here a
/// key-only one (an empty day's marker) that changes no part.
#[test]
fn a_fold_of_the_change_log_moves_the_memo() {
    the_write_moves_the_memo(None, |w, root| {
        w.set_fold_bytes_for_test(1);
        w.spend_keys_without_rows(&eurusd(), &["an-empty-day"]).unwrap();
        assert!(!series_of(root).join("_manifest.delta").exists(), "the log folded");
    });
}

/// An append that crashed between its part's seal and its publish, published by the next open's
/// recovery.
#[test]
fn an_append_recovered_at_open_moves_the_memo() {
    the_write_moves_the_memo(None, |w, root| {
        w.set_skip_publish_for_test(true);
        put_on(w, None, &[5 * MS_PER_DAY], "crashed");
        drop(HistoryStoreDataFusion::open(root).unwrap());
    });
}

/// A provisional day settled (the superseding appends), read through two handles: the reader's memo
/// held the provisional part, and the settled commit moves it.
#[test]
fn a_provisional_day_settled_moves_the_memo() {
    let dir = tempfile::tempdir().unwrap();
    let writer = HistoryStoreDataFusion::open(dir.path()).unwrap();
    let provisional = "dukascopy-resample-provisional:EURUSD:1m:0-1";
    let early = [bar(0, 1.0, None)];
    writer
        .append_bars_superseding("oanda", "EURUSD", "1m", &early, Some(provisional), None)
        .unwrap();
    let reader = fresh(dir.path());
    let open = days_on(&reader, None, TimestampRange::all());
    assert!(open[0].provisional, "{open:?}");

    let settled = [bar(0, 1.0, None), bar(60_000, 1.1, None)];
    let canonical = Some("dukascopy-resample:EURUSD:1m:0-1");
    writer
        .append_bars_superseding("oanda", "EURUSD", "1m", &settled, canonical, Some(provisional))
        .unwrap();
    let cold = days_on(&fresh(dir.path()), None, TimestampRange::all());
    assert!(!cold[0].provisional, "{cold:?}");
    assert_eq!(days_on(&reader, None, TimestampRange::all()), cold);
}

/// A repair rewrites the base and there is no log on either side of it (every publish of this series
/// folded), so only the base's stamp can say so: a NEW file renamed over the old, so a new inode even
/// when the length is the same and the mtime falls in the same clock tick.
#[cfg(unix)]
#[test]
fn a_repair_moves_the_memo_even_when_it_rewrites_the_base_to_the_same_length() {
    let dir = tempfile::tempdir().unwrap();
    let writer = HistoryStoreDataFusion::open(dir.path()).unwrap();
    writer.set_fold_bytes_for_test(1);
    plant(&writer, None);
    assert!(!series_of(dir.path()).join("_manifest.delta").exists(), "every publish folded");
    let reader = fresh(dir.path());
    let before = days_on(&reader, None, TimestampRange::all());
    touch_day_zero(dir.path(), None);
    writer.rebuild_series_manifest(&eurusd()).unwrap();
    let cold = days_on(&fresh(dir.path()), None, TimestampRange::all());
    assert_ne!(cold, before);
    assert_eq!(days_on(&reader, None, TimestampRange::all()), cold);
}

// ---- the measurement ------------------------------------------------------------------------------

/// MEASUREMENT, print-only: a series shaped like the `1d` input a JOAT run asks for on the CI box (2026-11-19:
/// 3,363 one-day parts, a 678 kB log never folded), asked as a backtest asks it. `series_days` over the
/// whole history by a store with no memo (the log replayed, every part stat-ed: what every run paid)
/// and again by one with it; the same over one day (the manifest read alone, nearly); and that one-day
/// read again once the log is folded into the base (what folding a growing series sooner would buy). Run:
/// `cargo test -p vike-store --release --test store manifest_memo::the_memo_measured -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: it prints, and asserts only that the answers agree"]
fn the_memo_measured() {
    const DAYS: i64 = 3_363;
    const PER_PUBLISH: i64 = 34; // ~100 publishes: a base and a log of ~99 frames
    let dir = tempfile::tempdir().unwrap();
    let df = HistoryStoreDataFusion::open(dir.path()).unwrap();
    for first in (0..DAYS).step_by(PER_PUBLISH as usize) {
        let ts: Vec<i64> =
            (first..(first + PER_PUBLISH).min(DAYS)).map(|d| d * MS_PER_DAY).collect();
        put_on(&df, None, &ts, &format!("klines:oanda:EURUSD:1m:{first}"));
    }
    let series = series_of(dir.path());
    let size = |name: &str| std::fs::metadata(series.join(name)).map_or(0, |m| m.len());
    let median = |mut runs: Vec<Duration>| {
        runs.sort();
        runs[runs.len() / 2]
    };
    // `runs` stores with no memo, each asked once; then one store asked `runs` times.
    let timed = |range: TimestampRange, runs: usize| {
        let want = days_on(&fresh(dir.path()), None, range);
        let mut cold = Vec::new();
        for _ in 0..runs {
            let store = fresh(dir.path());
            let started = Instant::now();
            let got = days_on(&store, None, range);
            cold.push(started.elapsed());
            assert_eq!(got, want);
        }
        let store = fresh(dir.path());
        assert_eq!(days_on(&store, None, range), want);
        let mut warm = Vec::new();
        for _ in 0..runs {
            let started = Instant::now();
            let got = days_on(&store, None, range);
            warm.push(started.elapsed());
            assert_eq!(got, want);
        }
        (want.len(), median(cold), median(warm))
    };
    let day = TimestampRange::of(DAYS / 2 * MS_PER_DAY, DAYS / 2 * MS_PER_DAY);
    let (base, log) = (size("_manifest.json"), size("_manifest.delta"));
    let (n, cold, warm) = timed(TimestampRange::all(), 21);
    println!("{n} days, a {base}-byte base and a {log}-byte log: no memo {cold:?}, memo {warm:?}");
    let (n, cold, warm) = timed(day, 21);
    println!("{n} day, the same manifest: no memo {cold:?}, memo {warm:?}");
    df.set_fold_bytes_for_test(1);
    df.spend_keys_without_rows(&eurusd(), &["fold-now"]).unwrap();
    let (base, log) = (size("_manifest.json"), size("_manifest.delta"));
    let (n, cold, warm) = timed(day, 21);
    println!(
        "{n} day, the log folded ({base}-byte base, {log}-byte log): no memo {cold:?}, memo {warm:?}"
    );
}
