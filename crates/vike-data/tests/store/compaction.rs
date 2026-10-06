//! `date=` partitioning, compaction, retention, appends during a merge, and the merge row budget.

use std::sync::Arc;
use std::time::Duration;

use vike_data::{
    CompactionConfig, DataFusionHist, HistStore, MaintenanceConfig, RetentionPolicy, TsRange,
};
use vike_model::{Bar, BookLevel, BookUpdate, BookUpdateKind, QuoteTick, SymbolProperties};

use crate::common::{DAY, assert_bars_bit_eq, bar, bars_series, bu, count_parquets, qt, tt};

// ---- slice 4: date= partitioning + compaction + retention ----------------------------------

#[test]
fn date_split_places_parts_under_date_dirs() {
    // one append spanning three UTC days → one sealed part per date=, data bit-eq on read-back.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = vec![
        bar(0, 100.0, None),             // 1970-01-01
        bar(DAY / 2, 101.0, None),       // 1970-01-01
        bar(DAY, 102.0, None),           // 1970-01-02
        bar(DAY + DAY / 2, 103.0, None), // 1970-01-02
        bar(2 * DAY, 104.0, None),       // 1970-01-03
    ];
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap(), 5);

    let series = bars_series(dir.path());
    for d in ["date=1970-01-01", "date=1970-01-02", "date=1970-01-03"] {
        assert!(series.join(d).exists(), "missing partition {d}");
    }
    assert_eq!(count_parquets(&series), 3, "one part per date");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
}

#[test]
fn date_boundary_is_bit_identical() {
    // rows straddling 00:00:00.000 UTC — the civil-date split must introduce no value drift.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = vec![
        bar(DAY - 1, 200.0, Some(0.01)),  // last ms of 1970-01-01
        bar(DAY, 201.0, None),            // 1970-01-02 00:00:00.000
        bar(DAY + 1, 202.0, Some(-0.02)), // first ms after
    ];
    df.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&bars, &got);
    assert!(bars_series(dir.path()).join("date=1970-01-02").exists(), "boundary row → new day");
}

#[test]
fn compaction_merges_and_is_bit_identical() {
    // four same-day fragments → one sealed part; every row bit-preserved, fewer files on disk.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    let before = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(count_parquets(&series), 4);

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 20);

    assert!(count_parquets(&series) < 4, "compaction reduced part count");
    let after = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&before, &after);
}

/// Schema-evolution tolerance: a series whose parts were written under DIFFERENT schema revisions
/// (an old part lacking a later-added nullable column) must still scan — the decode-time upgrade
/// policy (book-recording plan Task 4). Until Task 5 adds the `local_ts` column there is only ONE
/// schema revision, so this canNOT yet exercise genuinely-mixed parts; it PINS the per-file `collect`
/// union that makes Task 5's real `old_quote_parts_readable_after_local_ts` possible — two normal
/// appends land as two parts and both parts' rows must come back.
#[test]
fn scan_tolerates_mixed_part_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // one normal quote append (current full schema)
    store.append_quotes("v", "S", &[qt(1_000, 1.0)], Some("k1")).unwrap();
    // a second normal part; per-file reads must union the two parts (the property Task 5 rides)
    store.append_quotes("v", "S", &[qt(2_000, 2.0)], Some("k2")).unwrap();
    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2, "per-file collect must union parts");
    assert_eq!(got[0].ts, 1_000);
    assert_eq!(got[1].ts, 2_000);
}

