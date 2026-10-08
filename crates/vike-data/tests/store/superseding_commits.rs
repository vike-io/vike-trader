//! Provisional commits and the `*_superseding` primitive.

use std::path::{Path, PathBuf};

use vike_data::{CompactionConfig, DataFusionHist, HistStore, SeriesId, TsRange};

use crate::common::qt;

// ---- provisional commits: the `*_superseding` primitive (`commit_rows_inner`) ------------------

/// The on-disk leaf for a per-symbol QUOTE series — `<root>/kind=quote/venue=<v>/symbol=<s>` —
/// mirroring `DataFusionHist::series_dir`'s own path construction so the disk-level tests below can
/// prove what actually happened to a `.parquet` file, not just what `scan_quotes` reads back through
/// the manifest (which reflects the manifest ALONE and cannot tell "the old file was unlinked" from
/// "the old file just leaked, forever, orphaned but invisible" — the two are indistinguishable from
/// any read-path API by design).
fn quote_series_dir(store: &DataFusionHist, venue: &str, symbol: &str) -> PathBuf {
    store.root().join("kind=quote").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
}

/// Recursively count `.parquet` files under `dir` (a series leaf holds one `date=` dir per UTC day,
/// each holding its own sealed parts).
fn count_parquet_files(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                n += count_parquet_files(&p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("parquet") {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn a_superseding_commit_replaces_the_provisional_rows_not_adds_to_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let early = vec![qt(0, 1.0), qt(1_000, 1.1)];
    let complete = vec![qt(0, 1.0), qt(1_000, 1.1), qt(2_000, 1.2)];

    let n1 = store.append_quotes_superseding("v", "S", &early, Some("provisional"), None).unwrap();
    assert_eq!(n1, 2);

    let n2 = store
        .append_quotes_superseding("v", "S", &complete, Some("canonical"), Some("provisional"))
        .unwrap();
    assert_eq!(n2, 3);

    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "the provisional rows are GONE, not doubled");
}

#[test]
fn superseding_a_key_that_was_never_spent_is_a_harmless_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let rows = vec![qt(0, 1.0)];

    let n = store
        .append_quotes_superseding("v", "S", &rows, Some("canonical"), Some("never-spent"))
        .unwrap();

    assert_eq!(n, 1);
    assert_eq!(store.scan_quotes("v", "S", TsRange::all()).unwrap().len(), 1);
}

#[test]
fn a_repeated_canonical_commit_after_superseding_is_still_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let rows = vec![qt(0, 1.0)];
    store.append_quotes_superseding("v", "S", &rows, Some("provisional"), None).unwrap();
    store
        .append_quotes_superseding("v", "S", &rows, Some("canonical"), Some("provisional"))
        .unwrap();

    let n = store
        .append_quotes_superseding("v", "S", &rows, Some("canonical"), Some("provisional"))
        .unwrap();

    assert_eq!(n, 0, "the canonical key is already spent — a repeat is a no-op, as for any key");
    assert_eq!(store.scan_quotes("v", "S", TsRange::all()).unwrap().len(), 1, "not doubled");
}

#[test]
fn a_settled_empty_batch_still_supersedes_a_stale_provisional_entry() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();

    // The settled fetch genuinely found NOTHING new — an empty batch, but still superseding.
    let n = store
        .append_quotes_superseding("v", "S", &[], Some("canonical"), Some("provisional"))
        .unwrap();

    assert_eq!(n, 0, "nothing new to write");
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        0,
        "the stale provisional row is gone even though the settled fetch added nothing"
    );
}

