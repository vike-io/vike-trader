//! The manifest: idempotent ingest, a rebuildable cache, crashed-compaction rebuilds, the lock.

use std::time::{Duration, Instant};

use vike_data::{CompactionConfig, DataFusionHist, HistStore, SeriesId, TsRange};
use vike_model::Bar;

use crate::common::{
    assert_bars_bit_eq, bar, bars_id, bars_series, count_parquets, four_fragments_for_one_day,
    only_date_dir,
};

// ---- slice 2: manifest + idempotent ingest -------------------------------------------------

#[test]
fn append_is_idempotent_by_commit_key() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let bars: Vec<Bar> = (0..8).map(|i| bar(i * 60_000, 100.0, None)).collect();

    // first ingest of a keyed batch writes; the SAME key is a no-op (never a value-dedup)
    let n1 = df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("batch-A")).unwrap();
    let n2 = df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("batch-A")).unwrap();
    assert_eq!(n1, 8, "first append writes all rows");
    assert_eq!(n2, 0, "re-appending the same commit key is a no-op");

    // exactly one copy is stored (idempotent), not two
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 8, "no duplication from the retried batch");

    // a DIFFERENT key with genuinely-distinct rows DOES append (batch-level, not value-level)
    let more: Vec<Bar> = (8..12).map(|i| bar(i * 60_000, 100.0, None)).collect();
    assert_eq!(df.append_bars("binance", "BTCUSDT", "1m", &more, Some("batch-B")).unwrap(), 4);
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 12);
}

#[test]
fn manifest_persists_and_reopen_reads() {
    // the manifest is the durable file index — a fresh store over the same root sees the data
    let dir = tempfile::tempdir().unwrap();
    let bars: Vec<Bar> = (0..6).map(|i| bar(i * 60_000, 100.0, None)).collect();
    {
        let df = DataFusionHist::open(dir.path()).unwrap();
        df.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap();
    }
    // reopen (new runtime, new session) — reads come off the on-disk manifest
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    let got = df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(got.len(), 6);
    // and idempotency survives the reopen (commit-log is on disk)
    assert_eq!(df2.append_bars("binance", "BTCUSDT", "1m", &bars, Some("k")).unwrap(), 0);
}

#[test]
fn manifest_driven_read_prunes_non_overlapping_parts() {
    // three sealed parts at disjoint ts windows; a narrow query must return only the overlap.
    // (correctness proxy for file-level pruning — a non-overlapping part contributes nothing.)
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    let part = |base: i64| -> Vec<Bar> {
        (0..5).map(move |i| bar(base + i * 1000, 100.0, None)).collect()
    };
    df.append_bars("binance", "BTCUSDT", "1m", &part(0), Some("a")).unwrap(); // ts 0..4000
    df.append_bars("binance", "BTCUSDT", "1m", &part(100_000), Some("b")).unwrap(); // 100k..104k
    df.append_bars("binance", "BTCUSDT", "1m", &part(200_000), Some("c")).unwrap(); // 200k..204k

    // query only the middle window → only part b's rows
    let got = df.load_bars("binance", "BTCUSDT", "1m", TsRange::of(100_000, 104_000)).unwrap();
    assert_eq!(got.len(), 5);
    assert_eq!(got.first().unwrap().ts, 100_000);
    assert_eq!(got.last().unwrap().ts, 104_000);

    // whole-range query → all three parts merge, ordered
    let all = df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(all.len(), 15);
    assert!(all.windows(2).all(|w| w[0].ts <= w[1].ts));
}

// ---- manifest as a rebuildable CACHE, not ground truth ----------------------------------------

/// Remove a series' whole INDEX — the disaster a rebuild exists to recover from.
///
/// ⚠ **At manifest v3 the index is TWO files**, a base (`_manifest.json`) and its append-only delta
/// log (`_manifest.delta`), and every test below that stages "the index is gone" has to remove
/// both. Removing only the base is not a smaller disaster, it is a DIFFERENT state: the store
/// refuses to read it outright, because replaying a log with no base would index the parts
/// committed since the last fold and silently hide every older one. `manifest::read_manifest`
/// carries that argument; `a_base_less_series_with_a_surviving_log_is_refused_not_half_read` is
/// the test for it.
fn remove_series_index(series_dir: &std::path::Path) {
    std::fs::remove_file(series_dir.join("_manifest.json")).unwrap();
    match std::fs::remove_file(series_dir.join("_manifest.delta")) {
        Ok(()) => {}
        // A series whose every publish has been folded (or whose only publish was its first, which
        // writes a base rather than a frame) legitimately has no log.
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("removing the delta log: {e}"),
    }
}

