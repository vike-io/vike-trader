//! Gate for manifest v3 — the base manifest plus its framed delta log.
//!
//! What these cover, and why each one is here rather than argued in a PR body:
//!
//! - a crash mid-append (a TORN FRAME, planted rather than reasoned about) leaves a readable store;
//! - a payload corrupted in place is rejected by its CRC, which is the property `wal.rs`'s framing
//!   does NOT have and the reason this log carries one;
//! - a manifest that is not v3 (v1, v2) is refused with the repair, and the repair rebuilds it;
//! - the IDEMPOTENCY guard still refuses a duplicate commit — across a fold, across a reopen, and
//!   for an orphan key (spent with no part). This is the correctness property the whole commit log
//!   exists for and the one a delta log could quietly weaken;
//! - ATOMICITY: a reader never sees a manifest naming a part that is not there, nor misses one that
//!   is — asserted end to end, because a manifest naming a missing part fails to OPEN it and a
//!   manifest missing a part returns short;
//! - the bytes written per commit, measured, so the improvement is a number in CI rather than a
//!   claim.
//!
//! Only compiled/run with `--features hist-datafusion`.
#![cfg(feature = "hist-datafusion")]

use std::path::Path;

use vike_data::{DataFusionHist, HistStore, RetentionPolicy, TsRange};
use vike_model::Bar;

use crate::common::{bars_id, bars_series};

const MANIFEST: &str = "_manifest.json";
const DELTA: &str = "_manifest.delta";

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close - 0.5,
        high: close + 1.0,
        low: close - 1.5,
        close,
        volume: 10.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// One keyed append. Every ts lands in the same UTC day so each commit seals exactly one part,
/// which keeps the arithmetic in the measurement test below honest.
fn commit(store: &DataFusionHist, n: i64) {
    let ts = 1_700_000_000_000i64 + n * 60_000;
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(ts, 100.0 + n as f64)], Some(&key(n)))
        .expect("append");
}

fn key(n: i64) -> String {
    format!("live-binance-BTCUSDT-bar-{n}")
}

fn len_of(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Read a series' base manifest as JSON.
fn base_json(series: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(series.join(MANIFEST)).unwrap()).unwrap()
}

/// **The atomicity assertion, end to end.** A manifest naming a part that is not on disk fails to
/// OPEN that part, so the scan errors; a manifest that MISSES a part that is on disk returns short.
/// Both halves of "a reader never sees an inconsistent manifest" are therefore covered by asserting
/// that a full-range scan succeeds and yields exactly the rows that were committed.
///
/// Deliberately not a structural walk of the `files` array: that would need this test to re-derive
/// the base+delta fold, i.e. to reimplement the code under test and agree with its bugs.
fn assert_reads_exactly(store: &DataFusionHist, rows: usize) {
    let got = store
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .expect("a manifest naming a part that is not there cannot be opened — this is the assert");
    assert_eq!(
        got.len(),
        rows,
        "the manifest indexed a different number of rows than were committed"
    );
}

// ---------------------------------------------------------------------------------------------
// Crash safety: torn and corrupted frames
// ---------------------------------------------------------------------------------------------

