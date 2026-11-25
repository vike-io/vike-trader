//! The WRITER's kept manifest (`ManifestMemo::take` and `ManifestMemo::keep`): after every write path the
//! copy the memo holds under the series' current stamp is exactly what a fresh read of its files gives
//! (and a debug build asserts the same at every keep), a commit through another handle is seen, a writer
//! that keeps no copy leaves none the next commit could use, the memo stays inside its byte budget, and
//! (ignored) what a commit and a reader's page cost on a 100 MB-class manifest with and without it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_data::store::hist::HistoryStore;
use vike_marketdata::{Bar, TickTrade};
use vike_model::time::epoch_ms_to_utc_date;

use super::super::BulkConfig;
use super::super::manifest::{FOLD_BYTES, FileEntry, fold_base};
use super::*;
use crate::hist_maint::{CompactionConfig, RetentionPolicy};

fn bar(ts: i64) -> Bar {
    Bar {
        timestamp: ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn trade(ts: i64, symbol: &str) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price: 1.0,
        size: 1.0,
        is_buyer_maker: false,
        symbol: symbol.into(),
    }
}

fn eurusd() -> SeriesId {
    SeriesId::per_symbol(StoreKindId::Bar, "oanda", "EURUSD", Some("1m".to_string()))
}

fn put(store: &HistoryStoreDataFusion, ts: &[i64], key: &str) -> usize {
    let bars: Vec<Bar> = ts.iter().map(|&t| bar(t)).collect();
    store.append_bars("oanda", "EURUSD", "1m", &bars, Some(key)).unwrap()
}

/// The copy the memo holds for `dir` under the series' CURRENT stamp, if any: what the next writer
/// would take instead of reading the files.
fn kept(store: &HistoryStoreDataFusion, dir: &Path) -> Option<Manifest> {
    let memo = store.manifest_memo.0.lock().unwrap();
    let held = memo.held.get(dir)?;
    (held.stamp == manifest_stamp(dir)).then(|| held.listed.manifest.clone())
}

#[track_caller]
fn assert_kept_is_fresh(store: &HistoryStoreDataFusion, dir: &Path, what: &str) {
    let fresh = read_manifest(dir).unwrap();
    assert_eq!(kept(store, dir).as_ref(), Some(&fresh), "after {what}");
}

/// Every writer that keeps a copy (each append path, the key-only spend, a day replacement, a fold, a
/// series' first base, the bulk profile, the WAL replay at open) leaves the one a fresh read gives.
#[test]
fn every_writer_that_keeps_a_copy_keeps_what_a_fresh_read_gives() {
    let root = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(root.path()).unwrap();
    let dir = store.bars_dir("oanda", "EURUSD", "1m");

    assert_eq!(put(&store, &[0, 60_000], "a"), 2);
    assert!(!dir.join(DELTA).exists(), "the first commit writes a base");
    assert_kept_is_fresh(&store, &dir, "a series' first commit (its base)");
    put(&store, &[MS_PER_DAY - 60_000, MS_PER_DAY], "b");
    assert_kept_is_fresh(&store, &dir, "a commit straddling midnight (two parts, one frame)");
    assert_eq!(put(&store, &[120_000], "a"), 0);
    assert_kept_is_fresh(&store, &dir, "a repeated key (the no-op)");
    store.append_bars("oanda", "EURUSD", "1m", &[bar(180_000)], None).unwrap();
    assert_kept_is_fresh(&store, &dir, "a keyless append");

    let provisional = "dukascopy-resample-provisional:EURUSD:1m:2-2";
    let early = [bar(2 * MS_PER_DAY)];
    store
        .append_bars_superseding("oanda", "EURUSD", "1m", &early, Some(provisional), None)
        .unwrap();
    assert_kept_is_fresh(&store, &dir, "a provisional commit");
    let settled = [bar(2 * MS_PER_DAY), bar(2 * MS_PER_DAY + 60_000)];
    let canonical = Some("dukascopy-resample:EURUSD:1m:2-2");
    store
        .append_bars_superseding("oanda", "EURUSD", "1m", &settled, canonical, Some(provisional))
        .unwrap();
    assert_kept_is_fresh(&store, &dir, "a superseding commit (one frame adds and removes)");

    assert_eq!(store.spend_keys_without_rows(&eurusd(), &["empty-day"]).unwrap(), 1);
    assert_kept_is_fresh(&store, &dir, "a key spent without rows");
    assert_eq!(store.spend_keys_without_rows(&eurusd(), &["empty-day", "a"]).unwrap(), 0);
    assert_kept_is_fresh(&store, &dir, "a spend with nothing left to spend");

    store.replace_bars_day("oanda", "EURUSD", "1m", "1970-01-01", &[bar(0)], "replace").unwrap();
    assert_kept_is_fresh(&store, &dir, "a day replacement");

    store.set_fold_bytes_for_test(1);
    put(&store, &[3 * MS_PER_DAY], "folds");
    assert!(!dir.join(DELTA).exists(), "the commit folded the log");
    assert_kept_is_fresh(&store, &dir, "a commit that folds the log into a new base");
    store.set_fold_bytes_for_test(FOLD_BYTES);
    put(&store, &[4 * MS_PER_DAY], "after-fold");
    assert_kept_is_fresh(&store, &dir, "the first frame after a fold");

    let ticks = [trade(0, "AAA"), trade(1_000, "BBB")];
    store.append_trades_grouped("polymarket", "grp", &ticks, Some("grouped")).unwrap();
    let group = store.group_dir(StoreKindId::Trade, "polymarket", "grp", None);
    assert_kept_is_fresh(&store, &group, "a grouped append");

    let mut session =
        store.bulk_session(BulkConfig { max_batches: 1, max_rows: 1 << 20, max_bytes: 1 << 30 });
    session.stage_trades("binance", "BTCUSDT", &[trade(0, ""), trade(1_000, "")]);
    session.end_batch("bulk").unwrap();
    let bulk = store.ticks_dir(StoreKindId::Trade, "binance", "BTCUSDT");
    assert_kept_is_fresh(&store, &bulk, "a bulk flush");

    // A commit sealed and never published (the crash window) keeps nothing; the next open's WAL
    // replay publishes it and keeps what it published.
    store.set_skip_publish_for_test(true);
    put(&store, &[5 * MS_PER_DAY], "crashed");
    store.set_skip_publish_for_test(false);
    assert_eq!(kept(&store, &dir), None, "a sealed, unpublished edit is never kept");
    let reopened = HistoryStoreDataFusion::open(root.path()).unwrap();
    assert_kept_is_fresh(&reopened, &dir, "the WAL replay at open");
    assert!(reopened.series_has_commit(&eurusd(), "crashed").unwrap());
}

