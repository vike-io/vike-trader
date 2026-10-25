//! The START HISTORY against the REAL store: the production [`ingest_writer`] over a `DataFusionHist` in a
//! temporary directory. `crates/vike-datahub/src/strat/history_tests.rs` proves the saver's behaviour over an
//! in-memory double; this file proves what only the real store can: that the existing kline ingest takes the
//! bar through its codec, its still-forming guard and its commit key, that the name is the lane's and a slash
//! is refused, that a restart and even a COMPACTION leave one row per bar, and what a day of bars costs on disk.
//!
//! Behind `backfill-serve` for the reason `backfill_roundtrip.rs` is: the kline ingest exists only there. Nothing
//! here reaches a venue.
#![cfg(feature = "backfill-serve")]

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tempfile::TempDir;
use vike_data::{HistStore, SeriesId, StoreKindId, TsRange};
use vike_datahub::strat::history::ingest_writer;
use vike_marketdata::Bar;
use vike_store::{DataFusionHist, MaintenanceConfig};

const STEP: i64 = 60_000;
/// 2026-10-19T00:00:00Z: a UTC midnight, so a day of bars is one `date=` partition, and in the past.
const DAY0: i64 = 1_792_368_000_000;

fn bar_at(i: i64) -> Bar {
    bar_on(DAY0, i)
}

/// The bar that opens `i` minutes after `base`.
fn bar_on(base: i64, i: i64) -> Bar {
    let f = i as f64 / 1000.0;
    Bar {
        ts: base + i * STEP,
        open: 1.0 + f,
        high: 1.1 + f,
        low: 0.9 + f,
        close: 1.05 + f,
        volume: 10.0 + i as f64,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("the store opens"));
    (dir, store)
}

fn read(store: &DataFusionHist, symbol: &str) -> Vec<Bar> {
    store.load_bars("oanda", symbol, "1m", TsRange::all()).expect("the store reads")
}

fn coverage(store: &DataFusionHist) -> (u64, usize, u64) {
    let id = SeriesId::per_symbol(StoreKindId::Bar, "oanda", "EURUSD", Some("1m".to_string()));
    let all = store.inventory().expect("the inventory");
    let (_, c) = all.iter().find(|(s, _)| *s == id).expect("the series is listed");
    (c.rows, c.parts, c.bytes)
}

/// Every byte under `root`, files counted too: the manifest and its delta log are part of what a series costs.
fn disk(root: &Path) -> (u64, usize) {
    let (mut bytes, mut files) = (0, 0);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory").flatten() {
            let meta = entry.metadata().expect("metadata");
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += meta.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

/// Every file under `root` as `(path below root, bytes)`, largest first: what the byte total is made of.
fn files_by_size(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("a readable directory").flatten() {
            let meta = entry.metadata().expect("metadata");
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                let rel =
                    entry.path().strip_prefix(root).expect("below root").display().to_string();
                out.push((rel.replace('\\', "/"), meta.len()));
            }
        }
    }
    out.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    out
}

/// **The existing ingest takes the bar, once, under the lane's name.** A closed bar lands as one row, a repeat
/// of it is the idempotent no-op (a restart's second holder included), `EURUSD` and `EUR_USD` are two series,
/// and the store's door refuses a name with a slash - an error, never a panic, and nothing stored.
#[test]
fn a_closed_bar_is_saved_once_under_the_lanes_name_and_a_slash_is_refused() {
    let (_dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(0)), Ok(1));
    assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(0)), Ok(0), "the same bar again is a no-op");
    let restarted = ingest_writer(Arc::clone(&store));
    assert_eq!(restarted("oanda", "EURUSD", "1m", &bar_at(0)), Ok(0), "...and so after a restart");
    assert_eq!(read(&store, "EURUSD"), vec![bar_at(0)]);
    assert!(read(&store, "EUR_USD").is_empty(), "OANDA's own spelling is a different series");

    assert!(write("oanda", "EUR/USD", "1m", &bar_at(1)).is_err(), "a slash adds a partition level");
    assert_eq!(read(&store, "EURUSD").len(), 1);
    assert!(write("oanda", "EURUSD", "1w", &bar_at(1)).is_err(), "no bar width, no write");
}