/// A crash mid-append leaves a TORN frame. Proven by planting one rather than by reasoning about
/// it: the log is truncated inside its final frame's payload and then garbage is appended, which is
/// the shape a filesystem can leave when a header's page reaches disk and its payload's does not.
///
/// The property: every commit BEFORE the torn one is intact and readable, and the torn one simply
/// did not happen — which is correct, because a frame that never reached its fsync is a commit
/// whose caller never returned.
#[test]
fn a_torn_final_frame_is_dropped_and_the_store_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for n in 0..6 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    let log = series.join(DELTA);
    let whole = std::fs::read(&log).unwrap();
    assert!(whole.len() > 32, "there must be a log to tear: {} bytes", whole.len());
    drop(store);

    // Tear the tail: keep all but the last 40 bytes, then append plausible-looking garbage so the
    // reader is rejecting a frame rather than merely hitting EOF.
    let mut torn = whole[..whole.len() - 40].to_vec();
    torn.extend_from_slice(b"VMDF\x10\x00\x00\x00");
    std::fs::write(&log, &torn).unwrap();

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let rows = reopened.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert!(!rows.is_empty(), "a torn tail must not make the whole series unreadable");
    assert!(rows.len() < 6, "the torn frame's commit must not be counted as published");
    // And what survived is a CONSISTENT manifest: every part it names opens.
    assert_reads_exactly(&reopened, rows.len());

    // ⚠ THE PART THAT IS NOT OBVIOUS, and that a first version of this design got wrong: the store
    // must keep working AFTERWARDS. The log is opened in APPEND mode, so a frame written past a
    // torn tail lands where the reader — which stops AT the tear — will never reach it, and so does
    // every frame after that until the next fold. The store would go on accepting commits and
    // silently losing them from the instant of one crash. `publish` cuts the tail off first.
    commit(&reopened, 99);
    assert_eq!(
        reopened.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(),
        rows.len() + 1,
        "a commit after a torn tail must be readable — if this fails, appends are landing past the \
         tear and are invisible"
    );
    // The repair is a TRUNCATION, not an append-over: the 8 bytes of planted garbage are gone, so
    // the log is the intact prefix plus exactly one new frame rather than that plus the garbage.
    assert!(
        len_of(&series.join(DELTA)) < torn.len() as u64 + 400,
        "the torn tail must have been cut, not written past"
    );
    // ...and it is still durable across a reopen, which is the path that re-reads and replays.
    let again = DataFusionHist::open(dir.path()).unwrap();
    assert_reads_exactly(&again, rows.len() + 1);
}

/// The CRC's own reason for existing, and the one place this framing is stricter than `wal.rs`'s.
///
/// A crash can leave a frame whose LENGTH prefix is intact and whose payload is partly stale block
/// contents — structurally complete, semantically wrong. Here that is planted by flipping bytes
/// INSIDE the last frame's payload without touching its header, so a length-only reader would
/// accept it. Without the CRC this is the case that reaches `serde_json` and may parse.
#[test]
fn a_payload_corrupted_in_place_is_rejected_by_its_crc() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for n in 0..5 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    let log = series.join(DELTA);
    let mut bytes = std::fs::read(&log).unwrap();
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len();
    drop(store);

    // Corrupt the last byte of the file — inside the final frame's payload, header untouched.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&log, &bytes).unwrap();

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let after = reopened.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len();
    assert_eq!(
        after,
        before - 1,
        "exactly the corrupted frame must be dropped — {before} committed, {after} readable"
    );
    assert_reads_exactly(&reopened, after);
}

/// A base and its log are ONE object; half of one is corruption, not a degraded read.
///
/// Replaying a log over an absent base would succeed and index only the parts committed since the
/// last fold — a scan of the older window would then return fewer rows with NO error, which is the
/// one outcome nothing downstream can detect. So it is refused, and the error names the repair.
#[test]
fn a_base_less_series_with_a_surviving_log_is_refused_not_half_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for n in 0..4 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    assert!(series.join(DELTA).is_file(), "the log must exist for this test to mean anything");
    drop(store);
    std::fs::remove_file(series.join(MANIFEST)).unwrap();

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let err = reopened
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .expect_err("a log with no base must be refused, not partially replayed");
    let text = format!("{err:?}");
    assert!(text.contains("delta log but NO base"), "the error must say what is wrong: {text}");
    assert!(text.contains("rebuild_series_manifest"), "the error must name the repair: {text}");

    // ...and the repair works: a rebuild reads the parts themselves.
    reopened.rebuild_series_manifest(&bars_id()).unwrap();
    assert_reads_exactly(&reopened, 4);
}

// ---------------------------------------------------------------------------------------------
// Folding
// ---------------------------------------------------------------------------------------------