/// Deleting the index used to make every part beneath it UNREACHABLE — the read path never
/// LISTs directories (deliberately, spec must-fix #5), so data sitting on disk became invisible with
/// no way back. `rebuild_series_manifest` reconstructs the index from the parts themselves.
///
/// The assertion is round-trip IDENTITY, not "some rows came back": every recovered `FileEntry`
/// must match what the original manifest recorded — including `commit_keys`, which is why parts now
/// stamp them into their Parquet footer. Anything weaker would let a rebuild silently drop the
/// idempotency log and re-admit already-applied appends.
#[test]
fn manifest_rebuilds_from_the_parts_after_it_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    // Fold on every publish, so `_manifest.json` holds the WHOLE file index and the field-for-field
    // comparison below is against the same thing a rebuild produces. Without this the base carries
    // only the first commit and the rest sit in the delta log — a correct manifest, but not one a
    // single-file comparison can read.
    store.set_fold_bytes_for_test(1);

    // Three keyed appends across TWO UTC days, so the rebuild has to walk >1 `date=` dir and
    // recover several parts with distinct commit keys.
    let day1 = 1_700_000_000_000i64; // some ts inside one UTC day
    let day2 = day1 + 86_400_000;
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(day1, 100.0, None)], Some("k1")).unwrap();
    store
        .append_bars("binance", "BTCUSDT", "1m", &[bar(day1 + 60_000, 101.0, None)], Some("k2"))
        .unwrap();
    store.append_bars("binance", "BTCUSDT", "1m", &[bar(day2, 102.0, None)], Some("k3")).unwrap();

    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 3, "3 bars ingested");

    let series_dir = dir
        .path()
        .join("kind=bar")
        .join("venue=binance")
        .join("symbol=BTCUSDT")
        .join("interval=1m");
    let manifest_path = series_dir.join("_manifest.json");
    let id_for_commits = SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
        source: None,
    };
    // The index is base + delta log at v3, so "the manifest as it was" is read through the store
    // rather than off one file. `files` is still compared as JSON below; the commit log is compared
    // through `series_commits`, which is the accessor that answers it at either format.
    let original_files: serde_json::Value = {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        v["files"].clone()
    };
    let original_commits = store.series_commits(&id_for_commits).unwrap();

    // The disaster: the index is gone. Parts are all still on disk.
    remove_series_index(&series_dir);
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    assert!(
        store2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().is_empty(),
        "without the index the parts are invisible — this is the failure being fixed"
    );

    // The recovery.
    let id = SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: Some("1m".into()),
        group: None,
        source: None,
    };
    let report = store2.rebuild_series_manifest(&id).unwrap();
    assert_eq!(report.parts_recovered, 3, "one part per keyed append: {report:?}");
    assert_eq!(report.parts_unreadable, 0, "{report:?}");
    assert_eq!(report.parts_without_keys, 0, "every part stamped its commit key: {report:?}");

    // Rows are back, byte-for-byte.
    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(after.len(), 3);
    for (a, b) in before.iter().zip(after.iter()) {
        assert_eq!(a.ts, b.ts);
        assert_eq!(a.close.to_bits(), b.close.to_bits(), "f64 bit-identical across a rebuild");
    }

    // And the INDEX is back: same files, same ranges, same commit log. Compared as parsed JSON so
    // key order and the `version` counter (which a rebuild advances by design) don't matter.
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    assert_eq!(rebuilt["files"], original_files, "file index recovered field-for-field");
    let mut a = store2.series_commits(&id_for_commits).unwrap();
    let mut b = original_commits;
    a.sort();
    b.sort();
    assert_eq!(a, b, "the idempotency log survives the rebuild — k1/k2/k3");
    assert_eq!(b.len(), 3, "the comparison is over three real keys, not two empty vectors");
    // A rebuild publishes a BASE and leaves no log behind: a surviving frame would replay over the
    // very index the rebuild just decided on.
    assert!(
        !series_dir.join("_manifest.delta").exists(),
        "a rebuild must clear the delta log it replaced"
    );
}