/// Compaction bit-identity for a NON-bar kind: enough same-day quote fragments to trip compaction,
/// then the domain-roundtrip rewrite (decode → sort → re-encode with the CURRENT schema, book-
/// recording plan Task 4) must preserve every f64 field bit-for-bit (`to_bits`) AND row count/order.
/// This is the proof the decode-time schema-upgrade path is lossless for f64 (`Float64Array::value`
/// → `Float64Array::from` round-trips bits).
#[test]
fn compaction_quotes_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // four disjoint ascending windows, all inside UTC day 0 → four fragments in one date=
    let mut want: Vec<QuoteTick> = Vec::new();
    for c in 0..4i64 {
        let batch: Vec<QuoteTick> = (0..5)
            .map(|i| {
                let n = c * 5 + i;
                QuoteTick {
                    ts: 1_000 + n * 1_000,
                    local_ts: 0,
                    bid: 100.0 + n as f64 * 0.25,
                    ask: 100.1 + n as f64 * 0.25,
                    bid_size: 1.0 + n as f64 * 0.01,
                    ask_size: 2.0 + n as f64 * 0.02,
                    symbol: String::new(),
                }
            })
            .collect();
        df.append_quotes("binance", "BTCUSDT", &batch, Some(&format!("q{c}"))).unwrap();
        want.extend(batch);
    }
    let quote_series = dir.path().join("kind=quote").join("venue=binance").join("symbol=BTCUSDT");
    assert_eq!(count_parquets(&quote_series), 4, "four quote fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("quote", "binance", "BTCUSDT", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 20);
    assert!(count_parquets(&quote_series) < 4, "compaction reduced the part count");

    let got = df.scan_quotes("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "row count preserved through compaction");
    for (i, (x, y)) in want.iter().zip(&got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.bid.to_bits(), y.bid.to_bits(), "bid[{i}]");
        assert_eq!(x.ask.to_bits(), y.ask.to_bits(), "ask[{i}]");
        assert_eq!(x.bid_size.to_bits(), y.bid_size.to_bits(), "bid_size[{i}]");
        assert_eq!(x.ask_size.to_bits(), y.ask_size.to_bits(), "ask_size[{i}]");
    }
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "ts ascending after compaction");
    assert_eq!(got[0].symbol, "BTCUSDT", "symbol re-injected on scan");
}

/// Compaction bit-identity for the `kind=properties` series (PIT `SymbolProperties` snapshots) — the
/// `"properties"` arm of `compact_roundtrip`. Three same-day fragments (distinct `commit_key`s so
/// they don't dedup) trip compaction; afterward `scan_symbol_properties` must reproduce every
/// original `(ts, SymbolProperties)` row bit-for-bit and in ts-ascending order.
#[test]
fn compaction_properties_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // three fragments, all inside UTC day 0 → three parts in one date= dir, each its own commit
    let mut want: Vec<(i64, SymbolProperties)> = Vec::new();
    for c in 0..3i64 {
        let ts = 1_000 + c * 1_000;
        let f = SymbolProperties {
            tick_size: 0.01 + c as f64 * 0.001,
            step_size: 0.001 + c as f64 * 0.0001,
            min_qty: 0.001 + c as f64 * 0.0001,
            max_qty: 100.0 + c as f64,
            min_notional: 5.0 + c as f64 * 0.5,
            contract_size: 0.0,
            tick_scheme: None,
            taker_hold_ms: 0,
            asset_class: None,
        };
        df.append_symbol_properties("bybit", "BTCUSDT", &[(ts, f)], Some(&format!("f{c}")))
            .unwrap();
        want.push((ts, f));
    }
    let properties_series =
        dir.path().join("kind=properties").join("venue=bybit").join("symbol=BTCUSDT");
    assert_eq!(count_parquets(&properties_series), 3, "three filter fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("properties", "bybit", "BTCUSDT", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 3);
    assert_eq!(rep.parts_written, 1);
    assert_eq!(rep.rows, 3);
    assert!(count_parquets(&properties_series) < 3, "compaction reduced the part count");

    let got = df.scan_symbol_properties("bybit", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "row count preserved through compaction");
    for (i, ((wts, wf), (gts, gf))) in want.iter().zip(&got).enumerate() {
        assert_eq!(wts, gts, "ts[{i}]");
        assert_eq!(wf.tick_size.to_bits(), gf.tick_size.to_bits(), "tick_size[{i}]");
        assert_eq!(wf.step_size.to_bits(), gf.step_size.to_bits(), "step_size[{i}]");
        assert_eq!(wf.min_qty.to_bits(), gf.min_qty.to_bits(), "min_qty[{i}]");
        assert_eq!(wf.max_qty.to_bits(), gf.max_qty.to_bits(), "max_qty[{i}]");
        assert_eq!(wf.min_notional.to_bits(), gf.min_notional.to_bits(), "min_notional[{i}]");
    }
    assert!(got.windows(2).all(|w| w[0].0 < w[1].0), "ts ascending after compaction");
}