/// A writer that keeps no copy (compaction, retention, rename, repair, delete) moves the stamp, so the
/// copy kept before it is not taken, and the next commit keeps a fresh one carrying the change.
#[test]
fn a_writer_that_keeps_no_copy_makes_the_next_commit_read_the_files() {
    type Write = fn(&HistoryStoreDataFusion);
    let writers: [(&str, Write); 5] = [
        ("a compaction", |s| {
            let config =
                CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
            let report =
                s.compact_series(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &config).unwrap();
            assert_eq!(report.parts_merged, 3, "{report:?}");
        }),
        ("a retention prune", |s| {
            let policy = RetentionPolicy { before_timestamp: Some(MS_PER_DAY), max_age_ms: None };
            s.apply_retention(StoreKindId::Bar, "oanda", "EURUSD", Some("1m"), &policy).unwrap();
        }),
        ("a rename away and back", |s| {
            s.rename_series(&eurusd(), "GBPUSD").unwrap();
            let gbpusd = SeriesId { symbol: "GBPUSD".to_string(), ..eurusd() };
            s.rename_series(&gbpusd, "EURUSD").unwrap();
        }),
        ("a repair", |s| {
            s.rebuild_series_manifest(&eurusd()).unwrap();
        }),
        ("a deletion", |s| s.delete_series(&eurusd()).unwrap()),
    ];
    for (what, write) in writers {
        let root = tempfile::tempdir().unwrap();
        let store = HistoryStoreDataFusion::open(root.path()).unwrap();
        let dir = store.bars_dir("oanda", "EURUSD", "1m");
        put(&store, &[0], "p1");
        put(&store, &[60_000], "p2");
        put(&store, &[120_000], "p3");
        put(&store, &[2 * MS_PER_DAY], "p4");
        assert_kept_is_fresh(&store, &dir, "the plant");
        write(&store);
        assert_eq!(kept(&store, &dir), None, "{what} left a copy the next commit would take");
        put(&store, &[3 * MS_PER_DAY], "next");
        assert_kept_is_fresh(&store, &dir, what);
        let rows = store.load_bars("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap();
        let fresh = HistoryStoreDataFusion::open_read_only(root.path()).unwrap();
        assert_eq!(rows, fresh.load_bars("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap());
    }
}

/// Another process, as far as a memo knows: a commit, a fold and a repair through a second handle each
/// move the stamp, and the first handle's next commit reads the files, so it lists the other's part
/// beside its own, under distinct names, and every row reads back.
#[test]
fn a_commit_a_fold_and_a_repair_through_another_handle_are_seen() {
    let root = tempfile::tempdir().unwrap();
    let ours = HistoryStoreDataFusion::open(root.path()).unwrap();
    let theirs = HistoryStoreDataFusion::open(root.path()).unwrap();
    let dir = ours.bars_dir("oanda", "EURUSD", "1m");
    let mut next = 0;
    let mut commit = |store: &HistoryStoreDataFusion| {
        next += 1;
        put(store, &[next * 60_000], &format!("k{next}"));
    };

    commit(&ours);
    assert_kept_is_fresh(&ours, &dir, "our commit");
    commit(&theirs);
    assert_eq!(kept(&ours, &dir), None, "their commit moved the stamp");
    commit(&ours);
    assert_kept_is_fresh(&ours, &dir, "our commit after theirs");

    theirs.set_fold_bytes_for_test(1);
    commit(&theirs);
    assert!(!dir.join(DELTA).exists(), "their commit folded");
    assert_eq!(kept(&ours, &dir), None, "their fold moved the stamp");
    commit(&ours);
    assert_kept_is_fresh(&ours, &dir, "our commit after their fold");

    theirs.rebuild_series_manifest(&eurusd()).unwrap();
    assert_eq!(kept(&ours, &dir), None, "their repair moved the stamp");
    commit(&ours);
    assert_kept_is_fresh(&ours, &dir, "our commit after their repair");

    let m = read_manifest(&dir).unwrap();
    let names: BTreeSet<(&str, &str)> =
        m.files.iter().map(|f| (f.name.as_str(), f.date.as_str())).collect();
    assert_eq!(names.len(), m.files.len(), "no part name was handed out twice: {:?}", m.files);
    let rows = ours.load_bars("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap();
    let minutes: Vec<i64> = rows.iter().map(|b| b.timestamp / 60_000).collect();
    assert_eq!(minutes, (1..=6).collect::<Vec<i64>>(), "every commit of both handles reads back");
}

/// Readers racing a writer on one handle: whatever they held or re-read meanwhile, the copy held under
/// the current stamp at the end is what the files say, and every commit is in it.
#[test]
fn readers_racing_a_writer_leave_no_stale_copy_under_a_current_stamp() {
    let root = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(root.path()).unwrap();
    let dir = store.bars_dir("oanda", "EURUSD", "1m");
    put(&store, &[0], "k0");
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                while !done.load(Ordering::SeqCst) {
                    store.load_bars("oanda", "EURUSD", "1m", TimestampRange::all()).unwrap();
                    store.series_commits(&eurusd()).unwrap();
                }
            });
        }
        for n in 1..40 {
            put(&store, &[n * 60_000], &format!("k{n}"));
        }
        done.store(true, Ordering::SeqCst);
    });
    assert_kept_is_fresh(&store, &dir, "the race");
    assert_eq!(store.series_commits(&eurusd()).unwrap().len(), 40);
}