/// The point of recovering the commit log: an append whose key is already durable must STILL be a
/// no-op after a rebuild. Without footer-stamped keys this silently duplicated rows.
#[test]
fn idempotency_survives_a_manifest_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let ts = 1_700_000_000_000i64;
    store.append_bars("binance", "ETHUSDT", "1m", &[bar(ts, 50.0, None)], Some("dup")).unwrap();

    let series_dir = dir
        .path()
        .join("kind=bar")
        .join("venue=binance")
        .join("symbol=ETHUSDT")
        .join("interval=1m");
    remove_series_index(&series_dir);

    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let id = SeriesId {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        interval: Some("1m".into()),
        group: None,
        source: None,
    };
    store2.rebuild_series_manifest(&id).unwrap();

    // Re-running the SAME keyed append must write nothing.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let written = store3
        .append_bars("binance", "ETHUSDT", "1m", &[bar(ts, 50.0, None)], Some("dup"))
        .unwrap();
    assert_eq!(written, 0, "key 'dup' was recovered from the part footer — this must no-op");
    assert_eq!(
        store3.load_bars("binance", "ETHUSDT", "1m", TsRange::all()).unwrap().len(),
        1,
        "still exactly one bar — a lost commit log would have duplicated it"
    );
}

// ---- a rebuild must never count a merge's INPUTS and its OUTPUT both ---------------------------
//
// Compaction has two crash windows in which the inputs of a merge and the merge's output are BOTH
// sitting in the same `date=` directory. A rebuild reads the directory, so before the fix it indexed
// both and the recovered series carried every merged row TWICE. Measured previously by SIGKILLing a
// store mid-compaction: 34–68% row inflation in 7 of 10 kill trials. Anyone calling the rebuild is
// already recovering from a crash, which is the worst possible moment to silently double their data.
//
// Both windows are reproduced DETERMINISTICALLY here — no kill, no timing:
//   * `..._crashed_before_publish...` uses the crash-injection switch to stop compaction between its
//     unlocked merge and its publish (the long window: the merge is the whole cost of compaction).
//   * `..._crashed_before_the_unlinks...` runs a REAL compaction to completion and then restores the
//     input files, which is exactly the state a crash between the manifest publish and the unlink
//     loop leaves behind.

/// WINDOW 1 — the merge ran, nothing was published. This is the LONG window: compaction deliberately
/// releases the series lock for the decode/sort/re-encode (holding it starves the live recorder), so
/// a crash lands here with overwhelming probability.
///
/// The 20 rows exist once as four input parts and once inside the merge output. A rebuild that reads
/// the directory sees five files and, before the fix, indexed all five — 40 rows out of a 20-row
/// series.
#[test]
fn rebuild_after_a_compaction_crashed_before_publish_does_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 20);
    assert_eq!(count_parquets(&series), 4);

    // The crash: merge, then die before the publish.
    store.set_stop_after_merge_for_test(true);
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_written, 0, "nothing was published — the report must claim nothing");
    assert_eq!(
        count_parquets(&series),
        5,
        "the crash state: four inputs plus an unpublished merge output"
    );

    // The operator's recovery move. (`_manifest.json` is removed because that is WHY a rebuild is
    // ever run — the index is gone; the parts are all that is left.)
    remove_series_index(&series);
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let report = store2.rebuild_series_manifest(&bars_id()).unwrap();

    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(
        after.len(),
        20,
        "a rebuild over a crashed compaction counted the merge's inputs AND its output: {report:?}"
    );
    assert_bars_bit_eq(&before, &after);

    // ...and by the intended mechanism, not by luck: the four fragments were recovered and the
    // unpublished merge output was recognised as one.
    assert_eq!(report.parts_recovered, 4, "{report:?}");
    assert_eq!(report.parts_unpublished_merge, 1, "{report:?}");
    assert_eq!(report.parts_superseded, 0, "nothing published ⇒ nothing supersedes: {report:?}");

    // THE SAFETY PROPERTY, and the reason the fix cannot be "delete whatever the manifest does not
    // list": a rebuild may run while a compaction is mid-merge in ANOTHER process. Keeping the
    // fragments is what lets that merge's publish still find its inputs — the verify it runs under
    // the lock succeeds, so it publishes instead of abandoning and deleting its own output.
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(series.join("_manifest.json")).unwrap())
            .unwrap();
    let names: Vec<String> = rebuilt["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    for n in 1..=4 {
        let want = format!("part-{n:05}.parquet");
        assert!(names.contains(&want), "an in-flight merge's input vanished: {names:?}");
    }

    // And the series is still compactable: the leftover unpublished file is inert — never planned,
    // never read — so the retry merges the same four fragments and keeps all 20 rows.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let rep2 = store3.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep2.parts_merged, 4, "the retry must see the fragments, not the orphan: {rep2:?}");
    assert_eq!(rep2.rows, 20, "{rep2:?}");
    assert_bars_bit_eq(
        &before,
        &store3.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
    );
}

