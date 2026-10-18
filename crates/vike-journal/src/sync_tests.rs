use super::*;
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig};

/// GATE: [`warm_from`] tracks the APPENDER once it is past the watermark.
///
/// Small, and deliberately kept anyway. The alternative is nothing at all: the aim only matters
/// while the appender is running concurrently with a blocking `msync`, which no unit test in
/// this file arranges, so the version that aims at `synced` — the one that cost 37 µs at the
/// append's p99.9 — is green against every other test here.
///
/// ⚠ Declared residual: this pins the DECISION, not the CALL SITE. A future edit that stops
/// calling `warm_from` at all, or passes it `synced` twice, leaves this green. The measurement
/// harness (`journal::writer`'s `measure_append_cost_split`) is what would catch that, and it
/// is `--ignored`.
#[test]
fn the_warm_window_is_aimed_at_the_appender_not_the_watermark() {
    // The real case: the appender ran on for the whole msync and is megabytes ahead.
    assert_eq!(warm_from(20 * 1024 * 1024, 16 * 1024 * 1024), 20 * 1024 * 1024);
    // ...and the watermark is the FLOOR, so a clamped/rebased target cannot aim backwards.
    assert_eq!(warm_from(HEADER, 16 * 1024 * 1024), 16 * 1024 * 1024);
    assert_eq!(warm_from(4096, 4096), 4096);
}