/// **A forming bar is never stored as closed, and refusing it is LOUD**: a bar whose interval has not ended is
/// an error (a counted, announced hole), not the `Ok(0)` of an idempotent repeat that the ingest's own guard
/// would answer, and the refusal spends no commit key - so the same bar, once it has closed, still lands.
#[test]
fn a_bar_whose_interval_has_not_ended_is_refused_not_stored() {
    let (_dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    let now = vike_model::now_ms();
    let mut forming = bar_at(0);
    forming.ts = now.div_euclid(STEP) * STEP; // the minute that is open right now
    let refused = write("oanda", "EURUSD", "1m", &forming);
    assert!(refused.as_ref().is_err_and(|e| e.contains("has not ended")), "{refused:?}");
    assert!(read(&store, "EURUSD").is_empty(), "the open minute was not stored");
    assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(0)), Ok(1), "a closed bar still lands");
}

/// **No hole, no duplicate, in order** - a run of closes written one at a time, each repeated as a reconnect
/// would - reads back as exactly the bars, ascending.
#[test]
fn a_run_of_closes_reads_back_once_each_and_in_order() {
    let (_dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    for i in 0..10 {
        assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(i)), Ok(1));
        assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(i)), Ok(0));
    }
    let got = read(&store, "EURUSD");
    assert_eq!(got, (0..10).map(bar_at).collect::<Vec<_>>());
}

/// **A compaction pass keeps every bar once, and the bars it merged stay spent**: one part per close becomes
/// one part per day, the rows are the same, and a bar written again after the pass (the bridge re-emitting
/// after a restart) is still the no-op - the commit key went with the rows into the merged part.
#[test]
fn compaction_merges_the_one_bar_parts_and_keeps_every_bar_once() {
    let (_dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    for i in 0..24 {
        assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(i)), Ok(1));
    }
    let (rows, parts, _) = coverage(&store);
    assert_eq!((rows, parts), (24, 24), "one part per close before any pass");

    store.run_maintenance(&MaintenanceConfig::default()).expect("a pass");
    let (rows, parts, _) = coverage(&store);
    assert_eq!((rows, parts), (24, 1), "one part for the day, the same rows");
    assert_eq!(read(&store, "EURUSD"), (0..24).map(bar_at).collect::<Vec<_>>());

    assert_eq!(
        write("oanda", "EURUSD", "1m", &bar_at(3)),
        Ok(0),
        "a spent bar stays spent after a pass"
    );
    assert_eq!(
        write("oanda", "EURUSD", "1m", &bar_at(24)),
        Ok(1),
        "and the next close still lands"
    );
    assert_eq!(read(&store, "EURUSD").len(), 25);
}

