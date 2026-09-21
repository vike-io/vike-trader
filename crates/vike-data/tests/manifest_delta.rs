//! Gate for manifest v3 — the base manifest plus its framed delta log.
//!
//! What these cover, and why each one is here rather than argued in a PR body:
//!
//! - a crash mid-append (a TORN FRAME, planted rather than reasoned about) leaves a readable store;
//! - a payload corrupted in place is rejected by its CRC, which is the property `wal.rs`'s framing
//!   does NOT have and the reason this log carries one;
//! - an OLD-FORMAT (v2) store reads correctly, migrates on its first write, and can be rolled back;
//! - the IDEMPOTENCY guard still refuses a duplicate commit — across a fold, across a reopen, and
//!   for a migrated store's orphan keys. This is the correctness property the whole commit log
//!   exists for and the one a delta log could quietly weaken;
//! - ATOMICITY: a reader never sees a manifest naming a part that is not there, nor misses one that
//!   is — asserted end to end, because a manifest naming a missing part fails to OPEN it and a
//!   manifest missing a part returns short;
//! - the bytes written per commit, measured, so the improvement is a number in CI rather than a
//!   claim.
//!
//! Only compiled/run with `--features hist-datafusion`.
#![cfg(feature = "hist-datafusion")]

use std::path::{Path, PathBuf};

use vike_data::{DataFusionHist, HistStore, RetentionPolicy, SeriesId, TsRange};
use vike_model::Bar;

const MANIFEST: &str = "_manifest.json";
const DELTA: &str = "_manifest.delta";
const V2_BACKUP: &str = "_manifest.v2.json.bak";

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

fn bars_id() -> SeriesId {
    SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
    }
}

fn bars_series(root: &Path) -> PathBuf {
    root.join("kind=bar").join("venue=binance").join("symbol=BTCUSDT").join("interval=1m")
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

// ---------------------------------------------------------------------------------------------
// The v2 store: reads, migration, rollback
// ---------------------------------------------------------------------------------------------

/// Turn a v3 base into the v2 file the same store would have written, in place, over the SAME real
/// parts — so the migration below is exercised against a genuine legacy manifest rather than a
/// hand-built fixture whose fields might not match what v2 produced.
///
/// `extra_orphans` are keys v2's `commits` array holds that no part carries. A real store had 13 of
/// them (MEASURED on the live box, 2026-09-16) and the record that found them could not fully
/// account for their cause, so carrying them is the behaviour under test.
fn downgrade_base_to_v2(series: &Path, extra_orphans: &[&str]) {
    let mut v: serde_json::Value = base_json(series);
    let mut commits: Vec<String> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["commit_keys"].as_array().unwrap().iter())
        .map(|k| k.as_str().unwrap().to_string())
        .collect();
    commits.dedup();
    commits.extend(extra_orphans.iter().map(|s| s.to_string()));
    v["format"] = serde_json::json!(2);
    v["commits"] = serde_json::json!(commits);
    v.as_object_mut().unwrap().remove("orphan_commits");
    std::fs::write(series.join(MANIFEST), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    // A v2 store has no delta log; leaving one would be a state v2 never produced.
    let _ = std::fs::remove_file(series.join(DELTA));
}

/// An old-format store READS correctly with nothing written, migrates on its first write, keeps a
/// rollback copy, and carries its orphan commit keys across.
///
/// The read half is the load-bearing one. `MANIFEST_FORMAT`'s v2 doc justified a hard break with
/// "the data tree is derived — regenerate";
/// `docs/decisions/0060-the-manifest-rewrite-is-the-write-amplification.md` records that as false
/// on the live box, where 31 GiB of recorded market tape cannot be re-fetched. So a v3 build
/// pointed at a v2 store must simply work.
#[test]
fn a_v2_store_reads_then_migrates_in_place_and_can_be_rolled_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_fold_bytes_for_test(1); // fold every publish, so the base holds everything
    for n in 0..4 {
        commit(&store, n);
    }
    let series = bars_series(dir.path());
    drop(store);
    downgrade_base_to_v2(&series, &["migrate:book:orphaned-by-history"]);
    let v2_bytes = std::fs::read(series.join(MANIFEST)).unwrap();

    // 1. READS, with nothing written.
    let reader = DataFusionHist::open_read_only(dir.path()).unwrap();
    assert_eq!(
        reader.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(),
        4,
        "a v2 store must be readable by a v3 build with no migration step"
    );
    assert_eq!(
        std::fs::read(series.join(MANIFEST)).unwrap(),
        v2_bytes,
        "reading a v2 store must not rewrite it"
    );
    assert!(!series.join(V2_BACKUP).exists(), "a READ must not have migrated anything");
    let mut seen = reader.series_commits(&bars_id()).unwrap();
    seen.sort();
    assert!(
        seen.iter().any(|k| k == "migrate:book:orphaned-by-history"),
        "a v2 key with no surviving part must survive the conversion: {seen:?}"
    );
    drop(reader);

    // 2. The first WRITE migrates in place, and costs exactly one whole-file manifest write.
    let writer = DataFusionHist::open(dir.path()).unwrap();
    commit(&writer, 4);
    assert_eq!(base_json(&series)["format"].as_u64().unwrap(), 3, "the base is v3 now");
    assert!(series.join(V2_BACKUP).is_file(), "the rollback copy must exist");
    assert_eq!(
        std::fs::read(series.join(V2_BACKUP)).unwrap(),
        v2_bytes,
        "the backup must be the v2 file byte for byte"
    );
    assert_eq!(writer.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 5);

    // 3. The orphan key is still refused — the whole reason it was carried rather than derived away.
    assert_eq!(
        writer
            .append_bars(
                "binance",
                "BTCUSDT",
                "1m",
                &[bar(7, 7.0)],
                Some("migrate:book:orphaned-by-history")
            )
            .unwrap(),
        0,
        "an orphan commit key must still refuse its own re-commit after migration"
    );
    // ...and so is a key that DID have a part, proving the derivation itself did not lose anything.
    assert_eq!(
        writer.append_bars("binance", "BTCUSDT", "1m", &[bar(7, 7.0)], Some(&key(2))).unwrap(),
        0,
        "a key carried on its part must still be refused after migration"
    );

    // 4. A second migration does not overwrite the ORIGINAL backup with a newer state.
    commit(&writer, 5);
    assert_eq!(
        std::fs::read(series.join(V2_BACKUP)).unwrap(),
        v2_bytes,
        "the backup is written once"
    );

    // 5. ROLLBACK is the documented two-file move, and it lands on a store a v2 build could read.
    drop(writer);
    std::fs::rename(series.join(V2_BACKUP), series.join(MANIFEST)).unwrap();
    let _ = std::fs::remove_file(series.join(DELTA));
    let rolled: serde_json::Value = base_json(&series);
    assert_eq!(rolled["format"].as_u64().unwrap(), 2, "rollback restores a v2 manifest");
    let back = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(
        back.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(),
        4,
        "rollback returns the store to its state at the migration instant; the parts committed \
         after it are on disk and `rebuild_series_manifest` re-indexes them"
    );
    back.rebuild_series_manifest(&bars_id()).unwrap();
    assert_eq!(
        back.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(),
        6,
        "and the rebuild recovers the post-migration parts too — nothing was lost by rolling back"
    );
}