/// Compaction bit-identity for the `kind=book` series — the per-level row-rewrite path of
/// `compact_roundtrip`'s `"book"` arm (book-recording plan Task 5). Enough same-day fragments to
/// trip compaction, spanning a multi-level Snapshot, a Delta with a REMOVAL level `(price>0, 0.0)`,
/// and a zero-level status event (`GapStart`, which persists as a placeholder row) — so every row
/// shape goes through the decode → sort → re-encode rewrite. Afterward `scan_book_updates` must
/// reproduce every original event bit-for-bit (`to_bits` on all f64), proving the row-level book
/// compaction is lossless (incl. placeholder rows surviving the round-trip).
#[test]
fn compaction_book_domain_roundtrip_is_bit_identical() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();

    // Four fragments, all inside UTC day 0 (ts in the low thousands of epoch-ms) → one date= dir.
    // Delta/Snapshot seqs are monotonic-distinct; status events (GapStart) carry seq 0.
    let parts: Vec<Vec<BookUpdate>> = vec![
        vec![
            bu(
                1_000,
                1,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.45, 100.0), BookLevel::new(0.44, 20.0)],
                vec![BookLevel::new(0.46, 50.0), BookLevel::new(0.47, 10.0)],
            ),
            bu(
                1_005,
                2,
                BookUpdateKind::Delta,
                vec![BookLevel::new(0.44, 0.0)],
                vec![BookLevel::new(0.46, 55.0)],
            ), // removal level
        ],
        vec![
            bu(1_010, 0, BookUpdateKind::GapStart, vec![], vec![]), // status → placeholder row
            bu(
                1_020,
                3,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.43, 5.0)],
                vec![BookLevel::new(0.48, 8.0)],
            ),
        ],
        vec![
            bu(1_030, 4, BookUpdateKind::Delta, vec![BookLevel::new(0.43, 6.0)], vec![]),
            bu(1_040, 5, BookUpdateKind::Delta, vec![], vec![BookLevel::new(0.48, 0.0)]), // ask removal
        ],
        vec![
            bu(
                1_050,
                6,
                BookUpdateKind::Snapshot,
                vec![BookLevel::new(0.42, 3.0), BookLevel::new(0.41, 2.0)],
                vec![BookLevel::new(0.49, 4.0)],
            ),
            bu(1_060, 0, BookUpdateKind::LiveResume, vec![], vec![]), // status → placeholder row
        ],
    ];
    let mut want: Vec<BookUpdate> = Vec::new();
    for (c, part) in parts.iter().enumerate() {
        df.append_book_updates("polymarket", "TOK", part, Some(&format!("b{c}"))).unwrap();
        want.extend(part.iter().cloned());
    }
    let book_series = dir.path().join("kind=book").join("venue=polymarket").join("symbol=TOK");
    assert_eq!(count_parquets(&book_series), 4, "four book fragments before compaction");

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = df.compact_series("book", "polymarket", "TOK", None, &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    assert!(count_parquets(&book_series) < 4, "compaction reduced the part count");

    let got = df.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), want.len(), "event count preserved through compaction");
    for (i, (x, y)) in want.iter().zip(&got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.local_ts, y.local_ts, "local_ts[{i}]");
        assert_eq!(x.seq, y.seq, "seq[{i}]");
        assert_eq!(x.kind, y.kind, "kind[{i}]");
        assert_eq!(x.tick_size.to_bits(), y.tick_size.to_bits(), "tick_size[{i}]");
        assert_eq!(x.bids.len(), y.bids.len(), "bids.len[{i}]");
        assert_eq!(x.asks.len(), y.asks.len(), "asks.len[{i}]");
        for (j, (a, b)) in x.bids.iter().zip(&y.bids).enumerate() {
            assert_eq!(a.price.to_bits(), b.price.to_bits(), "bid price[{i}][{j}]");
            assert_eq!(a.qty.to_bits(), b.qty.to_bits(), "bid qty[{i}][{j}]");
        }
        for (j, (a, b)) in x.asks.iter().zip(&y.asks).enumerate() {
            assert_eq!(a.price.to_bits(), b.price.to_bits(), "ask price[{i}][{j}]");
            assert_eq!(a.qty.to_bits(), b.qty.to_bits(), "ask qty[{i}][{j}]");
        }
    }
}