/// WINDOW 2 — the manifest was published and the process died before (or during) the unlink loop
/// that removes the merged fragments. Publish-then-unlink is the right order (the reverse would
/// leave a manifest naming files that are gone), so this window is inherent, not a bug in itself.
///
/// Reproduced by restoring the input files after a REAL compaction: byte-for-byte the state a crash
/// in that window leaves. The output is a `part-c…` at its final name here, so the `_tmp-` rule that
/// answers window 1 cannot help — this is what the commit-key containment rule is for.
#[test]
fn rebuild_after_a_compaction_crashed_before_the_unlinks_does_not_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();
    assert_eq!(before.len(), 20);

    // Snapshot the fragments, compact for real, then put them back — the unlink loop never ran.
    let date_dir = only_date_dir(&series);
    // OUTSIDE the store root: a stray parquet under the root would be walked by `list_series`.
    let stash_dir = tempfile::tempdir().unwrap();
    let stash = stash_dir.path();
    let mut inputs = Vec::new();
    for e in std::fs::read_dir(&date_dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&p, stash.join(&name)).unwrap();
        inputs.push(name);
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(rep.parts_merged, 4);
    assert_eq!(rep.parts_written, 1);
    for name in &inputs {
        std::fs::copy(stash.join(name), date_dir.join(name)).unwrap();
    }
    assert_eq!(count_parquets(&series), 5, "the crash state: the sealed part plus its dead inputs");

    remove_series_index(&series);
    let store2 = DataFusionHist::open(dir.path()).unwrap();
    let report = store2.rebuild_series_manifest(&bars_id()).unwrap();

    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(
        after.len(),
        20,
        "a rebuild counted the merged fragments AND the sealed part that replaced them: {report:?}"
    );
    assert_bars_bit_eq(&before, &after);

    // Through the containment rule, not by luck: the sealed part is the one recovered, and each of
    // the four fragments was recognised as already inside it by its commit key.
    assert_eq!(report.parts_recovered, 1, "{report:?}");
    assert_eq!(report.parts_superseded, 4, "{report:?}");
    // The idempotency log survives the supersession — the sealed part carries the union of its
    // inputs' keys, so re-running any of those four appends must still be a no-op.
    let store3 = DataFusionHist::open(dir.path()).unwrap();
    let again =
        store3.append_bars("binance", "BTCUSDT", "1m", &[bar(0, 100.0, None)], Some("b0")).unwrap();
    assert_eq!(again, 0, "key 'b0' rode into the sealed part's footer — this must no-op");
}

/// A final-named orphan sitting exactly where the next merge wants to publish must not WEDGE that
/// date. `fs::rename` replaces on unix but FAILS on Windows when the destination exists, and a
/// destination can exist — a build that wrote the output at its final name (every build before the
/// `_tmp-merge-` rename) left one there on every crash. Without the remove-then-retry fallback that
/// date would abandon on every pass, forever, and the store would never compact again on Windows.
///
/// The orphan is manufactured from a real crashed merge rather than by guessing a filename, so the
/// test cannot rot away from the naming rule it depends on.
#[test]
fn a_final_named_orphan_does_not_wedge_the_next_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series = four_fragments_for_one_day(dir.path(), &store);
    let before = store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap();

    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    store.set_stop_after_merge_for_test(true);
    store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();

    // Strip the prefix: what an older build left behind is this same file at its final name.
    let date_dir = only_date_dir(&series);
    let tmp = std::fs::read_dir(&date_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("_tmp-merge-"))
        })
        .expect("the merge output is written under the unpublished prefix");
    let final_name = tmp.file_name().unwrap().to_str().unwrap().trim_start_matches("_tmp-merge-");
    std::fs::rename(&tmp, date_dir.join(final_name)).unwrap();

    // The retry re-merges the same four fragments and must publish over that orphan.
    store.set_stop_after_merge_for_test(false);
    let rep = store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    assert_eq!(
        rep.parts_merged, 4,
        "the orphan wedged the date instead of being replaced: {rep:?}"
    );
    assert_eq!(rep.rows, 20, "{rep:?}");
    assert_bars_bit_eq(
        &before,
        &store.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap(),
    );
}