/// The un-REQUESTED debt must stay bounded by the segment's chunk — that bound is what keeps
/// any single `msync` (including the seal, which costs time proportional to whatever is still
/// dirty when it runs) from having to move a whole segment at once.
#[test]
fn unrequested_debt_stays_bounded_while_appending() {
    let dir = tmp_dir("syncdebt");
    // A segment several chunks wide, so the bound is actually exercised rather than trivially
    // satisfied by the segment being smaller than one chunk.
    let seg = (SYNC_CHUNK_MIN * SYNC_CHUNK_DIVISOR as usize * 3) as u64;
    let chunk = sync_chunk_bytes(seg);
    let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let mut worst = 0usize;
    // FAT records (~4KiB framed) so the run crosses the 12MiB chunk in 5 000 appends rather
    // than the ~140 000 the ~90B `ingest` would need. `crossed` below PROVES the crossing
    // happened rather than leaving it to record-size arithmetic that can silently drift — which
    // it did, exactly once, and cost the p99 gate 141ms: see `SYNC_CHUNK_DIVISOR`'s corrected
    // harness note.
    let mut crossed = 0usize;
    for i in 0..5_000u64 {
        let before = j.queued_to;
        j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
        if j.queued_to != before {
            crossed += 1;
        }
        worst = worst.max(j.cursor.saturating_sub(j.queued_to));
    }
    assert!(crossed > 0, "test appended too little to cross a {chunk}-byte chunk at all");
    assert!(
        worst < chunk + 8192,
        "un-requested debt reached {worst} bytes, over the {chunk}-byte bound"
    );
    drop(j);
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A journal whose segment is far from full must issue no chunk sync at all — the property that
/// keeps `SYNC_CHUNK_DIVISOR` a FRACTION of the segment rather than the fixed 4MiB that reached
/// main in #789 and reddened the p99 gate (`max=30235675ns at hop #29241`, 4.18MB in — exactly
/// that fixed chunk's boundary).
///
/// ⚠ This is NO LONGER the `runtime_latency` harness's shape, and the difference is the whole
/// reason the forced sync moved off the append path. The harness writes 100 000 records into a
/// 256MiB segment (chunk = 16MiB): at the ~143B/record of the era this test was written that was
/// 13.6MiB and crossed nothing, but records are now ~287B, so it writes ~27.4MiB and crosses the
/// boundary ONCE — one ~141ms `msync`, on the fold thread, at hop #58 518 of 100 000. Sizing the
/// chunk as a fraction cured the COUNT (3 syncs became 1), never the depth. So this test keeps
/// the small ~90B `ingest` deliberately (≈8.9MiB over 100 000 appends, still short of the 16MiB
/// chunk) and the two `precondition:` asserts below bracket that rather than trusting the
/// estimate: what it pins is the FRACTION RULE, not a claim about the harness.
#[test]
fn a_journal_that_never_rolls_never_forces_a_sync() {
    let dir = tmp_dir("norollsync");
    let cfg = JournalFileConfig { segment_bytes: 256 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let shared = j.sync_shared();
    for i in 0..100_000u64 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    assert!(j.cursor > 4 * 1024 * 1024, "precondition: wrote past a fixed 4MiB chunk");
    assert!(j.cursor < j.sync_chunk, "precondition: stayed under the FRACTIONAL chunk");
    assert_eq!(
        j.queued_to, HEADER,
        "a journal far from rolling must not have REQUESTED any sync (cursor={}, chunk={})",
        j.cursor, j.sync_chunk
    );
    drop(j);
    assert_eq!(
        shared.ranges.load(Ordering::Acquire),
        0,
        "...and the syncer must therefore have performed none"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Resuming must not re-sync the whole existing prefix (that would move the very stall this
/// bounding removes to startup instead — and now onto the syncer, where it would delay the
/// first REAL sync behind a pointless one).
#[test]
fn resume_rebases_the_sync_debt_onto_the_existing_cursor() {
    let dir = tmp_dir("syncresume");
    let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    for i in 0..20_000u64 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let j2 = CommandJournal::open(&dir, cfg).unwrap();
    let shared = j2.sync_shared();
    assert!(j2.cursor > HEADER, "precondition: the resumed segment has records");
    assert_eq!(j2.queued_to, j2.cursor, "resume must owe nothing for already-written bytes");
    assert_eq!(
        shared.synced_to.load(Ordering::Acquire),
        j2.cursor,
        "the syncer must start on the same rebased cursor, not at HEADER"
    );
    drop(j2);
    let _ = std::fs::remove_dir_all(&dir);
}

// -- the syncer thread ------------------------------------------------------------------
//
// What these can and cannot prove. They pin that each of the three `msync` sites (chunk / seal /
// shutdown) still happens, happens ON THE SYNCER (the counters are written by that thread and
// nowhere else), and that the data survives — plus the shutdown-drain guarantee, which is the
// one whose failure mode is silent data loss. What NO unit test here proves is the LATENCY
// claim: "the appender does not wait" needs a slow disk and a measured hop, and the gate for
// that is `tests/runtime_latency.rs`'s journal variant (JOURNAL_MAX_NS / JOURNAL_HOPS_OVER_100US
// / JOURNAL_P999_NS), not this module.

/// Chunk syncs must be performed BY THE SYNCER, and the appender must not wait for them: it
/// advances `queued_to` at the boundary and keeps going.
///
/// Note the deliberately loose upper bound on `ranges`: wakes COALESCE (one un-consumed wake at
/// a time, the work being a monotonic watermark), so a syncer that falls behind legitimately
/// covers several boundaries in one `msync`. Fewer syncs than boundaries is the design working,
/// not a lost sync — which is exactly why the appender never has to block to bound the backlog.
#[test]
fn chunk_syncs_are_performed_by_the_syncer_thread() {
    let dir = tmp_dir("syncthread");
    // 32MiB segment ⇒ chunk = max(2MiB, SYNC_CHUNK_MIN) = 4MiB. ~4KiB records x 5 000 ≈ 19.4MiB
    // ⇒ 4 boundaries crossed, and no roll (19.4MiB < 32MiB).
    let cfg = JournalFileConfig { segment_bytes: 32 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let shared = j.sync_shared();
    let mut crossed = 0usize;
    for i in 0..5_000u64 {
        let before = j.queued_to;
        j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
        if j.queued_to != before {
            assert_eq!(j.queued_to, j.cursor, "the appender must advance without waiting");
            crossed += 1;
        }
    }
    assert!(crossed >= 3, "precondition: expected several chunk crossings, got {crossed}");
    assert_eq!(j.seg_idx, 0, "precondition: this test must not roll");
    let cursor = j.cursor;
    drop(j); // drains + joins the syncer

    let ranges = shared.ranges.load(Ordering::Acquire);
    assert!(
        (1..=crossed as u64).contains(&ranges),
        "expected 1..={crossed} syncer-side chunk syncs, got {ranges}"
    );
    assert_eq!(shared.seals.load(Ordering::Acquire), 0, "no roll happened, so no seal");
    assert_eq!(shared.finals.load(Ordering::Acquire), 1, "exactly one shutdown sync");
    assert_eq!(
        shared.synced_to.load(Ordering::Acquire),
        cursor,
        "the syncer must have caught up to the final watermark before the join returned"
    );
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// GATE: a chunk sync must warm the window TWICE — once BEFORE the blocking `msync` and once
/// after — and the "before" half is the one that carries the fold thread.
///
/// # Why a counter and not a timing test
///
/// The effect of a warm is populated page tables, which no Rust-visible state reports; the only
/// honest observation is a fault count, and that is a whole-process figure a unit test cannot
/// isolate. So [`SyncShared::warms`] records the ACT. What it defends is structural and was
/// missing for real: the refresh used to happen only after `flush_range`, a blocking `msync`
/// measured at 36-141 ms, during which the appender keeps running at ~430 MB/s and leaves the
/// window. Warming after the flush therefore populated pages the appender had ALREADY faulted
/// on. `segment::warm_ahead_bytes` carries the measurement; this asserts the ordering survives.
#[test]
fn a_chunk_sync_warms_the_window_before_the_blocking_msync_as_well_as_after() {
    let dir = tmp_dir("syncwarm");
    // Same shape as `chunk_syncs_are_performed_by_the_syncer_thread`: 32MiB segment ⇒ 4MiB
    // chunk, ~4KiB records x 5 000 ≈ 19.4MiB ⇒ several boundaries, no roll.
    let cfg = JournalFileConfig { segment_bytes: 32 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let shared = j.sync_shared();
    for i in 0..5_000u64 {
        j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    assert_eq!(j.seg_idx, 0, "precondition: this test must not roll");
    drop(j); // drains + joins the syncer

    let ranges = shared.ranges.load(Ordering::Acquire);
    assert!(ranges >= 1, "precondition: expected at least one chunk sync, got {ranges}");
    let warms = shared.warms.load(Ordering::Acquire);
    assert_eq!(
        warms,
        2 * ranges,
        "every chunk sync must warm the window twice — once before `flush_range` and once \
             after — got {warms} warms for {ranges} chunk syncs"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The segment SEAL — `roll`'s old opening `self.map.flush()`, a 292ms whole-segment `msync` on
/// a dirty 64MB segment — must happen on the syncer, and the rolled-to segment must come back
/// with a re-based watermark rather than inheriting the old segment's offset.
#[test]
fn a_roll_seals_the_old_segment_on_the_syncer_thread() {
    let dir = tmp_dir("syncseal");
    // 8MiB segment ⇒ chunk = SYNC_CHUNK_MIN = 4MiB. ~4KiB x 3 000 ≈ 11.6MiB ⇒ exactly one roll.
    let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let shared = j.sync_shared();
    for i in 0..3_000u64 {
        j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    assert!(j.seg_idx >= 1, "precondition: expected a roll, still on segment {}", j.seg_idx);
    assert!(
        j.queued_to < 8 * 1024 * 1024,
        "the rolled-to segment's watermark must be re-based, not carried over"
    );
    let rolls = j.seg_idx;
    drop(j);

    assert_eq!(
        shared.seals.load(Ordering::Acquire),
        rolls,
        "every roll's whole-segment msync must have run on the syncer"
    );
    assert_eq!(shared.finals.load(Ordering::Acquire), 1, "exactly one shutdown sync");
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 3_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The shutdown-drain guarantee.** A journal whose tail never reaches a chunk boundary has had
/// NO sync requested for it at all, so the final sync on `Drop` is the only thing that puts it on
/// disk. Getting this wrong turns a graceful stop into silent data loss, so it is pinned
/// directly: zero chunk syncs, zero seals, exactly one final sync — and the records read back.
#[test]
fn a_graceful_shutdown_syncs_the_untouched_tail() {
    let dir = tmp_dir("synctail");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    let shared = j.sync_shared();
    for i in 0..50u64 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    let cursor = j.cursor;
    assert_eq!(j.queued_to, HEADER, "precondition: the tail must be entirely un-requested");
    assert_eq!(shared.finals.load(Ordering::Acquire), 0, "precondition: not stopped yet");
    drop(j);

    assert_eq!(shared.ranges.load(Ordering::Acquire), 0, "no chunk boundary was crossed");
    assert_eq!(shared.seals.load(Ordering::Acquire), 0, "no roll happened");
    assert_eq!(
        shared.finals.load(Ordering::Acquire),
        1,
        "Drop must drain and sync the tail exactly once"
    );
    assert_eq!(
        shared.synced_to.load(Ordering::Acquire),
        cursor,
        "the final sync must cover everything the appender wrote"
    );
    // ...and the records are actually there.
    let j2 = CommandJournal::open(&dir, cfg).unwrap();
    assert_eq!(j2.next_seq(), 50);
    drop(j2);
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 50);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE syncer thread serves the directory for its whole life: `roll` MOVES the handle to the
/// successor instead of stopping one thread and starting another. Pinned through the shared
/// cell's identity — if `roll` re-spawned, the successor would carry a fresh `SyncShared` and
/// the counters observed here would be zero.
#[test]
fn the_syncer_is_carried_across_a_roll_not_respawned() {
    let dir = tmp_dir("synccarry");
    let cfg = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    let before = j.sync_shared();
    for i in 0..3_000u64 {
        j.append_cmd(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    assert!(j.seg_idx >= 1, "precondition: expected a roll, still on segment {}", j.seg_idx);
    let after = j.sync_shared();
    assert!(Arc::ptr_eq(&before, &after), "the roll must carry the SAME syncer, not respawn");
    drop(j);
    assert_eq!(
        before.finals.load(Ordering::Acquire),
        1,
        "one thread for the journal's life ⇒ exactly one shutdown sync"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