/// A fold collapses the log into a new base, and a fold that is KILLED between publishing the base
/// and clearing the log must be a no-op rather than a doubling.
///
/// The second half is the reason `DeltaFrame::version` exists. Replay applies only frames above the
/// base's version, so a surviving log whose frames the base already absorbed changes nothing. If
/// that guard were dropped, each such frame would re-add its `FileEntry` — and a duplicated entry
/// is a part READ TWICE, i.e. silently doubled rows.
#[test]
fn a_fold_collapses_the_log_and_a_crashed_fold_does_not_double_apply() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = bars_series(dir.path());
    // Small enough that a handful of commits crosses it; production is 4 MiB, which no test could
    // reach at this cadence.
    store.set_fold_bytes_for_test(700);

    for n in 0..8 {
        commit(&store, n);
    }
    assert!(
        len_of(&series.join(DELTA)) < 700,
        "the log must have folded at least once, not grown unbounded"
    );
    assert_reads_exactly(&store, 8);
    // A fold absorbs the frames written UP TO IT, not every frame ever — the commits since the last
    // fold are still in the log, which is the whole point. So the proof that folding happened is
    // that the base has moved well past the version its first commit left it at.
    let folded_version = base_json(&series)["version"].as_u64().unwrap();
    assert!(folded_version > 1, "no fold happened — the base is still at version {folded_version}");
    assert!(folded_version < 8, "a fold absorbs what came before it, not the commits after it");

    // The crashed fold: take the log as it is, let a fold happen, then put the pre-fold log back —
    // which is exactly the on-disk state of a process killed after the base rename and before the
    // unlink.
    let pre_fold_log = std::fs::read(series.join(DELTA)).unwrap_or_default();
    commit(&store, 8);
    commit(&store, 9);
    let mut resurrected = pre_fold_log;
    resurrected.extend_from_slice(&std::fs::read(series.join(DELTA)).unwrap_or_default());
    std::fs::write(series.join(DELTA), &resurrected).unwrap();

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    assert_reads_exactly(&reopened, 10);
    // And committing on top of that state still works and still refuses a duplicate.
    assert_eq!(
        reopened.append_bars("binance", "BTCUSDT", "1m", &[bar(1, 1.0)], Some(&key(3))).unwrap(),
        0,
        "a resurrected log must not have unpublished an already-committed key"
    );
}

// ---------------------------------------------------------------------------------------------
// Idempotency — the property the commit log exists for
// ---------------------------------------------------------------------------------------------

/// The guard, exercised through every state v3 introduced: a key committed into the delta log, a
/// key that has since been FOLDED into the base, and both after a reopen (which re-reads the base
/// and replays the log rather than holding anything in memory).
///
/// This is the test to mutate against production code. Weakening `Manifest::has_commit` to `false`
/// must redden it, and redden it for duplicate rows rather than for a count that happens to differ.
#[test]
fn the_idempotency_guard_still_refuses_a_duplicate_commit() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(700);
    for n in 0..8 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    assert!(len_of(&series.join(DELTA)) < 700, "at least one fold must have happened");

    // A key from BEFORE the fold — it now lives only in the base.
    assert_eq!(
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(5, 5.0)], Some(&key(0))).unwrap(),
        0,
        "a key folded into the base must still be refused"
    );
    // A key from AFTER the fold — it lives only in an unfolded delta frame.
    assert_eq!(
        store.append_bars("binance", "BTCUSDT", "1m", &[bar(5, 5.0)], Some(&key(7))).unwrap(),
        0,
        "a key that is still only in the delta log must be refused"
    );
    assert_reads_exactly(&store, 8);

    // ...and across a reopen, which is the path that actually replays the log.
    drop(store);
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    for n in 0..8 {
        assert_eq!(
            reopened
                .append_bars("binance", "BTCUSDT", "1m", &[bar(5, 5.0)], Some(&key(n)))
                .unwrap(),
            0,
            "key {n} must still be refused after a reopen"
        );
    }
    assert_reads_exactly(&reopened, 8);
}

/// Retention drops a part's commit keys WITH the part, which is the behaviour v2 spent an
/// `O(total keys)`-per-dropped-key scan inside the series lock to produce. v3 gets it by
/// construction, because a key's home IS the part — so the subtlety that scan existed for has to be
/// checked rather than assumed: a re-backfill of a pruned window must APPEND rather than no-op.
#[test]
fn retention_drops_a_parts_commit_keys_with_the_part() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let day = 86_400_000i64;
    let old = 1_700_000_000_000i64;
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(old, 1.0)], Some("old-window")).unwrap();
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(old + 30 * day, 2.0)], Some("new-window"))
        .unwrap();
    assert_eq!(store.series_commits(&bars_id()).unwrap().len(), 2);

    // Prune everything older than the recent day.
    let report = store
        .apply_retention(
            "bar",
            "binance",
            "BTCUSDT",
            Some("1m"),
            &RetentionPolicy { before_ts: Some(old + day), max_age_ms: None },
        )
        .unwrap();
    assert!(report.files_dropped >= 1, "the old part must have been pruned: {report:?}");

    let left = store.series_commits(&bars_id()).unwrap();
    assert!(
        !left.iter().any(|k| k == "old-window"),
        "a pruned part's key must go with it: {left:?}"
    );
    assert!(left.iter().any(|k| k == "new-window"), "a surviving part keeps its key: {left:?}");
    // The point of GCing the key: re-backfilling the pruned window must WRITE, not silently no-op.
    assert_eq!(
        store
            .append_bars("binance", "BTCUSDT", "1m", &[bar(old, 1.0)], Some("old-window"))
            .unwrap(),
        1,
        "a re-backfill of a pruned window must append again"
    );
}