#[test]
fn compaction_preserves_commit_log_idempotency() {
    // after a merge, an already-committed key stays a no-op (GC never resurrects/double-counts).
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let mk =
        |c: i64| -> Vec<Bar> { (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0, None)).collect() };
    for c in 0..4i64 {
        df.append_bars("binance", "BTCUSDT", "1m", &mk(c), Some(&format!("b{c}"))).unwrap();
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &mk(0), Some("b0")).unwrap(), 0);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 20);
}

#[test]
fn retention_drops_old_date_dirs_and_commit_log() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let old: Vec<Bar> = (0..5).map(|i| bar(i * 1000, 100.0, None)).collect(); // day 0
    let new: Vec<Bar> = (0..5).map(|i| bar(10 * DAY + i * 1000, 200.0, None)).collect(); // day 10
    df.append_bars("binance", "BTCUSDT", "1m", &old, Some("old")).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &new, Some("new")).unwrap();

    let policy = RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None };
    let rep = df.apply_retention("bar", "binance", "BTCUSDT", Some("1m"), &policy).unwrap();
    assert_eq!(rep.files_dropped, 1);
    assert_eq!(rep.dates_dropped, 1);
    assert_eq!(rep.rows_dropped, 5);

    assert!(!bars_series(dir.path()).join("date=1970-01-01").exists(), "old date dir unlinked");
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_bars_bit_eq(&new, &got);

    // the pruned commit key was GC'd → re-backfill of that window APPENDS (not a false no-op)
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &old, Some("old")).unwrap(), 5);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 10);
}

#[test]
fn concurrent_append_and_compact_no_lost_update() {
    // the per-series lock serializes the manifest read-modify-write: an append racing a compaction
    // loses no rows.
    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    let mk = |c: i64| -> Vec<Bar> {
        (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect()
    };
    for c in 0..4i64 {
        df.append_bars("binance", "BTCUSDT", "1m", &mk(c), Some(&format!("s{c}"))).unwrap();
    }
    let d1 = df.clone();
    let d2 = df.clone();
    let appender = std::thread::spawn(move || {
        for c in 4..8i64 {
            let b: Vec<Bar> =
                (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
            d1.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("s{c}"))).unwrap();
        }
    });
    let compactor = std::thread::spawn(move || {
        let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
        d2.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    });
    appender.join().unwrap();
    compactor.join().unwrap();

    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 40, "all 8 batches * 5 rows present");
    assert!(got.windows(2).all(|w| w[0].ts < w[1].ts), "ts unique + ascending");
}

#[test]
fn retention_after_compaction_gcs_commit_log() {
    // compact a day, THEN prune it: the merged part must carry the union of its inputs' keys, so a
    // re-backfill of the pruned window APPENDS (guards the compact→prune→re-ingest false-no-op).
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let b: Vec<Bar> = (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &b, Some(&format!("k{c}"))).unwrap();
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    let policy = RetentionPolicy { before_ts: Some(5 * DAY), max_age_ms: None };
    df.apply_retention("bar", "binance", "BTCUSDT", Some("1m"), &policy).unwrap();
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 0);

    // keys k0..k3 (folded into the merged part, then pruned) were GC'd → re-backfill re-appends
    let b0: Vec<Bar> = (0..5).map(|i| bar(i * 1000, 100.0, None)).collect();
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &b0, Some("k0")).unwrap(), 5);
}