#[test]
fn a_compacted_multi_key_part_unrelated_to_the_supersede_key_survives_untouched() {
    // ⚠ Renamed from `a_file_carrying_other_keys_besides_the_supersede_key_is_never_touched`
    // (second re-review round). That old name promised a file is "never touched" — but under the
    // fixed I2 design, a file that DOES carry the supersede key alongside others is now a REFUSAL
    // case (`a_provisional_key_already_folded_into_a_multi_key_part_by_compaction_refuses_rather_
    // than_doubles` below), not a silent skip, so a "never touched" name next to a scenario that no
    // longer involves the supersede key at all was misleading regardless of pass/fail. What this
    // test actually proves is narrower and still real: a multi-key part genuinely UNRELATED to the
    // key being superseded (shares none of its keys) is left alone. It also used to assert only a
    // ROW COUNT ("compacted to one part") that reads identically whether compaction ran or not — a
    // pre-compaction k1-only-part + k2-only-part scan already returns the same 2 rows — so it never
    // actually verified its own premise; this version asserts on `CompactionReport` directly.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes("v", "S", &[qt(0, 1.0)], Some("k1")).unwrap();
    store.append_quotes("v", "S", &[qt(1_000, 1.1)], Some("k2")).unwrap();
    let report = store
        .compact_series(
            "quote",
            "v",
            "S",
            None,
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(report.parts_merged, 2, "k1 and k2's fragments must actually be merged: {report:?}");
    assert_eq!(report.parts_written, 1, "into exactly one sealed multi-key part: {report:?}");
    assert_eq!(store.scan_quotes("v", "S", TsRange::all()).unwrap().len(), 2);

    // A day well past 1970-01-01, so this part never shares a `date=` dir with the compacted one.
    store
        .append_quotes_superseding("v", "S", &[qt(90_000_000, 2.0)], Some("provisional"), None)
        .unwrap();

    let n = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(90_000_000, 2.1)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap();

    assert_eq!(n, 1);
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        3,
        "the unrelated compacted part (k1+k2) survives untouched; the provisional row was replaced"
    );
}

#[test]
fn a_crash_between_publish_and_unlink_leaves_the_store_correct() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();
    let series_dir = quote_series_dir(&store, "v", "S");
    assert_eq!(count_parquet_files(&series_dir), 1, "one provisional part on disk");

    store.set_stop_after_supersede_publish_for_test(true);
    let n = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(1_000, 1.1)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap();
    assert_eq!(n, 2, "the seal + publish completed before the simulated crash point");
    store.set_stop_after_supersede_publish_for_test(false);

    // ⚠ THE ASSERTION THAT MATTERS, and the one a manifest-only read can never make: the publish
    // already succeeded (proved by the row count above, which reads through the manifest), but the
    // seam fires BEFORE the unlink loop — so the superseded provisional part's BYTES must still be
    // physically present. A suite that only ever reads through `scan_quotes` cannot tell "unlinked
    // on time", "never unlinked at all" and "unlinked too early (before publish)" apart; this can.
    assert_eq!(
        count_parquet_files(&series_dir),
        2,
        "the orphaned provisional file must still be ON DISK — the crash seam fires strictly \
         before the unlink loop runs, manifest-first"
    );

    // The publish succeeded (asserted above via a real row count), but the physical provisional
    // file was never unlinked — this is the orphan the crash window leaves. A FRESH open must see
    // only the durable manifest state: the provisional file gone from every read, regardless of
    // whether its bytes are still sitting on disk.
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let got = reopened.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        2,
        "correct data, not doubled, even though the old file was never unlinked"
    );
    assert_eq!(
        count_parquet_files(&series_dir),
        2,
        "reopening does not clean up the orphan — that is the repair tool's job (see \
         `a_resurrected_orphan_provisional_part_is_recognized_and_not_double_counted`), not \
         recovery's; the orphan staying on disk, inert and invisible to reads, IS the property \
         being proved here"
    );
}

#[test]
fn a_normal_supersede_actually_unlinks_the_superseded_file_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();
    let series_dir = quote_series_dir(&store, "v", "S");
    assert_eq!(count_parquet_files(&series_dir), 1);

    store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(1_000, 1.1)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap();

    // No crash seam fired this time — the physical provisional file must be GONE, not merely
    // absent from the manifest. This is the half a manifest-only (`scan_quotes`) assertion cannot
    // see: the unlink loop must actually RUN in the ordinary case, not have been silently deleted
    // or made a no-op.
    assert_eq!(
        count_parquet_files(&series_dir),
        1,
        "the superseded provisional part must be UNLINKED from disk, not just dropped from the \
         manifest — only the new canonical part's file should remain"
    );
}