/// Plant a base whose `format` tag is `format`, drop the delta log, and return the error a read of
/// the series gives.
fn read_error_for_format(format: u64) -> String {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    commit(&store, 0);
    let series = bars_series(dir.path());
    drop(store);
    let mut v = base_json(&series);
    v["format"] = serde_json::json!(format);
    std::fs::write(series.join(MANIFEST), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let _ = std::fs::remove_file(series.join(DELTA));

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let err = reopened
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .expect_err("a manifest that is not v3 is not read");
    format!("{err:?}")
}

/// A v1 manifest is refused, and the refusal names the format it found and the repair.
#[test]
fn a_v1_manifest_is_refused_with_a_message_that_names_the_repair() {
    let text = read_error_for_format(1);
    assert!(text.contains("format v1"), "the error must name the format it found: {text}");
    assert!(text.contains("hist repair"), "the error must name the repair: {text}");
}

/// A v2 manifest is refused like any other non-v3 file: this build reads v3 only, and the message
/// says to rebuild the manifest from the parts. The rebuild then makes the series readable again.
#[test]
fn a_v2_manifest_is_refused_and_the_repair_it_names_rebuilds_the_series() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for n in 0..3 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    drop(store);
    let mut v = base_json(&series);
    v["format"] = serde_json::json!(2);
    v["commits"] = serde_json::json!([key(0), key(1), key(2)]);
    std::fs::write(series.join(MANIFEST), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let _ = std::fs::remove_file(series.join(DELTA));

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let err = reopened
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .expect_err("a v2 manifest is not read");
    let text = format!("{err:?}");
    assert!(text.contains("format v2"), "the error must name the format it found: {text}");
    assert!(text.contains("reads only v3"), "the error must say what this build reads: {text}");
    assert!(text.contains("hist repair"), "the error must name the repair: {text}");

    reopened.rebuild_series_manifest(&bars_id()).unwrap();
    assert_reads_exactly(&reopened, 3);
}

// ---------------------------------------------------------------------------------------------
// Keys spent WITHOUT rows — the empty-day marker (`spend_keys_without_rows`, the first producer of
// a frame's `keys_add`). docs/superpowers/specs/2026-10-02-oanda-empty-days-design.md §7, PR 1.
// ---------------------------------------------------------------------------------------------

/// An empty-day marker key, in the shape the day-chunked ingest builds: the day's own key plus
/// `vike_data::store::store_kind::EMPTY_MARKER_SUFFIX`.
fn marker(n: i64) -> String {
    format!("binance:BTCUSDT:1m:{n}-{n}{}", vike_data::store::store_kind::EMPTY_MARKER_SUFFIX)
}

/// Every parquet part under a series leaf, by walking the `date=` directories — the filesystem's
/// answer rather than the manifest's, so "no part was added" is not asked of the code under test.
fn parquet_files(series: &Path) -> usize {
    let Ok(dates) = std::fs::read_dir(series) else { return 0 };
    dates
        .filter_map(|d| d.ok().map(|d| d.path()))
        .filter(|p| p.is_dir())
        .flat_map(|d| std::fs::read_dir(d).unwrap().filter_map(|e| e.ok().map(|e| e.path())))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
        .count()
}

/// The base's orphan list, verbatim.
fn base_orphans(series: &Path) -> Vec<String> {
    base_json(series)["orphan_commits"]
        .as_array()
        .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// **A key spent without rows is SPENT, and is nothing else.** The idempotency read sees it — and
/// the two-key read answers it beside a day key from one manifest read — while every surface that
/// reads PARTS (coverage, rows, the files on disk) is byte-for-byte what it was before.
#[test]
fn a_key_spent_without_rows_is_spent_and_adds_no_part() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    commit(&store, 0);
    commit(&store, 1);
    let series = bars_series(dir.path());
    let coverage = store.series_coverage(&bars_id()).unwrap();
    let rows = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    let parts = parquet_files(&series);
    let m = marker(7);
    assert!(!store.series_has_commit(&bars_id(), &m).unwrap(), "not spent before the call");

    assert_eq!(store.spend_keys_without_rows(&bars_id(), &[&m]).unwrap(), 1);

    assert!(store.series_has_commit(&bars_id(), &m).unwrap(), "the marker must read as spent");
    assert_eq!(
        store.series_has_commits(&bars_id(), &[&key(0), &m, "never-spent"]).unwrap(),
        vec![true, true, false],
        "the multi-key read answers each key in order, a part's key and a rowless one alike"
    );
    assert!(store.series_commits(&bars_id()).unwrap().contains(&m));
    assert_eq!(store.series_coverage(&bars_id()).unwrap(), coverage, "coverage reads parts only");
    assert_eq!(store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(), rows);
    assert_eq!(parquet_files(&series), parts, "a rowless key must write no part");
    assert_reads_exactly(&store, 2);
}

/// A rowless key has no part to ride on, so its only home is the orphan list — and that list must
/// carry it through a FOLD (the base is rewritten whole from the in-memory manifest) and a REOPEN
/// (the base is all there is). Lose it at either and the marked day is asked again forever.
#[test]
fn a_rowless_key_survives_a_fold_and_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let series = bars_series(dir.path());
    let m = marker(1);
    {
        let store = DataFusionHist::open(dir.path()).unwrap();
        commit(&store, 0);
        store.set_fold_bytes_for_test(1); // every publish folds
        assert_eq!(store.spend_keys_without_rows(&bars_id(), &[&m]).unwrap(), 1);
        assert_eq!(len_of(&series.join(DELTA)), 0, "the spend's own publish folded the log away");
        assert_eq!(base_orphans(&series), vec![m.clone()], "the fold wrote it into the base");
        // ...and an ordinary commit's fold after it does not drop it either.
        commit(&store, 2);
        assert_eq!(base_orphans(&series), vec![m.clone()]);
    }
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    assert!(reopened.series_has_commit(&bars_id(), &m).unwrap(), "spent across a reopen");
    assert_reads_exactly(&reopened, 2);
}