/// **An append lands DURING a compaction merge, and neither loses.**
///
/// This is the property the plan/merge/publish split exists to create. Compaction used to hold the
/// series lock across the whole decode→re-encode, so on a large series a concurrent `RecorderSink`
/// flush timed out and DISCARDED its buffer — 110,354 rows lost on the CI box in about an hour before
/// this was found. Now the lock is held only to plan and to publish.
///
/// Two things are asserted, and the ROW COUNTS are only the second of them.
///
/// The counts guard the LOST-UPDATE hazard the unlocked window introduces: the merge and the append
/// each rewrite the manifest, so an appended row could be dropped by a publish that overwrote it.
/// What the counts cannot guard is the hazard this split exists for, because they are satisfied by
/// the pre-fix code too. With the merge inside the lock the appender does not LOSE its rows — it
/// blocks on `SeriesLock::acquire` and then lands, and this series' merge — eight one-row parquet
/// parts — finishes three orders of magnitude inside the 2000 × 2 ms spin that
/// `crates/vike-data/src/store/datafusion_hist/manifest.rs`'s `SPIN_ATTEMPTS` budgets, so the timeout
/// that IS the production failure can never be reached here. The proof is in this file: the
/// pre-split `concurrent_append_and_compact_no_lost_update` asserts the same shape and passed
/// against the OLD code. (An earlier version of this doc claimed the opposite — that row counts
/// "cannot pass on a fast machine for the wrong reason". The weakness was never that the overlap
/// is UNLIKELY — the merge is the whole cost of compaction. It is that the overlap was not
/// ENFORCED.)
///
/// So the append is driven through the window BY CONSTRUCTION:
/// `DataFusionHist::set_pause_in_merge_for_test` parks the compactor with its merge output written,
/// the lock released and the publish not yet begun, and the appends run to completion there. Move
/// the merge back inside the lock and the compactor parks while HOLDING it, so the first append
/// spins its ~4 s budget out and fails — the exact error `crates/vike-data/src/rec/live_rec.rs`'s
/// `append_with_retry` deliberately does not retry ("it has already burned the lock spin"), which
/// is how the 110,354 rows became a hole in the tape rather than a delay.
#[test]
fn an_append_during_compaction_is_not_lost() {
    use std::sync::Arc;
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let df = Arc::new(DataFusionHist::open(dir.path()).unwrap());

    // Enough fragments on one date that compaction has real work to do.
    for i in 0..8i64 {
        df.append_trades("binance", "BTCUSDT", &[tt(1_000 + i, 1.0, 1.0)], Some(&format!("a{i}")))
            .unwrap();
    }
    assert_eq!(df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 8);

    // Hold the post-merge/pre-publish window open, then compact on its own thread.
    let (parked_tx, parked_rx) = mpsc::channel::<()>();
    let (resume_tx, resume_rx) = mpsc::channel::<()>();
    df.set_pause_in_merge_for_test(parked_tx, resume_rx);
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 2, ..Default::default() };
    let c = Arc::clone(&df);
    let compactor =
        std::thread::spawn(move || c.compact_series("trade", "binance", "BTCUSDT", None, &cfg));

    // Generous, because it bounds a real merge on a loaded CI box — but it is a FAILURE bound, not
    // a timing assumption: the assertions below start only once the compactor has said it is here.
    let arrived = parked_rx.recv_timeout(Duration::from_secs(60));
    assert!(
        arrived.is_ok(),
        "the compactor never reached the post-merge window — it merged nothing, failed, or the \
         pause seam is gone (in which case this test is back to racing for the overlap)"
    );

    // The appends run HERE, with a compaction demonstrably in flight and its merge output already
    // on disk. Under the pre-fix design this is where the loss happened: the compactor would park
    // holding the series lock, and this call would spin ~4 s and return `timeout acquiring series
    // lock` — on which a real RecorderSink flush discards its buffer, up to `max_rows` rows.
    for i in 0..8i64 {
        df.append_trades("binance", "BTCUSDT", &[tt(9_000 + i, 2.0, 2.0)], Some(&format!("b{i}")))
            .expect("an append blocked on a merge that had already released the lock");
    }
    // Sampled BEFORE the release, so the message can name what the compaction went on to do.
    let finished_while_appending = compactor.is_finished();
    let _ = resume_tx.send(());
    let report = compactor.join().expect("compactor panicked").expect("compaction failed");
    assert!(
        !finished_while_appending,
        "the compaction had already completed before the last append returned, so these rows did \
         not overlap it at all and the counts below prove nothing about concurrency: {report:?}"
    );
    // ...and the publish crossed those appends rather than abandoning the date over them: an
    // abandoned merge leaves all 8 inputs in place, which the 16 rows below would also satisfy.
    assert_eq!(
        report.parts_merged, 8,
        "the merge published across the concurrent appends: {report:?}"
    );

    // Every row survives: the 8 originals (compacted or not) plus the 8 appended concurrently.
    let got = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 16, "no row lost on either side: {report:?}");
    assert_eq!(
        got.iter().filter(|t| t.ts >= 9_000).count(),
        8,
        "the concurrent appends are all here"
    );

    // ...and the store still reads correctly after a reopen (the manifest agrees with the disk).
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(reopened.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 16);
}