#[test]
fn a_crash_between_wal_fsync_and_publish_replays_the_supersede_too_not_just_the_seal() {
    // The exact scenario a crash BEFORE the first publish leaves: `skip_publish_for_test` — unlike
    // `stop_after_supersede_publish_for_test` above — fires BEFORE that publish, so this proves the
    // OTHER crash window: the one the WAL (not the unlink loop) exists to close. If the WAL record
    // only carried `commit_key` and not `supersede_key`, recovery would replay the seal alone and
    // ts=0 would come back TWICE (the never-removed provisional row plus the recovered canonical
    // one) — permanently, since a retry through `commit_rows_inner` hits `has_commit("canonical")`
    // and returns `Ok(0)` before the supersede logic ever runs again.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();

    store.set_skip_publish_for_test(true);
    let both = [qt(0, 1.0), qt(1_000, 1.1)];
    let n = store
        .append_quotes_superseding("v", "S", &both, Some("canonical"), Some("provisional"))
        .unwrap();
    assert_eq!(n, 2, "the seal + WAL fsync completed before the simulated crash point");
    store.set_skip_publish_for_test(false);

    // Reopen WITHOUT ever publishing live: recovery must replay the WAL record, which must carry
    // enough to redo BOTH halves of the original call — the seal AND the supersede — in one pass.
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let got = reopened.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(
        got.len(),
        2,
        "ts=0 must not appear twice: crash recovery of a superseding commit must ALSO remove the \
         provisional part it was superseding, not just re-seal its own rows"
    );
}

#[test]
fn a_replayed_superseding_commit_stamps_the_key_it_superseded() {
    // The replay's half of the stamp that
    // `a_resurrected_orphan_provisional_part_is_recognized_and_not_double_counted` pins on the live
    // path. The crash-window test above counts ROWS, and the stamp changes no row, so a replay that
    // sealed the recovered canonical part under `[canonical]` alone — forgetting the provisional key
    // it superseded — passed the whole suite until this test existed. What the stamp is FOR is that
    // the superseded key stays spent: a late write under it must be the idempotent no-op, exactly
    // as it is after a live supersede.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();

    store.set_skip_publish_for_test(true);
    let both = [qt(0, 1.0), qt(1_000, 1.1)];
    let n = store
        .append_quotes_superseding("v", "S", &both, Some("canonical"), Some("provisional"))
        .unwrap();
    assert_eq!(n, 2, "the seal + WAL fsync completed before the simulated crash point");
    drop(store); // == the crash: nothing was published

    let reopened = DataFusionHist::open(dir.path()).unwrap();
    assert_eq!(
        reopened.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        2,
        "the replay applied the seal and the supersede (the test above pins this half)"
    );

    let late = reopened
        .append_quotes_superseding("v", "S", &[qt(500, 9.9)], Some("provisional"), None)
        .unwrap();
    assert_eq!(
        late, 0,
        "the replayed canonical part must carry the provisional key it superseded, so that key is \
         still spent and a late write under it is a no-op — not a row sealed beside the canonical \
         ones"
    );
    assert_eq!(
        reopened.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        2,
        "nothing was written under the spent key"
    );
}

#[test]
fn a_plain_keyed_commit_leaves_a_v1_wal_frame_and_a_superseding_one_a_v2_frame() {
    // The frame an OLDER binary meets after a crash, read off the disk where it would meet it. A
    // release through v0.1.35 stops at the first frame that is not `VWAL` and then removes the
    // file, so every plain keyed commit — every recorder flush — must still leave a `VWAL` frame
    // behind; only a commit that carries a supersede key needs the `VWL2` shape, and gets it.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.set_skip_publish_for_test(true); // each record stays in its WAL: the crash window
    store.append_quotes("v", "PLAIN", &[qt(0, 1.0)], Some("plain")).unwrap();
    store
        .append_quotes_superseding("v", "SUPER", &[qt(0, 1.0)], Some("canonical"), Some("p"))
        .unwrap();

    let wal_magic = |symbol: &str| {
        let wal = quote_series_dir(&store, "v", symbol).join("_wal.arrow");
        std::fs::read(&wal).unwrap_or_else(|e| panic!("read {}: {e}", wal.display()))[..4].to_vec()
    };
    assert_eq!(wal_magic("PLAIN"), b"VWAL", "a record with no supersede key is the v1 frame");
    assert_eq!(wal_magic("SUPER"), b"VWL2", "a record carrying a supersede key is the v2 frame");
}