/// Spending is idempotent in the same sense an append is: a key already spent — as a marker, OR
/// on a part's own key list — is dropped before the publish, a repeat inside one call counts once,
/// and a call left with nothing to spend publishes NOTHING (no frame, so no growth in the log).
#[test]
fn spending_twice_or_spending_a_carried_key_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    commit(&store, 0);
    let series = bars_series(dir.path());
    let m = marker(1);
    assert_eq!(store.spend_keys_without_rows(&bars_id(), &[&m, &m]).unwrap(), 1, "once");
    let log = len_of(&series.join(DELTA));
    assert!(log > 0, "the first spend published a frame");

    assert_eq!(store.spend_keys_without_rows(&bars_id(), &[&m]).unwrap(), 0);
    assert_eq!(len_of(&series.join(DELTA)), log, "an already-spent key must publish no frame");

    // A key a PART carries is spent too, and is not spent a second time as an orphan.
    let carried = marker(5);
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(1_700_000_300_000, 1.0)], Some(&carried))
        .unwrap();
    let log = len_of(&series.join(DELTA));
    assert_eq!(store.spend_keys_without_rows(&bars_id(), &[&carried]).unwrap(), 0);
    assert_eq!(len_of(&series.join(DELTA)), log, "a part-carried key must publish no frame");
    assert_eq!(store.spend_keys_without_rows(&bars_id(), &[]).unwrap(), 0, "nothing to spend");
    assert_eq!(len_of(&series.join(DELTA)), log);

    // No duplicate anywhere: the folded base holds the one marker once, and the part-carried key
    // is NOT an orphan.
    store.set_fold_bytes_for_test(1);
    commit(&store, 2);
    assert_eq!(base_orphans(&series), vec![m.clone()]);
    let all = store.series_commits(&bars_id()).unwrap();
    assert_eq!(all.iter().filter(|k| **k == m).count(), 1, "{all:?}");
    assert_eq!(all.iter().filter(|k| **k == carried).count(), 1, "{all:?}");
}