/// The bound: the held JSON bytes never pass the budget, the least recently read or written goes
/// first, and a series larger than the whole budget or with no base is not held at all.
#[test]
fn the_memo_holds_at_most_its_budget_and_never_a_series_with_no_base() {
    let stamp = |base: Option<u64>, log: u64| {
        ManifestStamp([base.map(|len| (len, None, 0)), (log > 0).then_some((log, None, 0))])
    };
    let listed = || Arc::new(Listed::of(Manifest::empty()));
    let held = |memo: &MemoEntries| {
        let mut dirs: Vec<String> = memo.held.keys().map(|d| d.display().to_string()).collect();
        dirs.sort();
        dirs
    };
    let total = |memo: &MemoEntries| memo.held.values().map(|k| json_bytes(k.stamp)).sum::<u64>();
    let mut memo = MemoEntries::default();
    memo.admit(Path::new("a"), stamp(Some(300), 100), listed(), 1_000);
    memo.admit(Path::new("b"), stamp(Some(400), 0), listed(), 1_000);
    assert_eq!(held(&memo), ["a", "b"]);
    memo.admit(Path::new("c"), stamp(Some(400), 0), listed(), 1_000);
    assert_eq!(held(&memo), ["b", "c"], "the coldest goes first");
    assert!(total(&memo) <= 1_000);
    memo.admit(Path::new("b"), stamp(Some(450), 0), listed(), 1_000);
    memo.admit(Path::new("d"), stamp(Some(500), 0), listed(), 1_000);
    assert_eq!(held(&memo), ["b", "d"], "a series just written is the newest");
    memo.admit(Path::new("e"), stamp(Some(1_001), 0), listed(), 1_000);
    assert_eq!(held(&memo), ["b", "d"], "larger than the whole budget: not held, nothing evicted");
    memo.admit(Path::new("f"), stamp(None, 500), listed(), 1_000);
    memo.admit(Path::new("g"), stamp(None, 0), listed(), 1_000);
    assert_eq!(held(&memo), ["b", "d"], "no base: not held");
    assert!(total(&memo) <= 1_000);
}