#[test]
fn a_replay_refusal_skips_its_series_and_the_store_still_opens() {
    // A supersede the replay must REFUSE: series A's canonical commit crashed inside its
    // seal→publish window, and — the process kept running — compaction then folded the provisional
    // part it supersedes into a multi-key part, so the removal can never be performed exactly.
    // That refusal used to fail `open` for the WHOLE store: the datahub, and the recorder inside
    // it, restart-looped on one series. Series B, crashed the same way with a plain commit, is the
    // bystander that must still recover.
    let dir = tempfile::tempdir().unwrap();
    {
        let store = DataFusionHist::open(dir.path()).unwrap();
        store
            .append_quotes_superseding("v", "A", &[qt(0, 1.0)], Some("provisional"), None)
            .unwrap();
        store.set_skip_publish_for_test(true);
        store
            .append_quotes_superseding(
                "v",
                "A",
                &[qt(0, 1.0), qt(1_000, 1.1)],
                Some("canonical"),
                Some("provisional"),
            )
            .unwrap();
        store.append_quotes("v", "B", &[qt(0, 2.0)], Some("b1")).unwrap();
        store.set_skip_publish_for_test(false);
        // Still running: A gets a second commit on the same day, and compaction folds the
        // provisional part into one part with it before anything replays the canonical record.
        store.append_quotes("v", "A", &[qt(2_000, 1.2)], Some("k2")).unwrap();
        let report = store
            .compact_series(
                "quote",
                "v",
                "A",
                None,
                &CompactionConfig { min_parts: 2, ..Default::default() },
            )
            .unwrap();
        assert_eq!(report.parts_merged, 2, "the provisional part must be folded: {report:?}");
    } // == the process dies with A's canonical record still in its WAL

    let reopened = DataFusionHist::open(dir.path())
        .expect("a refusal confined to one series must not fail `open` for the whole store");
    assert_eq!(
        reopened.scan_quotes("v", "B", TsRange::all()).unwrap().len(),
        1,
        "the bystander series' crashed commit was recovered"
    );
    let a_ts: Vec<i64> =
        reopened.scan_quotes("v", "A", TsRange::all()).unwrap().iter().map(|q| q.ts).collect();
    assert_eq!(
        a_ts,
        vec![0, 2_000],
        "A holds exactly its pre-crash rows: the refused commit's rows were not sealed and the \
         provisional row was not removed"
    );
    let a_wal = quote_series_dir(&reopened, "v", "A").join("_wal.arrow");
    assert!(a_wal.exists(), "the refused record is KEPT, to be met again at every open");
    assert!(
        !quote_series_dir(&reopened, "v", "B").join("_wal.arrow").exists(),
        "B's applied record was cleared as always"
    );
    // The refused series still takes writes...
    assert_eq!(reopened.append_quotes("v", "A", &[qt(3_000, 1.3)], Some("k3")).unwrap(), 1);
    drop(reopened);

    // ...and a second open meets the kept record again, and again opens.
    let again = DataFusionHist::open(dir.path()).expect("the second open succeeds too");
    assert_eq!(again.scan_quotes("v", "A", TsRange::all()).unwrap().len(), 3);
    assert!(a_wal.exists(), "still kept — nothing resolves it but an operator");
}

#[test]
fn a_provisional_commit_after_its_canonical_twin_writes_nothing() {
    // The race the follow-ups spec's #3 closes: the SETTLED commit lands first, while its
    // provisional twin has never been spent (a slower recent request is still fetching). The
    // canonical commit PRE-SPENDS the twin — its part carries `[canonical, provisional]` whether
    // or not a provisional part existed to remove — so the late provisional write meets
    // `has_commit` under the series lock and is the idempotent no-op, instead of sealing its early,
    // partial rows beside the settled ones for good.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let full = [qt(0, 1.0), qt(1_000, 1.1), qt(2_000, 1.2)];
    let early = [qt(0, 1.0), qt(1_000, 1.1)];

    let n = store
        .append_quotes_superseding("v", "S", &full, Some("canonical"), Some("provisional"))
        .unwrap();
    assert_eq!(n, 3, "the twin was never spent: nothing to remove, the full rows sealed");

    let late =
        store.append_quotes_superseding("v", "S", &early, Some("provisional"), None).unwrap();
    assert_eq!(late, 0, "the canonical commit pre-spent its provisional twin");
    let ts: Vec<i64> =
        store.scan_quotes("v", "S", TsRange::all()).unwrap().iter().map(|q| q.ts).collect();
    assert_eq!(ts, vec![0, 1_000, 2_000], "exactly the full rows, not the early ones beside them");
}

