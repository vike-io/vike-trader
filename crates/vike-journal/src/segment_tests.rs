use super::*;
use crate::testutil::*;
use crate::{CommandJournal, JournalFileConfig};

/// GATE: the warm window must outlast a whole chunk `msync` plus the chunk that follows it.
///
/// This is the property [`warm_ahead_bytes`]'s doc measures, expressed as arithmetic a future
/// edit cannot break silently. The old fixed 8 MiB constant FAILS it at both real segment sizes
/// — which is exactly how the append tail got to 37 µs — and its own doc asserted the opposite
/// ("the cursor never reaches its far edge") in prose, where nothing could check it.
///
/// ⚠ The bound is stated against [`super::sync::sync_chunk_bytes`] rather than against a
/// second copy of 4/16 MiB: the two are one derivation, and pinning a literal here would let
/// `SYNC_CHUNK_DIVISOR` move the chunk out from under the window while this stayed green.
#[cfg(unix)]
#[test]
fn the_warm_window_outlasts_a_chunk_msync_and_the_chunk_after_it() {
    // The two configurations that exist: `JournalConfig::at`'s production segment and the
    // `journal` latency variant's.
    for seg in [64 * 1024 * 1024usize, 256 * 1024 * 1024] {
        let chunk = crate::sync::sync_chunk_bytes(seg as u64);
        let window = warm_ahead_bytes(seg);
        assert!(
            window >= 2 * chunk,
            "a {seg}-byte segment syncs in {chunk}-byte chunks, so its warm window must carry \
                 at least two of them (one to survive the blocking msync, one to reach the next \
                 refresh) — got {window}"
        );
        // ...and it must still be a WINDOW: warming a segment whole cost 694 ms at open, which
        // is what ruled that out. A quarter of the segment keeps that argument intact.
        assert!(
            window <= seg / 4,
            "the window must stay a fraction of the {seg}-byte segment, not approach it — \
                 got {window}"
        );
    }
}

/// GATE: the floor holds for a segment too small for the chunk derivation to matter, and the
/// window is CLAMPED to the mapping rather than handed to `madvise` as a range past its end —
/// which is what makes the floor safe for the KB-sized segments the unit tests use.
///
/// ⚠ It asserts `>= WARM_AHEAD_MIN`, deliberately NOT the exact formula: an equality would
/// restate [`WARM_AHEAD_CHUNKS`] as a literal here, and a constant spelled twice is the thing
/// this file's neighbours keep being wrong about.
#[cfg(unix)]
#[test]
fn a_tiny_segment_still_gets_the_floor_window() {
    assert!(warm_ahead_bytes(4096) >= WARM_AHEAD_MIN);
    assert_eq!(warm_window(16, 4096), Some((16, 4096 - 16)));
    assert_eq!(warm_window(4096, 4096), None);
}

/// `reserve_blocks` must leave the segment BACKED BY REAL BLOCKS, not merely sized.
///
/// The distinction is invisible to `metadata().len()` (a sparse file reports full length), so
/// this asserts on `st_blocks` — the only thing that separates "sized" from "allocated", and
/// the exact property that keeps ext4 from doing block allocation inside an append's write
/// fault. Unix-only: `reserve_blocks` is a no-op elsewhere and there is no portable st_blocks.
///
/// Tolerant by design where it must be: a filesystem that cannot fallocate (tmpfs, some
/// network mounts) legitimately degrades to sparse, and `reserve_blocks` is documented as
/// best-effort — so a genuinely unsupported FS SKIPS rather than fails. `/tmp` on the CI
/// runners is ext4, where it does apply.
#[cfg(unix)]
#[test]
fn segment_blocks_are_reserved_not_sparse() {
    use std::os::unix::fs::MetadataExt;
    let dir = tmp_dir("fallocate");
    let seg = 8 * 1024 * 1024; // 8MB: big enough that sparse-vs-allocated is unambiguous
    let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 256 };
    let j = CommandJournal::open(&dir, cfg).unwrap();
    drop(j);
    let md = std::fs::metadata(seg_path(&dir, 0)).unwrap();
    assert_eq!(md.len(), seg, "segment must be sized to segment_bytes");
    // st_blocks counts 512-byte units regardless of the FS block size.
    let allocated = md.blocks() * 512;
    if allocated < seg / 2 {
        // Under half allocated after a full-length fallocate means the FS ignored it entirely.
        eprintln!("skipping: filesystem did not honor fallocate ({allocated} of {seg} bytes)");
        return;
    }
    assert!(
        allocated >= seg,
        "segment should be fully block-allocated, got {allocated} of {seg} bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The ROLLED segment must be block-reserved too — that is what `prepare_next` buys, and the
/// thing `map_segment(reserve = false)` alone cannot deliver.
///
/// Also pins the crash-safety property the `.prep` suffix exists for: once the roll is done,
/// no `.prep` file is left behind masquerading as a segment.
#[cfg(unix)]
#[test]
fn a_rolled_segment_is_reserved_by_the_prepared_successor() {
    use std::os::unix::fs::MetadataExt;
    let dir = tmp_dir("rollprep");
    // Small enough to roll quickly, large enough that sparse-vs-allocated is unambiguous.
    let seg = 1024 * 1024;
    let cfg = JournalFileConfig { segment_bytes: seg, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg).unwrap();
    // Append past one full segment so at least one roll happens.
    for i in 0..40_000u64 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    assert!(j.seg_idx >= 1, "expected at least one roll, still on segment {}", j.seg_idx);
    let rolled = seg_path(&dir, 1);
    drop(j);

    let md = std::fs::metadata(&rolled).unwrap();
    let allocated = md.blocks() * 512;
    if allocated < seg / 2 {
        eprintln!("skipping: filesystem did not honor fallocate ({allocated} of {seg})");
        return;
    }
    assert!(
        allocated >= seg,
        "rolled segment should be block-allocated by prepare_next, got {allocated} of {seg}"
    );
    // The prepared file must have been RENAMED, not copied/left: no `.prep` may survive for a
    // segment that is now live.
    assert!(!prep_path(&dir, 1).exists(), "a live segment must not leave its .prep behind");
    // And the records must still read back intact across the roll.
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 40_000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `.prep` file must be INVISIBLE to every segment scanner — the property that keeps a crash
/// between preparation and roll from resetting the sequence to zero.
#[test]
fn a_leftover_prep_file_is_not_mistaken_for_a_segment() {
    let dir = tmp_dir("prepscan");
    let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    for i in 0..50 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Simulate the crash window: a prepared successor exists, but the roll never happened.
    std::fs::write(prep_path(&dir, 1), vec![0u8; 64 * 1024]).unwrap();
    // Reopening must resume segment 0 at seq 50 — NOT adopt the empty prepared file as the
    // highest segment and restart the sequence at 0.
    let j2 = CommandJournal::open(&dir, cfg).unwrap();
    assert_eq!(j2.seg_idx, 0, "the .prep file must not be seen as segment 1");
    assert_eq!(j2.next_seq(), 50, "resume must keep the sequence, not restart it");
    drop(j2);
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 50);
    let _ = std::fs::remove_dir_all(&dir);
}