// ---- the measurement ------------------------------------------------------------------------------

/// Bytes of heap a manifest's own allocations hold (capacities; no allocator overhead).
fn heap_bytes(m: &Manifest) -> usize {
    use std::mem::size_of;
    let strings = |v: &Vec<String>| {
        v.capacity() * size_of::<String>() + v.iter().map(String::capacity).sum::<usize>()
    };
    m.files.capacity() * size_of::<FileEntry>()
        + m.files
            .iter()
            .map(|f| f.name.capacity() + f.date.capacity() + strings(&f.commit_keys))
            .sum::<usize>()
        + strings(&m.orphan_commits)
}

fn median(mut runs: Vec<Duration>) -> Duration {
    runs.sort();
    runs[runs.len() / 2]
}

/// MEASUREMENT, print-only: a commit on a series whose base is a 100 MB-class manifest shaped like a
/// compacted recorded tape (1,200 parts of 1,000 commit keys each, keys as long as the Polymarket
/// book's), with the memo emptied before each commit (what every commit paid: the base parsed under the
/// lock) and with the writer's kept copy; a reader's page (the manifest it reads) parsed and from the
/// memo; and the parsed manifest's heap against its JSON. Run:
/// `cargo test -p vike-store --release --lib kept_manifest_tests::the_commit_measured -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement: it prints, and asserts only that the kept copy is the files' manifest"]
fn the_commit_measured() {
    const PARTS: i64 = 1_200;
    const KEYS: i64 = 1_000;
    let root = tempfile::tempdir().unwrap();
    let store = HistoryStoreDataFusion::open(root.path()).unwrap();
    let id = SeriesId::per_symbol(StoreKindId::Trade, "polymarket", "BIG", None);
    let dir = store.series_dir_of(&id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut planted = Manifest { version: 1, ..Manifest::empty() };
    let start = 1_789_000_000_000i64;
    for p in 0..PARTS {
        let first = start + p * KEYS * 10_000;
        let commit_keys = (0..KEYS)
            .map(|k| {
                let from = first + k * 10_000;
                format!("live-polymarket-btc-updown-5m-book-{from}-{}-{:05}", from + 9_999, k)
            })
            .collect();
        planted.files.push(FileEntry {
            name: format!("part-c{:08}.parquet", p + 1),
            date: epoch_ms_to_utc_date(first),
            ts_min: first,
            ts_max: first + KEYS * 10_000 - 1,
            rows: 17_010,
            commit_keys,
        });
    }
    let planted_heap = heap_bytes(&planted);
    fold_base(&dir, &planted).unwrap();
    drop(planted);
    let base = std::fs::metadata(dir.join(MANIFEST)).unwrap().len();

    let parse = Instant::now();
    let parsed = read_manifest(&dir).unwrap();
    let parse = parse.elapsed();
    let heap = heap_bytes(&parsed);
    drop(parsed);
    println!(
        "planted base: {base} bytes of JSON, {} commit keys; parsed in {parse:?}; heap {heap} bytes \
         ({:.2} of the JSON; built in memory {planted_heap})",
        PARTS * KEYS,
        heap as f64 / base as f64
    );

    let now = 1_795_000_000_000i64;
    let mut n = 0;
    let mut commit = |store: &HistoryStoreDataFusion| {
        n += 1;
        let started = Instant::now();
        let ticks = [trade(now + n, "")];
        store.append_trades("polymarket", "BIG", &ticks, Some(&format!("measure-{n}"))).unwrap();
        started.elapsed()
    };
    let cold: Vec<Duration> = (0..5)
        .map(|_| {
            store.manifest_memo.0.lock().unwrap().held.clear();
            commit(&store)
        })
        .collect();
    let warm: Vec<Duration> = (0..21).map(|_| commit(&store)).collect();
    println!(
        "commit: re-reading the manifest (before) median {:?} of {cold:?}; kept copy (after) median {:?}, max {:?}",
        median(cold.clone()),
        median(warm.clone()),
        warm.iter().max().unwrap()
    );

    let page = |clear: bool| {
        (0..5)
            .map(|_| {
                if clear {
                    store.manifest_memo.0.lock().unwrap().held.clear();
                }
                let started = Instant::now();
                let listed = store.listed(&dir).unwrap();
                assert!(listed.manifest.files.len() as i64 > PARTS);
                started.elapsed()
            })
            .collect::<Vec<_>>()
    };
    let cold = median(page(true));
    commit(&store);
    let warm = median(page(false));
    println!("a reader's page: the manifest parsed (before) {cold:?}; the memo's (after) {warm:?}");
    assert_eq!(kept(&store, &dir).as_ref(), Some(&read_manifest(&dir).unwrap()));
}