/// **The repair plan counts markers as a NOTE, and counts the ones still in the LOG.** A rebuild
/// derives keys from part footers, so it drops every marker; that costs a request per marker and
/// never a row, so the verdict must stay LOSSLESS — while a v2-residue orphan beside them is still
/// the LOSS it always was. One marker is folded into the base and one lives only in the delta log,
/// because a plan that read the base alone would miss every marker spent since the last fold.
#[test]
fn the_repair_plan_counts_markers_as_a_note_from_the_log_too() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    commit(&store, 0);
    let series = bars_series(dir.path());
    store.set_fold_bytes_for_test(1);
    store.spend_keys_without_rows(&bars_id(), &[&marker(1)]).unwrap();
    store.set_fold_bytes_for_test(u64::MAX);
    store.spend_keys_without_rows(&bars_id(), &[&marker(2)]).unwrap();
    assert_eq!(base_orphans(&series), vec![marker(1)], "one marker folded into the base…");
    assert!(len_of(&series.join(DELTA)) > 0, "…and one only in the log");

    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    assert_eq!(plan.empty_markers, 2, "the base's AND the log's: {plan:?}");
    assert_eq!(plan.orphan_commits, 0, "a marker is not the LOSSY orphan: {plan:?}");
    assert!(plan.is_lossless(), "{:?}", plan.losses());
    let text = plan.lines().join("\n");
    assert!(!text.contains("LOSSY"), "{text}");
    assert!(text.contains("2 empty-day marker(s) dropped"), "{text}");
    assert!(plan.notes().join("\n").contains("Not a loss"), "{:?}", plan.notes());

    // A v2-residue orphan BESIDE them is still the loss it always was — counted apart.
    let mut doc = base_json(&series);
    doc["orphan_commits"].as_array_mut().unwrap().push("live-2026-08-02:1".into());
    std::fs::write(series.join(MANIFEST), serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    let plan = store.plan_series_manifest_rebuild(&bars_id()).unwrap();
    assert_eq!((plan.empty_markers, plan.orphan_commits), (2, 1), "{plan:?}");
    assert_eq!(plan.losses().len(), 1, "{:?}", plan.losses());

    // And the rebuild does what the plan said: the markers are gone, the rows are not.
    store.rebuild_series_manifest_if_uncontended(&bars_id()).unwrap().unwrap();
    assert!(!store.series_has_commit(&bars_id(), &marker(2)).unwrap(), "a rebuild drops markers");
    assert_reads_exactly(&store, 1);
}

// ---------------------------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------------------------

