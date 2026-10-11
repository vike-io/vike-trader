//! The bulk/offline write profile (`BulkIngestSession`).

use std::path::{Path, PathBuf};

use vike_data::{BulkConfig, DataFusionHist, HistStore, TsRange};

use crate::common::tt;

// ---- slice 8: the bulk/offline write profile (BulkIngestSession) -----------------------------
//
// The live-path gates above (bar/quote/trade/book round trips, WAL crash-recovery, maintenance)
// are ALL untouched by this profile's existence — these tests are additive proof that (a) the
// bulk profile actually batches/flushes on its configured window, (b) it stays idempotent across
// a simulated re-run, (c) a crash mid-flush leaves the store readable and safely re-runnable
// (never half-written data that reads as valid), and (d) it genuinely never writes the WAL the
// live profile still does — the one behavior that must NOT have changed.

/// The `kind=trade` series leaf dir for `(venue, symbol)` — no `interval=` segment (ticks_dir's
/// shape). Used to inspect on-disk artifacts (`_wal.arrow` presence) the public API doesn't expose.
fn trade_series_dir(root: &Path, venue: &str, symbol: &str) -> PathBuf {
    root.join("kind=trade").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
}

#[test]
fn bulk_flushes_only_at_the_configured_batch_boundary() {
    // max_batches=3 (rows/bytes set high enough to never trigger first): staging + end_batch
    // twice must NOT flush; the third end_batch call must.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let cfg = BulkConfig { max_batches: 3, max_rows: 1_000_000, max_bytes: 1 << 30 };
    let mut session = df.bulk_session(cfg);

    for i in 0..2i64 {
        session.stage_trades("binance", "BTCUSDT", &[tt(i * 1000, 100.0 + i as f64, 1.0)]);
        let report = session.end_batch("test").unwrap();
        assert_eq!(report, Default::default(), "batch {i}: below the window, no flush yet");
    }
    assert!(
        df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().is_empty(),
        "nothing durable before the window closes"
    );

    session.stage_trades("binance", "BTCUSDT", &[tt(2000, 102.0, 1.0)]);
    let report = session.end_batch("test").unwrap();
    assert_eq!(report.series_flushed, 1, "the 3rd end_batch crosses max_batches=3 and flushes");
    assert_eq!(report.rows_written, 3);
    assert_eq!(session.total_commits(), 1);
    assert_eq!(session.total_rows_written(), 3);

    let got = df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "all 3 staged rows landed in ONE commit, not three");
}

#[test]
fn bulk_reflush_under_the_same_prefix_is_idempotent() {
    // A fresh session's window index always starts at 0, so replaying the SAME backfill
    // invocation (same key_prefix, same staged rows, same order) from a brand-new session — the
    // shape of "the process restarted and reran the same command" — lands on the identical commit
    // key and must not duplicate rows.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let rows = [tt(0, 1.0, 1.0), tt(1000, 1.1, 1.0), tt(2000, 1.2, 1.0)];

    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &rows);
        let report = session.flush("vikearchive:trade:2026-07-26:bulk").unwrap();
        assert_eq!(report.rows_written, 3);
    }
    assert_eq!(df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(), 3);

    // "re-run": a brand-new session, the identical rows staged again, the identical prefix.
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &rows);
        let report = session.flush("vikearchive:trade:2026-07-26:bulk").unwrap();
        assert_eq!(report.rows_written, 0, "same window key already durable — no-op, not a dup");
    }
    assert_eq!(
        df.scan_trades("binance", "BTCUSDT", TsRange::all()).unwrap().len(),
        3,
        "re-running the identical bulk invocation must not duplicate rows"
    );
}

#[test]
fn bulk_crash_mid_flush_leaves_the_store_readable_and_safely_rerunnable() {
    // Series A flushes normally (durable). Series B's flush is interrupted right after its part is
    // sealed but before the manifest publishes (the test-only crash-injection switch this module
    // shares with the live path's WAL-recovery tests) — simulating a process death mid-batch.
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let a_rows = [tt(0, 1.0, 1.0), tt(1000, 1.1, 1.0)];
    let b_rows = [tt(0, 2.0, 1.0), tt(1000, 2.1, 1.0), tt(2000, 2.2, 1.0)];

    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_A", &a_rows);
        let report = session.flush("p").unwrap();
        assert_eq!(report.rows_written, 2, "A commits durably before the simulated crash");
    }
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2);

    df.set_skip_publish_for_test(true);
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_B", &b_rows);
        let report = session.flush("p").unwrap(); // seals B's part, but the manifest never publishes
        assert_eq!(report.rows_written, 3, "seal_into_manifest still reports rows sealed");
    }
    df.set_skip_publish_for_test(false);

    // The "crash": B's rows are simply ABSENT (never partially/corruptly visible) — obviously
    // incomplete, not a landmine that later reads as valid data.
    assert!(
        df.scan_trades("binance", "SYM_B", TsRange::all()).unwrap().is_empty(),
        "an unpublished bulk flush must be invisible, exactly like the live path's WAL window"
    );
    // A is completely unaffected by B's crashed flush.
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2);

    // The "re-run": identical invocation (same rows, same prefix) from a fresh session recovers B
    // — and must not duplicate anything, for A or B.
    {
        let mut session = df.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "SYM_B", &b_rows);
        let report = session.flush("p").unwrap();
        assert_eq!(report.rows_written, 3, "the redo commits exactly once — no orphan duplication");
    }
    assert_eq!(df.scan_trades("binance", "SYM_B", TsRange::all()).unwrap().len(), 3);
    assert_eq!(df.scan_trades("binance", "SYM_A", TsRange::all()).unwrap().len(), 2, "A untouched");
}

#[test]
fn live_profile_still_writes_a_wal_record_but_bulk_never_does() {
    // The differentiator this whole module trades away: reusing the SAME test-only crash-injection
    // switch (`skip_publish_for_test`) on each profile and checking for `_wal.arrow`'s presence on
    // disk proves the live path is byte-for-byte unchanged (still WAL-then-seal-then-publish) while
    // the bulk path genuinely never appends to a WAL at all (not merely "doesn't need to replay
    // one" — the file is never created).
    let live_dir = tempfile::tempdir().unwrap();
    let live = DataFusionHist::open(live_dir.path()).unwrap();
    live.set_skip_publish_for_test(true);
    live.append_trades("binance", "BTCUSDT", &[tt(0, 1.0, 1.0)], Some("live-batch")).unwrap();
    let live_wal = trade_series_dir(live_dir.path(), "binance", "BTCUSDT").join("_wal.arrow");
    assert!(live_wal.exists(), "the LIVE profile must still WAL-append before a manifest publish");

    let bulk_dir = tempfile::tempdir().unwrap();
    let bulk = DataFusionHist::open(bulk_dir.path()).unwrap();
    bulk.set_skip_publish_for_test(true);
    {
        let mut session = bulk.bulk_session(BulkConfig::default());
        session.stage_trades("binance", "BTCUSDT", &[tt(0, 1.0, 1.0)]);
        session.flush("p").unwrap();
    }
    let bulk_wal = trade_series_dir(bulk_dir.path(), "binance", "BTCUSDT").join("_wal.arrow");
    assert!(!bulk_wal.exists(), "the BULK profile must never write a WAL record at all");
}