/// A v1 manifest is still refused, and the refusal says what this build can and cannot convert. The
/// v2 conversion must not have turned the format check into "accept anything".
#[test]
fn a_v1_manifest_is_still_refused_with_a_message_that_names_the_repair() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    commit(&store, 0);
    let series = bars_series(dir.path());
    drop(store);
    let mut v = base_json(&series);
    v["format"] = serde_json::json!(1);
    std::fs::write(series.join(MANIFEST), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let _ = std::fs::remove_file(series.join(DELTA));

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let err = reopened
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .expect_err("v1 is not convertible by anything in this tree");
    let text = format!("{err:?}");
    assert!(text.contains("format v1"), "the error must name the format it found: {text}");
    assert!(text.contains("rebuild"), "the error must name the repair: {text}");
}

/// What the v2 -> v3 migration COSTS, measured against a copy of a real manifest.
///
/// `#[ignore]`d and self-skipping: the case that matters is the live data box's 104 MB
/// `kind=book/venue=polymarket` manifest, and no such file belongs in this repository. Point
/// `VIKE_MANIFEST_MIGRATION_FIXTURE` at a **COPY** of one — this test WRITES where it points — and
/// run it with `--ignored --nocapture`.
///
/// It measures the two things an operator needs before restarting a daemon on a store that cannot
/// be regenerated: how long the series lock is held, and how many bytes reach the device.
#[test]
#[ignore = "needs a copy of a real manifest; see VIKE_MANIFEST_MIGRATION_FIXTURE"]
fn measure_v2_migration_on_a_real_manifest() {
    let Ok(fixture) = std::env::var("VIKE_MANIFEST_MIGRATION_FIXTURE") else {
        println!("VIKE_MANIFEST_MIGRATION_FIXTURE unset — nothing to measure");
        return;
    };
    let fixture = PathBuf::from(fixture);
    let v2_len = std::fs::metadata(&fixture).expect("the fixture must exist").len();

    // A series leaf holding just the manifest. The migration reads and writes only that file (plus
    // its backup), so the parts are not needed — and pointedly must not be, since the whole claim
    // is that migrating does not touch the 31 GiB of tape underneath. The leaf is spelled as an
    // ordinary bar series so the plain `append_bars` verb reaches it; the migration is the same
    // code for every kind, and what is being measured is the SIZE of the manifest.
    let dir = tempfile::tempdir().unwrap();
    let series = bars_series(dir.path());
    std::fs::create_dir_all(&series).unwrap();
    std::fs::copy(&fixture, series.join(MANIFEST)).unwrap();

    let store = DataFusionHist::open(dir.path()).unwrap();

    // READ: what a v3 build pays to open an UN-migrated store. This is the parse plus the orphan
    // derivation, and it is the number to compare against the 0.91 s `docs/decisions/0060-…`
    // measured for the v2 parse alone.
    let t0 = std::time::Instant::now();
    let keys = store.series_commits(&bars_id()).unwrap();
    let read_secs = t0.elapsed().as_secs_f64();

    // MIGRATE: the first write. One row, which is the shape the recorder's next flush would have
    // had — an empty batch would return before publishing and migrate nothing.
    let t1 = std::time::Instant::now();
    store
        .append_bars(
            "binance",
            "BTCUSDT",
            "1m",
            &[bar(1_700_000_000_000, 1.0)],
            Some("measurement-migration-probe"),
        )
        .expect("the migrating append");
    let migrate_secs = t1.elapsed().as_secs_f64();

    let v3_len = len_of(&series.join(MANIFEST));
    let bak_len = len_of(&series.join(V2_BACKUP));

    // AFTER: what every read of the migrated store pays, and what the next commit's critical
    // section opens with. This is the number the format bump has to justify itself against — a
    // parse inside `SeriesLock` is what `docs/decisions/0060-…` identified as the row-loss hazard
    // (`live_rec.rs`'s `SLOW_ATTEMPT` is one second, past which a contending flush DISCARDS).
    let t2 = std::time::Instant::now();
    let after_keys = store.series_commits(&bars_id()).unwrap();
    let after_read_secs = t2.elapsed().as_secs_f64();

    // ...and the whole critical section of the NEXT commit, end to end: lock, read, guard, WAL,
    // seal, publish.
    let t3 = std::time::Instant::now();
    store
        .append_bars(
            "binance",
            "BTCUSDT",
            "1m",
            &[bar(1_700_000_060_000, 2.0)],
            Some("measurement-steady-state-probe"),
        )
        .expect("the steady-state append");
    let commit_secs = t3.elapsed().as_secs_f64();

    // ...and the SAME commit under v2's write behaviour, for the comparison that matters. A fold
    // threshold of 1 byte makes every publish rewrite the whole base, which is exactly what v2 did;
    // this is therefore a measured v2 critical section rather than a modelled one. It is
    // CONSERVATIVE by about half: it rewrites the 54 MB v3 base where v2 rewrote 104 MB.
    store.set_fold_bytes_for_test(1);
    let t4 = std::time::Instant::now();
    store
        .append_bars(
            "binance",
            "BTCUSDT",
            "1m",
            &[bar(1_700_000_120_000, 3.0)],
            Some("measurement-whole-file-probe"),
        )
        .expect("the whole-file-publish append");
    let whole_file_commit_secs = t4.elapsed().as_secs_f64();

    println!(
        "V2 -> V3 MIGRATION, measured on a {v2_len} B manifest ({} keys):\n  \
         read BEFORE (v2 parse + orphan derivation, no write) = {read_secs:.3} s\n  \
         migrate (backup copy + v3 base publish)              = {migrate_secs:.3} s\n  \
         bytes written = {bak_len} B backup + {v3_len} B base = {} B\n  \
         base shrank {v2_len} B -> {v3_len} B ({:.1}%)\n  \
         read AFTER (v3 base + delta replay)                  = {after_read_secs:.3} s \
         ({} keys)\n  \
         CRITICAL SECTION, whole commit:\n    \
           v3 (append a frame)        = {commit_secs:.3} s\n    \
           v2-shaped (rewrite whole)  = {whole_file_commit_secs:.3} s  \
         [conservative: rewrites the 54 MB v3 base, not v2's 104 MB]",
        keys.len(),
        bak_len + v3_len,
        100.0 * v3_len as f64 / v2_len as f64,
        after_keys.len(),
    );
    assert_eq!(
        after_keys.len(),
        keys.len() + 1,
        "the migration must carry every commit key across, plus the one it published"
    );
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
    const WARM: i64 = 150;
    const WINDOW: i64 = 100;

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