/// **MEASUREMENT, not an assertion of a number: what a day of one-minute bars costs on disk.** The design
/// estimated "about a megabyte a day for ten keys"; this writes a day's fourteen hundred and forty closes of one
/// key one at a time (the shape of the live save), prints the bytes, the files and the parts BEFORE a
/// maintenance pass and AFTER it, and the wall time per commit. Ten keys are ten times each figure.
///
/// Run on a lane: `cargo test -p vike-datahub --features backfill-serve --release --test start_history_store
/// -- --ignored --nocapture a_day_of_one_minute_bars_costs_this_on_disk`. The only assertions are the ones that
/// make the print mean something: the rows are all there, once, before and after.
#[test]
#[ignore = "a measurement: fourteen hundred commits; run on a lane with --ignored --nocapture"]
fn a_day_of_one_minute_bars_costs_this_on_disk() {
    let (dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    let started = Instant::now();
    for i in 0..1_440 {
        assert_eq!(write("oanda", "EURUSD", "1m", &bar_at(i)), Ok(1));
    }
    let wrote = started.elapsed();
    let (rows, parts, part_bytes) = coverage(&store);
    let (before_bytes, before_files) = disk(dir.path());
    assert_eq!((rows, parts), (1_440, 1_440));

    let pass = Instant::now();
    store.run_maintenance(&MaintenanceConfig::default()).expect("a pass");
    let passed = pass.elapsed();
    let (rows_after, parts_after, part_bytes_after) = coverage(&store);
    let (after_bytes, after_files) = disk(dir.path());
    assert_eq!(rows_after, 1_440, "the pass lost no bar");
    assert_eq!(read(&store, "EURUSD").len(), 1_440);

    println!(
        "START-HISTORY-VOLUME key=oanda/EURUSD/1m bars=1440 commit_ms_each={:.1} pass_ms={}",
        wrote.as_secs_f64() * 1000.0 / 1440.0,
        passed.as_millis()
    );
    println!(
        "START-HISTORY-VOLUME before parts={parts} part_bytes={part_bytes} disk_bytes={before_bytes} files={before_files}"
    );
    println!(
        "START-HISTORY-VOLUME after  parts={parts_after} part_bytes={part_bytes_after} disk_bytes={after_bytes} files={after_files}"
    );
    println!(
        "START-HISTORY-VOLUME ten_keys_after_bytes={} ten_keys_before_bytes={}",
        after_bytes * 10,
        before_bytes * 10
    );
    for (file, bytes) in files_by_size(dir.path()) {
        println!("START-HISTORY-VOLUME after file={file} bytes={bytes}");
    }
}

/// **MEASUREMENT: does a bar a minute for a fortnight cost more per commit, and how big does the series'
/// bookkeeping get?** Every saved bar spends one commit key, and a compaction pass merges the files but keeps
/// the keys, so the manifest of a series grows by one key per bar for as long as the series lives. This writes
/// fourteen days of one-minute bars one commit at a time with a maintenance pass after each day (the shape of
/// the shipped five-minute pass, coarser), and prints per day the mean commit time and what the series
/// occupies on disk, so the growth is a table and not a guess. The only assertion is that no bar is lost or
/// doubled.
///
/// Run on a lane: `cargo test -p vike-datahub --features backfill-serve --release --test start_history_store
/// -- --ignored --nocapture --test-threads=1 a_fortnight_of_one_minute_bars_grows_the_series_this_much`.
#[test]
#[ignore = "a measurement: twenty thousand commits; run on a lane with --ignored --nocapture"]
fn a_fortnight_of_one_minute_bars_grows_the_series_this_much() {
    const DAYS: i64 = 14;
    const PER_DAY: i64 = 1_440;
    let (dir, store) = store();
    let write = ingest_writer(Arc::clone(&store));
    let day_ms = 24 * 60 * STEP;
    // Start sixteen days back from the top of the current UTC day, so every bar is closed.
    let base = (vike_model::now_ms().div_euclid(day_ms) - DAYS - 2) * day_ms;
    for day in 0..DAYS {
        let started = Instant::now();
        for i in 0..PER_DAY {
            let n = day * PER_DAY + i;
            assert_eq!(write("oanda", "EURUSD", "1m", &bar_on(base, n)), Ok(1), "bar {n}");
        }
        let commit_ms = started.elapsed().as_secs_f64() * 1000.0 / PER_DAY as f64;
        let pass = Instant::now();
        store.run_maintenance(&MaintenanceConfig::default()).expect("a pass");
        let (bytes, files) = disk(dir.path());
        let (rows, parts, part_bytes) = coverage(&store);
        println!(
            "START-HISTORY-FORTNIGHT day={} commit_ms_mean={commit_ms:.1} pass_ms={} rows={rows} parts={parts} part_bytes={part_bytes} disk_bytes={bytes} files={files}",
            day + 1,
            pass.elapsed().as_millis()
        );
    }
    let all = read(&store, "EURUSD");
    assert_eq!(all.len(), (DAYS * PER_DAY) as usize, "no bar lost or doubled");
    for (file, bytes) in files_by_size(dir.path()).into_iter().take(6) {
        println!("START-HISTORY-FORTNIGHT end file={file} bytes={bytes}");
    }
}