/// A BROKEN series must not stop maintenance for the rest of the store.
///
/// Regression for the the CI box outage of 2026-08-02: `run_maintenance` walked series with `?`, so the
/// FIRST series that errored aborted the whole pass — and `MaintenanceScheduler` discarded that
/// error without logging it. Compaction and retention silently stopped store-wide (~270 uncompacted
/// parts in one date, 15+ minutes, zero log lines). The per-kind precedents
/// (`run_maintenance_handles_chain_series`, `..._equity_series`) each patched ONE kind into the
/// dispatch; this pins the general property instead: an unroutable series is isolated, reported,
/// and the healthy series next to it still compacts.
#[test]
fn one_broken_series_does_not_abort_maintenance_for_the_others() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();

    // A HEALTHY series with enough parts to compact.
    let bars: Vec<Bar> =
        (0..4).map(|i| bar(1_700_000_000_000 + i * 60_000, 100.0 + i as f64, None)).collect();
    for (c, b) in bars.iter().enumerate() {
        store
            .append_bars(
                "binance",
                "BTCUSDT",
                "1m",
                std::slice::from_ref(b),
                Some(&format!("k{c}")),
            )
            .unwrap();
    }

    // A BROKEN one: a kind the compaction dispatch cannot route (`unknown series kind`). Built by
    // copying the healthy series' on-disk layout under a bogus `kind=`, so it is a well-formed
    // series in every respect EXCEPT that nothing can compact it — the shape a future unhandled
    // kind takes, which is exactly how this bit in production.
    let good = dir.path().join("kind=bar/venue=binance/symbol=BTCUSDT/interval=1m");
    let bad = dir.path().join("kind=bogus/venue=binance/symbol=BTCUSDT/interval=1m");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    copy_tree(&good, &bad);

    let report = store
        .run_maintenance(&MaintenanceConfig {
            compaction: CompactionConfig { min_parts: 2, ..Default::default() },
            retention: None,
        })
        .expect("a broken series must not make the PASS fail");

    assert_eq!(report.failed.len(), 1, "the broken series is reported, not hidden: {report:?}");
    assert_eq!(report.failed[0].0.kind, "bogus");
    assert!(
        report.failed[0].1.contains("unknown series kind"),
        "the recorded error names the cause: {}",
        report.failed[0].1
    );
    assert!(
        report.compaction.parts_merged > 0,
        "the HEALTHY series must still compact despite the broken neighbour: {report:?}"
    );
    // And the healthy series' data is intact through that pass.
    let got = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(got.len(), 4, "no rows lost while a sibling series was failing");
}

/// Recursive directory copy for the fault-injection above (no external dep).
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// `target_bytes` must BOUND a compaction pass, not merely describe its ambitions.
///
/// the CI box, 2026-08-04: the planner took every part of a `date=` as one merge and decoded them all
/// into Arrow before writing — 3,634 parts, 658 MB of zstd Parquet, in a 4 GB cgroup. The OOM
/// killer took the recorder down twice, and the second kill left a lock behind that wedged every
/// restart for 11 h. `target_bytes` was plumbed from the profile's `target_mb` all the way into
/// `CompactionConfig` and then never read.
#[test]
fn compaction_splits_a_date_into_merges_bounded_by_max_merge_rows() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..8i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let series = bars_series(dir.path());
    assert_eq!(count_parquets(&series), 8, "eight same-day fragments");
    let before = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    // 40 rows across 8 equal parts, with room for 10 rows per merge: several merges, not one.
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, max_merge_rows: 10 };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    assert_eq!(rep.parts_merged, 8, "every fragment was still compacted");
    assert!(
        rep.parts_written > 1,
        "the whole date merged as ONE part despite a row budget a quarter its size — the merge is \
         unbounded, and its peak memory is whatever the biggest date happens to be"
    );
    assert_eq!(rep.rows, 40);
    assert!(count_parquets(&series) < 8, "compaction still reduced the part count");
    assert_bars_bit_eq(&before, &df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap());
}

/// The same date under a budget that comfortably fits it merges as ONE part — a memory bound must
/// not fragment a series it was never meant to constrain.
#[test]
fn compaction_within_the_row_budget_still_merges_a_date_whole() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..8i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        df.append_bars("binance", "BTCUSDT", "1m", &batch, Some(&format!("b{c}"))).unwrap();
    }
    let cfg = CompactionConfig { min_parts: 3, ..Default::default() };
    let rep = df.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 8);
    assert_eq!(rep.parts_written, 1, "40 rows against the 1,000,000-row default is one merge");
}