#[test]
fn a_failed_provisional_publish_is_not_replayed_beside_its_canonical_twin() {
    // #3's WAL-replay variant. The provisional commit's publish fails after its WAL fsync and the
    // process keeps running; the canonical commit then lands with the twin still unspent. Before
    // the pre-spend, the canonical commit's WAL rewrite KEPT the provisional record (its key was
    // still unspent), and the next `open` replayed it beside the canonical rows.
    let dir = tempfile::tempdir().unwrap();
    {
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.set_skip_publish_for_test(true);
        store
            .append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None)
            .unwrap();
        store.set_skip_publish_for_test(false);
        let full = [qt(0, 1.0), qt(1_000, 1.1), qt(2_000, 1.2)];
        let n = store
            .append_quotes_superseding("v", "S", &full, Some("canonical"), Some("provisional"))
            .unwrap();
        assert_eq!(n, 3);
    }
    let reopened = DataFusionHist::open(dir.path()).unwrap();
    let ts: Vec<i64> =
        reopened.scan_quotes("v", "S", TsRange::all()).unwrap().iter().map(|q| q.ts).collect();
    assert_eq!(ts, vec![0, 1_000, 2_000], "the unpublished provisional record was not replayed");
    assert!(
        !quote_series_dir(&reopened, "v", "S").join("_wal.arrow").exists(),
        "the canonical commit's own WAL rewrite dropped the twin's record as applied"
    );
}

#[test]
fn a_keyless_superseding_commit_is_refused_and_touches_nothing() {
    // #6. A keyless seal carries `[supersede_key]` alone — the very key set of the provisional part
    // it would remove — so the next supersede of that key would delete it wholesale, and a repeat
    // of the call would replace it again instead of being idempotent.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();

    let err = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(1_000, 1.1)],
            None,
            Some("provisional"),
        )
        .unwrap_err();
    assert!(
        !err.is_supersede_refusal(),
        "a keyless call is a caller's mistake, not the store refusing a KEY — a backfill must not \
         step over it as one: {err}"
    );
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        1,
        "exactly the provisional row: nothing sealed, nothing removed"
    );
    assert_eq!(count_parquet_files(&quote_series_dir(&store, "v", "S")), 1, "one part on disk");
}

#[test]
fn a_resurrected_orphan_provisional_part_is_recognized_and_not_double_counted() {
    // The design's safety argument leans on `rebuild_manifest`'s existing containment rule (a part
    // whose commit-key set is a STRICT SUBSET of another's in the same `date=` is a resurrected
    // orphan and is skipped) to cover an orphan a crash or a discarded `remove_file` error might
    // leave. `{provisional}` alone is NOT a subset of `{canonical}` alone — the two are disjoint —
    // so that rule only fires because `seal_into_manifest` now stamps the canonical part with BOTH
    // keys (`[canonical, provisional]`). This proves the MECHANISM, not just the outcome: a rebuild
    // must report the orphan as `parts_superseded`, not `parts_recovered`.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes_superseding("v", "S", &[qt(0, 1.0)], Some("provisional"), None).unwrap();

    // Force the exact orphan a crash between publish and unlink leaves: the manifest already
    // reflects the removal, but the provisional part's bytes are still on disk.
    store.set_stop_after_supersede_publish_for_test(true);
    let n = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(1_000, 1.1)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap();
    assert_eq!(n, 2);
    store.set_stop_after_supersede_publish_for_test(false);

    let id = SeriesId {
        kind: "quote".to_string(),
        venue: "v".to_string(),
        symbol: "S".to_string(),
        interval: None,
        group: None,
        source: None,
    };
    let report = store.rebuild_series_manifest(&id).unwrap();
    assert_eq!(
        report.parts_superseded, 1,
        "the orphaned {{provisional}} part must be recognized as a SUBSET of the canonical \
         part's [canonical, provisional] key set and skipped by the containment rule — not \
         double-counted as a second real part: {report:?}"
    );

    let got = store.scan_quotes("v", "S", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2, "a rebuild must not double-count the orphan: {got:?}");
}