/// A part with NO commit key is the one shape the containment rule cannot see — the empty set is a
/// subset of everything, so treating it as contained would drop every keyless part in a date that
/// also holds a keyed one. Pinned here as a KNOWN residual so nobody later "tidies" the empty-set
/// guard away: a keyless fragment left by a crash in window 2 is still double-counted.
///
/// It is narrow by construction — the recorder, every backfill collector and the bulk importer all
/// key their appends (that is what makes them idempotent), so this is the shape of an append that
/// deliberately opted out of idempotency — and it is REPORTED rather than silent:
/// `parts_without_keys` counts exactly the parts this pass had to take on trust.
#[test]
fn a_keyless_fragment_is_still_double_counted_and_the_report_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    for c in 0..4i64 {
        let batch: Vec<Bar> =
            (0..5).map(|i| bar((c * 5 + i) * 1000, 100.0 + (c * 5 + i) as f64, None)).collect();
        store.append_bars("binance", "BTCUSDT", "1m", &batch, None).unwrap(); // NO commit key
    }
    let series = bars_series(dir.path());
    let date_dir = only_date_dir(&series);
    let stash_dir = tempfile::tempdir().unwrap();
    let mut inputs = Vec::new();
    for e in std::fs::read_dir(&date_dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        std::fs::copy(&p, stash_dir.path().join(&name)).unwrap();
        inputs.push(name);
    }
    let cfg = CompactionConfig { target_bytes: 1 << 20, min_parts: 3, ..Default::default() };
    store.compact_series("bar", "binance", "BTCUSDT", Some("1m"), &cfg).unwrap();
    for name in &inputs {
        std::fs::copy(stash_dir.path().join(name), date_dir.join(name)).unwrap();
    }

    remove_series_index(&series);
    let report =
        DataFusionHist::open(dir.path()).unwrap().rebuild_series_manifest(&bars_id()).unwrap();
    assert_eq!(report.parts_superseded, 0, "no keys ⇒ no containment to prove: {report:?}");
    assert_eq!(report.parts_without_keys, 5, "the report names the blind spot: {report:?}");
    let after = DataFusionHist::open(dir.path())
        .unwrap()
        .load_bars("binance", "BTCUSDT", "1m", TsRange::all())
        .unwrap();
    assert_eq!(after.len(), 40, "KNOWN residual: keyless parts still double-count (20 real rows)");
}

// ---- crash-safety of the per-series write lock -----------------------------------------------
//
// the CI box, 2026-08-04: the recorder's compaction thread was OOM-killed by the cgroup while holding a
// series lock. SIGKILL runs no `Drop`, so `_manifest.lock` was left on disk — and because the lock
// WAS that file's existence, no later process could tell a dead owner from a live one. Every
// restart timed out opening the store and exited 1; systemd restarted it 2,728 times over ~11 h,
// recording nothing. The lock must be held by the OS (which releases it when the holder dies),
// never by a file that outlives its owner.

/// A lock file left behind by a killed writer must not wedge the next writer.
#[test]
fn a_leftover_lock_file_does_not_block_the_next_writer() {
    let dir = tempfile::tempdir().unwrap();
    let df = DataFusionHist::open(dir.path()).unwrap();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(1000, 100.0, None)], Some("a")).unwrap();

    // Exactly what a SIGKILL'd holder leaves: the lock file, with nobody holding it.
    let leftover = bars_series(dir.path()).join("_manifest.lock");
    std::fs::write(&leftover, b"").unwrap();
    assert!(leftover.exists());

    let t0 = Instant::now();
    df.append_bars("binance", "BTCUSDT", "1m", &[bar(2000, 101.0, None)], Some("b"))
        .expect("a leftover lock file wedged the writer — a killed holder must not outlive itself");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "append took {:?} — it spun on the leftover lock instead of taking it",
        t0.elapsed()
    );
    assert_eq!(df.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 2);

    // And reopening the store (the path that actually crash-looped: WAL recovery locks each series)
    // must work too.
    drop(df);
    let df2 = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(df2.load_bars("binance", "BTCUSDT", "1m", TsRange::all()).unwrap().len(), 2);
}