/// **The improvement as a number in CI, not a claim in a PR body.**
///
/// The BEFORE is measured, not modelled. A fold threshold of 1 byte makes every publish fold, which
/// is precisely v2's behaviour — a whole-file manifest write per commit — so the cost of that
/// regime can be summed directly off the file the store writes. The AFTER is the same store, same
/// series, same commits, at the production threshold.
///
/// The v2 figure is still a deliberate UNDER-estimate in one respect: a real v2 file also carried
/// every commit key a SECOND time in a top-level array, which MEASURED at 99.0% of a 104 MB file on
/// the live box. This baseline carries them once. A conservative baseline is the right one under a
/// ratio assertion — it can only understate the win.
#[test]
fn bytes_written_per_commit_collapses_against_the_whole_file_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = bars_series(dir.path());
    // Sized from this test's own numbers, MEASURED 2026-10-03 at 150/100 (the latency box lane): v2 45,255
    // B/commit vs v3 193 (the floor is 20x; it held 234x), a 33,842 B base after warm-up (the floor
    // is 4,000), and with the 8 KiB threshold 3 folds at 2,856 B/commit against a 98,704 B base (the
    // floor is 2x; it held 17x). At 40/40 the base after warm-up is ~9 KB and v2 ~10 KB/commit —
    // every floor still clears by more than 2x — and the three windows commit 160 times, not 450.
    // The window-3 fold count stays above zero because window 2's log already carries ~7.7 KB.
    const WARM: i64 = 40;
    const WINDOW: i64 = 40;

    // ---- BEFORE: fold on every publish == v2's whole-file rewrite, measured commit by commit ----
    store.set_fold_bytes_for_test(1);
    for n in 0..WARM {
        commit(&store, n);
    }
    let base_at_start = len_of(&series.join(MANIFEST));
    assert!(base_at_start > 4_000, "the base must be big enough to measure: {base_at_start} bytes");
    let mut v2_bytes = 0u64;
    for n in WARM..WARM + WINDOW {
        commit(&store, n);
        v2_bytes += len_of(&series.join(MANIFEST));
    }
    assert_eq!(len_of(&series.join(DELTA)), 0, "folding every publish leaves no log");
    let base_after_warm = len_of(&series.join(MANIFEST));

    // ---- AFTER: the production threshold, on the very same store ----
    store.set_fold_bytes_for_test(4 * 1024 * 1024);
    let mut v3_bytes = 0u64;
    let mut folds = 0u32;
    let mut last = len_of(&series.join(DELTA));
    for n in WARM + WINDOW..WARM + 2 * WINDOW {
        commit(&store, n);
        let now = len_of(&series.join(DELTA));
        if now < last {
            // A fold: the base was rewritten whole and the log truncated. The shrink is how a
            // file-size walk sees a rewrite that barely changes the size.
            folds += 1;
            v3_bytes += len_of(&series.join(MANIFEST)) + now;
        } else {
            v3_bytes += now - last;
        }
        last = now;
    }

    let v3_per_commit = v3_bytes / WINDOW as u64;
    let v2_per_commit = v2_bytes / WINDOW as u64;
    println!(
        "MANIFEST BYTES PER COMMIT, {WINDOW} commits each, same series:\n  \
         v2 (whole-file rewrite, measured) = {v2_per_commit} B/commit\n  \
         v3 (base+delta, production 4 MiB fold) = {v3_per_commit} B/commit, {folds} folds\n  \
         base grew {base_at_start} B -> {base_after_warm} B over the v2 window"
    );

    assert_eq!(
        folds, 0,
        "{WINDOW} tiny commits must not reach the 4 MiB production fold threshold"
    );
    assert!(
        v3_per_commit * 20 < v2_per_commit,
        "v3 must cost at least 20x less per commit than the whole-file rewrite it replaces: \
         v3 {v3_per_commit} B vs v2 {v2_per_commit} B"
    );
    // The absolute claim, not just the ratio. A one-part append's frame is a few hundred bytes AND
    // — the property that actually matters — it does not grow with the series' history, which is
    // what turns a cost that was quadratic in commits into one that is linear.
    assert!(
        v3_per_commit < 400,
        "a one-part append must cost hundreds of bytes, not {v3_per_commit}"
    );
    assert_reads_exactly(&store, (WARM + 2 * WINDOW) as usize);

    // ---- ...and the amortised cost INCLUDING folds, which is what a long-running store pays ----
    // Driven at a small threshold because the production one is ~16,000 commits away.
    store.set_fold_bytes_for_test(8 * 1024);
    let mut folded_bytes = 0u64;
    let mut folded_folds = 0u32;
    let mut last = len_of(&series.join(DELTA));
    for n in WARM + 2 * WINDOW..WARM + 3 * WINDOW {
        commit(&store, n);
        let now = len_of(&series.join(DELTA));
        if now < last {
            folded_folds += 1;
            folded_bytes += len_of(&series.join(MANIFEST)) + now;
        } else {
            folded_bytes += now - last;
        }
        last = now;
    }
    let amortised = folded_bytes / WINDOW as u64;
    let base_now = len_of(&series.join(MANIFEST));
    println!(
        "amortised with an 8 KiB fold threshold ({folded_folds} folds over {WINDOW} commits): \
         {amortised} B/commit against a {base_now} B base"
    );
    assert!(folded_folds > 0, "the small threshold must actually have folded");
    assert!(
        amortised * 2 < base_now,
        "even at an 8 KiB threshold — a thousandth of production's — the amortised cost \
         ({amortised} B) must comfortably beat rewriting the {base_now} B base every commit"
    );
    assert_reads_exactly(&store, (WARM + 3 * WINDOW) as usize);
}