#[test]
fn a_provisional_key_already_folded_into_a_multi_key_part_by_compaction_refuses_rather_than_doubles()
 {
    // The design and the brief both assumed a provisional part is "never compacted before being
    // superseded" — explicitly flagged as "a defensive property, not a requirement the design
    // leans on being true". It is not true: this store's always-on default background maintenance
    // (`run_maintenance`, whose `CompactionConfig::default()` sets `min_parts: 4`) can merge a
    // `date=`'s small parts — including a provisional one — long before its canonical commit
    // arrives. Once that happens, the exact-match removal can never find `{provisional}` again (it
    // now lives inside e.g. `{k1, provisional}`), so this must ERROR rather than silently let the
    // canonical commit double the row.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    store.append_quotes("v", "S", &[qt(0, 1.0)], Some("k1")).unwrap();
    store
        .append_quotes_superseding("v", "S", &[qt(1_000, 1.1)], Some("provisional"), None)
        .unwrap();
    store
        .compact_series(
            "quote",
            "v",
            "S",
            None,
            &CompactionConfig { min_parts: 2, ..Default::default() },
        )
        .unwrap();
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        2,
        "compacted to one multi-key part carrying [k1, provisional]"
    );

    let err = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(2_000, 1.2)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("provisional"),
        "the refusal should name the key it could not supersede: {msg}"
    );

    // Refusing must mean refusing — an error is a clean no-op, not a partial write. The canonical
    // row must NOT have been added either: since the plan/apply split, `plan_supersede` runs
    // BEFORE anything is sealed (see `commit_rows_inner`'s doc — checking after sealing is what let
    // a freshly-sealed part answer its own "already folded away" question), so a refusal here
    // aborts the whole call before `seal_into_manifest` or `publish` ever run.
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        2,
        "a refused supersede must not silently add the canonical rows on top"
    );
}

#[test]
fn a_day_straddling_provisional_commit_is_fully_superseded_not_just_its_first_dated_part() {
    // A commit whose `ts` spans a UTC midnight seals ONE part PER DATE (`seal_into_manifest` groups
    // by `epoch_ms_to_utc_date`), and every part sealed by that ONE commit carries the SAME commit
    // key — so this provisional commit seals TWO parts, both under `commit_keys == ["provisional"]`.
    // `plan_supersede` must collect and remove BOTH, or the one left behind permanently duplicates
    // whatever it holds against the canonical commit's data. This is concretely reachable from
    // Dukascopy: any resample interval that does not evenly divide a day (`7h` gives 21-hour
    // chunks; anything `>= 2d` spans multiple calendar dates by construction) straddles midnight.
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let series_dir = quote_series_dir(&store, "v", "S");

    // ts=90_000_000 is more than a day past ts=0 (see `quote_series_dir`'s sibling tests above for
    // the same day-boundary math), so this ONE commit seals into two `date=` partitions.
    store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(90_000_000, 1.1)],
            Some("provisional"),
            None,
        )
        .unwrap();
    assert_eq!(count_parquet_files(&series_dir), 2, "one part per UTC day the commit touched");

    let n = store
        .append_quotes_superseding(
            "v",
            "S",
            &[qt(0, 1.0), qt(90_000_000, 1.1)],
            Some("canonical"),
            Some("provisional"),
        )
        .unwrap();

    assert_eq!(n, 2);
    assert_eq!(
        store.scan_quotes("v", "S", TsRange::all()).unwrap().len(),
        2,
        "not 3: BOTH provisional-dated parts must be gone, not just the first one plan_supersede \
         happens to find"
    );
    // Per the I3 discipline (check the filesystem, not just `scan_quotes`): the canonical commit
    // also seals two new dated parts, so a disk-level count is what actually distinguishes "both
    // old provisional parts were unlinked" from "one was unlinked and one leaked, invisible to
    // reads but still on disk" — a manifest-only read cannot tell those apart.
    assert_eq!(
        count_parquet_files(&series_dir),
        2,
        "both canonical parts on disk; both provisional-dated parts actually unlinked, not just one"
    );
}
