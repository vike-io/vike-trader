use super::*;
use crate::format::HEADER;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};

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
    let config = JournalFileConfig { segment_bytes: seg, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let mut worst = 0usize;
    // FAT records (~4KiB framed) so the run crosses the 12MiB chunk in 5 000 appends rather
    // than the ~140 000 the ~90B `ingest` would need. `crossed` below PROVES the crossing
    // happened rather than leaving it to record-size arithmetic that can silently drift — which
    // it did, exactly once, and cost the p99 gate 141ms: see `SYNC_CHUNK_DIVISOR`'s corrected
    // harness note.
    let mut crossed = 0usize;
    for i in 0..5_000u64 {
        let before = j.queued_to;
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
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
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 5_000);
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
    let config = JournalFileConfig { segment_bytes: 256 * 1024 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let shared = j.sync_shared();
    for i in 0..100_000u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
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
    let config = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    for i in 0..20_000u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let j2 = Journal::open(&dir, config).unwrap();
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
    let config = JournalFileConfig { segment_bytes: 32 * 1024 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let shared = j.sync_shared();
    let mut crossed = 0usize;
    for i in 0..5_000u64 {
        let before = j.queued_to;
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
        if j.queued_to != before {
            assert_eq!(j.queued_to, j.cursor, "the appender must advance without waiting");
            crossed += 1;
        }
    }
    assert!(crossed >= 3, "precondition: expected several chunk crossings, got {crossed}");
    assert_eq!(j.segment_index, 0, "precondition: this test must not roll");
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
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 5_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// GATE (the asynchronous journal, decision 0279): [`crate::WriteFence::wait`] returns only once every
/// frame appended so far is in the segment FILE — read back through an independent `read`, never
/// through the stage — across a roll too. This is the safety property an order rides on: the core
/// sends nothing that leaves the process before this wait returned.
///
/// (It replaced the gate that a chunk `msync` warms the mapping twice: the core writes no file
/// mapping any more, and the stage it writes is warmed by the sync thread, counted in `warms`.)
#[test]
fn a_write_fence_returns_only_once_the_frames_are_in_the_file() {
    let dir = tmp_dir("fence");
    let config = JournalFileConfig { segment_bytes: 256 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let fence = j.write_fence();
    for i in 0..6_000u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        if i % 97 == 0 {
            fence.wait().unwrap();
            let bytes = std::fs::read(crate::segment::segment_path(&dir, j.segment_index)).unwrap();
            let (end, _) = crate::frame::walk_frames(&bytes, HEADER, |_| {});
            assert_eq!(end, j.cursor, "after the fence the file holds every frame appended ({i})");
        }
    }
    assert!(
        j.segment_index >= 1,
        "precondition: the run must roll, still on segment {}",
        j.segment_index
    );
    drop(j);
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 6_000);
}

/// **An execution report is a LAZY frame** (`crate::handoff`'s module doc): a fence does not wait for it, the
/// next eager frame carries it into the file, a report no record follows reaches the file on the write thread's
/// own, and its bytes are the eager encoding's exactly.
#[test]
fn an_execution_report_rides_the_next_frame_and_no_fence_waits_for_it() {
    let dir = tmp_dir("lazy-report");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let shared = j.sync_shared();
    let fence = j.write_fence();
    let file_end = |j: &Journal| {
        let bytes = std::fs::read(crate::segment::segment_path(&dir, j.segment_index)).unwrap();
        crate::frame::walk_frames(&bytes, HEADER, |_| {}).0
    };
    let report = |n: u64| vike_model::orders::ExecutionReport {
        caused_by_sequence: n,
        at_ms: 1_000,
        ownership: vike_model::orders::ExecutionOwnership::Ours,
        account: Some("DEFAULT".into()),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        order_client_id: format!("op{n}"),
        venue_order_id: None,
        mount_id: None,
        strategy_code: None,
        order: None,
        fill: None,
        fee_correction: None,
        reason: None,
    };
    let command = j.append_command(1_000, &ingest(0)).unwrap();
    let eager_end = j.cursor;
    let start = j.cursor;
    let first = j.append_execution_report(1_001, &report(command)).unwrap();
    assert_eq!(
        shared.appended.load(Ordering::Acquire),
        crate::handoff::pos(0, eager_end),
        "a fence's target stops at the last eager frame"
    );
    assert_eq!(
        shared.staged_end(),
        crate::handoff::pos(0, j.cursor),
        "the stage ends past the report"
    );
    fence.wait().unwrap();
    assert!(file_end(&j) >= eager_end, "the fence still covers every eager frame");

    // the next eager frame carries the report in its own hand-off
    j.append_command(1_002, &ingest(1)).unwrap();
    fence.wait().unwrap();
    assert_eq!(
        file_end(&j),
        j.cursor,
        "the fence after an eager frame covers the report before it"
    );
    let bytes = std::fs::read(crate::segment::segment_path(&dir, j.segment_index)).unwrap();
    let mut eager = Vec::new();
    crate::frame::encode_into(
        &mut eager,
        &crate::record::JournalRecordRef::ExecutionReport {
            sequence: first,
            now_ms: 1_001,
            report: &report(command),
        },
    );
    assert_eq!(
        &bytes[start + 8..start + 8 + eager.len()],
        eager.as_slice(),
        "the eager encoding, byte for byte"
    );

    // a report no record follows reaches the file on the write thread's own
    j.append_execution_report(1_003, &report(command + 2)).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while file_end(&j) < j.cursor {
        assert!(std::time::Instant::now() < deadline, "the lazy tail never reached the file");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    // and a lazy tail at the stop is drained like every frame
    j.append_execution_report(1_004, &report(command + 3)).unwrap();
    drop(j);
    let read = Journal::read_all(&dir).unwrap();
    assert_eq!(read.len(), 5);
    assert!(matches!(read[4], crate::JournalRecord::ExecutionReport { now_ms: 1_004, .. }));
}

/// The segment SEAL — `roll`'s old opening `self.map.flush()`, a 292ms whole-segment `msync` on
/// a dirty 64MB segment — must happen on the syncer, and the rolled-to segment must come back
/// with a re-based watermark rather than inheriting the old segment's offset.
#[test]
fn a_roll_seals_the_old_segment_on_the_syncer_thread() {
    let dir = tmp_dir("syncseal");
    // 8MiB segment ⇒ chunk = SYNC_CHUNK_MIN = 4MiB. ~4KiB x 3 000 ≈ 11.6MiB ⇒ exactly one roll.
    let config = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let shared = j.sync_shared();
    for i in 0..3_000u64 {
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    assert!(
        j.segment_index >= 1,
        "precondition: expected a roll, still on segment {}",
        j.segment_index
    );
    assert!(
        j.queued_to < 8 * 1024 * 1024,
        "the rolled-to segment's watermark must be re-based, not carried over"
    );
    let rolls = j.segment_index;
    drop(j);

    assert_eq!(
        shared.seals.load(Ordering::Acquire),
        rolls,
        "every roll's whole-segment msync must have run on the syncer"
    );
    assert_eq!(shared.finals.load(Ordering::Acquire), 1, "exactly one shutdown sync");
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 3_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The shutdown-drain guarantee.** A journal whose tail never reaches a chunk boundary has had
/// NO sync requested for it at all, so the final sync on `Drop` is the only thing that puts it on
/// disk. Getting this wrong turns a graceful stop into silent data loss, so it is pinned
/// directly: zero chunk syncs, zero seals, exactly one final sync — and the records read back.
#[test]
fn a_graceful_shutdown_syncs_the_untouched_tail() {
    let dir = tmp_dir("synctail");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    let shared = j.sync_shared();
    for i in 0..50u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
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
    let j2 = Journal::open(&dir, config).unwrap();
    assert_eq!(j2.next_sequence(), 50);
    drop(j2);
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 50);
    let _ = std::fs::remove_dir_all(&dir);
}

/// ONE syncer thread serves the directory for its whole life: `roll` MOVES the handle to the
/// successor instead of stopping one thread and starting another. Pinned through the shared
/// cell's identity — if `roll` re-spawned, the successor would carry a fresh `JournalSyncShared` and
/// the counters observed here would be zero.
#[test]
fn the_syncer_is_carried_across_a_roll_not_respawned() {
    let dir = tmp_dir("synccarry");
    let config = JournalFileConfig { segment_bytes: 8 * 1024 * 1024, flush_every: 256 };
    let mut j = Journal::open(&dir, config).unwrap();
    let before = j.sync_shared();
    for i in 0..3_000u64 {
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    assert!(
        j.segment_index >= 1,
        "precondition: expected a roll, still on segment {}",
        j.segment_index
    );
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

/// **A step at or below the base raises nothing and hands off no stamp** (decision 0279's counters). Format
/// 30 made the base the write version, so every lazy step is below it: the appender sends the write and
/// sync threads no stamp and the file keeps the base past the fence. (Until format 30 this pinned one
/// stamp handed off per raise; the hand-off path waits for the next step above the base.)
#[test]
fn a_step_at_or_below_the_base_hands_off_no_stamp() {
    let dir = tmp_dir("syncheader");
    let mut j = Journal::open(&dir, small_config()).unwrap();
    let shared = j.sync_shared();
    j.append_command(1_000, &ingest(0)).unwrap();
    j.stamp_order_list_version();
    j.stamp_order_list_version();
    assert_eq!(shared.stamps_sent.load(Ordering::Acquire), 0, "no raise, no stamp handed off");
    j.write_fence().wait().unwrap();
    assert_eq!(shared.stamps_done.load(Ordering::Acquire), 0, "the write thread wrote none");
    let seg = crate::segment::segment_path(&dir, 0);
    let stamp = || u32::from_le_bytes(std::fs::read(&seg).unwrap()[4..8].try_into().unwrap());
    assert_eq!(stamp(), crate::format::BASE_VERSION, "the file keeps the base past the fence");
    j.append_command(1_001, &ingest(1)).unwrap();
    drop(j); // drains the write thread, then joins the sync thread

    assert_eq!(stamp(), crate::format::BASE_VERSION);
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 2);
}

/// Poll `watch` until its mark reaches `want`, failing after a bounded wait.
fn wait_for_mark(watch: &JournalDurableWatch, want: JournalDurableMark) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while watch.durable_mark() < want {
        assert!(std::time::Instant::now() < deadline, "the mark stayed short of {want:?}");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(watch.durable_mark(), want);
}

/// THE DURABLE MARK, the execution stream's bound: an append alone does not move it, even once the
/// working file holds the frame; a sync does, to the end of what it synced, whoever asked for it. A
/// roll moves it into the new segment once the old one is sealed, a closed journal's mark covers
/// every segment, and a resumed journal starts it at its resume point. The `drop` returning at all
/// is the other half: a watch holds no sender, so it never keeps the sync thread alive.
#[test]
fn the_durable_mark_moves_only_with_a_sync() {
    let dir = tmp_dir("durable-mark");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    let watch = j.durable_watch();
    let start = JournalDurableMark { segment_index: 0, offset: HEADER };
    assert_eq!(watch.durable_mark(), start, "a fresh journal: nothing past the header");

    for i in 0..3 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.write_fence().wait().unwrap();
    assert_eq!(watch.durable_mark(), start, "in the working file is not durable");
    j.flush().unwrap();
    assert_eq!(watch.durable_mark(), JournalDurableMark { segment_index: 0, offset: j.cursor });

    // Asked from another thread, as the publisher asks.
    j.append_command(1_003, &ingest(3)).unwrap();
    std::thread::scope(|s| {
        s.spawn(|| watch.request_sync());
    });
    wait_for_mark(&watch, JournalDurableMark { segment_index: 0, offset: j.cursor });

    let mut n = 4;
    while j.segment_index == 0 {
        j.append_command(1_000 + n as i64, &ingest(n)).unwrap();
        n += 1;
    }
    j.flush().unwrap();
    wait_for_mark(&watch, JournalDurableMark { segment_index: 1, offset: j.cursor });

    let last = j.segment_index;
    drop(j);
    assert_eq!(
        watch.durable_mark(),
        JournalDurableMark { segment_index: last + 1, offset: 0 },
        "a closed journal is durable whole"
    );
    watch.request_sync(); // a no-op once closed

    let resumed = Journal::open(&dir, config).unwrap();
    assert_eq!(
        resumed.durable_watch().durable_mark(),
        JournalDurableMark { segment_index: last, offset: resumed.cursor },
        "a resumed journal is durable up to where it resumed"
    );
}

/// **A failed seal is retried through the segment's durable copy, never by a second `fsync` of the working file**
/// (`retry_unsealed`): the next message brings the retired copy up to the segment's end, the copy then stands
/// for the segment (it stays on disk for the next boot's repair) and the durable mark moves past it. A copy that
/// cannot be completed is retried [`SEAL_RETRIES`] times, then said and left as it was.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_seal_is_retried_through_the_durable_copy() {
    let dir = tmp_dir("failed-seal");
    let mut bytes = memmap2::MmapMut::map_anon(4096).unwrap();
    bytes[..64].fill(7);
    let stage = std::sync::Arc::new(memmap2::MmapRaw::from(bytes));
    let copy = |index: u64, ordinal: u64, file: Option<File>| Copy {
        index,
        ordinal,
        stage: std::sync::Arc::clone(&stage),
        file,
        off: 0,
        end: Some(64),
    };
    let mut live = Copy { end: None, ..copy(2, 1, None) };
    let mut retired = vec![copy(1, 0, Some(create_durable(&dir, 1).unwrap()))];
    let mut sealed_early = Vec::new();
    // `fsync` of a character device fails (EINVAL): the shape of a working file whose seal failed.
    let failing = std::fs::OpenOptions::new().write(true).open("/dev/null").unwrap();
    assert!(!seal(&dir, &failing, 1, &mut live, &mut retired, &mut sealed_early));
    assert_eq!(durable_position(&live, &retired, &sealed_early), pos(0, 0), "the mark waits at it");

    let mut unsealed = vec![Unsealed { index: 1, tries: 0 }];
    retry_unsealed(&mut unsealed, &mut retired, live.index);
    assert!(unsealed.is_empty() && retired.is_empty(), "the copy holds the segment whole");
    assert_eq!(
        std::fs::read(crate::durable::durable_path(&dir, 1)).unwrap(),
        vec![7u8; 64],
        "and stays for the next boot's repair"
    );
    assert_eq!(
        durable_position(&live, &retired, &sealed_early),
        pos(1, 0),
        "the mark moved past it"
    );

    // No copy was made for this one: nothing can complete it.
    let mut retired = vec![copy(3, 2, None)];
    let mut unsealed = vec![Unsealed { index: 3, tries: 0 }];
    for _ in 1..SEAL_RETRIES {
        retry_unsealed(&mut unsealed, &mut retired, live.index);
    }
    assert_eq!(unsealed.len(), 1, "still retried inside its bound");
    retry_unsealed(&mut unsealed, &mut retired, live.index);
    assert!(unsealed.is_empty(), "given up at its bound");
    assert_eq!(retired.len(), 1, "and left as it was");
}
