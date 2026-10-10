//! The duplicate-instance interlock tests: `JournalLock` itself, then `CommandJournal::open`.

use super::*;

use crate::format::VERSION;
use crate::record::record_seq;
use crate::testutil::{ingest, seg_files, small_cfg, tmp_dir};
use crate::{CommandJournal, JournalFileConfig};

/// A first acquire on a fresh directory succeeds and materializes the sentinel.
#[test]
fn acquire_creates_the_sentinel_and_succeeds() {
    let dir = tmp_dir("acquire");
    let g = JournalLock::acquire(&dir).unwrap();
    assert!(dir.join(LOCK_FILE).exists(), "the sentinel file is created inside the dir");
    drop(g);
}

/// A second acquire while the first guard LIVES is refused, with an actionable error that
/// names the directory.
#[test]
fn a_second_acquire_while_the_first_lives_is_refused() {
    let dir = tmp_dir("contend");
    let first = JournalLock::acquire(&dir).unwrap();
    let err = JournalLock::acquire(&dir).expect_err("the second acquire must be refused");
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse, "refusal is AddrInUse, not a generic io");
    let msg = err.to_string();
    let want = dir.display().to_string();
    assert!(msg.contains(want.as_str()), "the error names the journal dir: {msg}");
    drop(first);
}

/// The lock releases on drop — a LEFTOVER sentinel file is not a stale lock, so the next
/// instance starts cleanly (the crash-restart case).
#[test]
fn the_lock_releases_on_drop_and_a_leftover_sentinel_is_not_stale() {
    let dir = tmp_dir("release");
    let first = JournalLock::acquire(&dir).unwrap();
    drop(first);
    assert!(dir.join(LOCK_FILE).exists(), "the sentinel file survives (never unlinked)");
    let second = JournalLock::acquire(&dir).expect("a released lock re-acquires");
    drop(second);
}

/// Two DIFFERENT directories lock independently — the interlock is per journal dir, not global.
#[test]
fn two_different_dirs_lock_independently() {
    let a = tmp_dir("indep-a");
    let b = tmp_dir("indep-b");
    let ga = JournalLock::acquire(&a).unwrap();
    let gb = JournalLock::acquire(&b).expect("a different dir is unaffected");
    drop(ga);
    drop(gb);
}

// ---- duplicate-instance interlock (see `crate::lock`) ---------------------------------------

/// (a) A FIRST open of a fresh directory succeeds and takes the directory lock.
#[test]
fn first_open_succeeds_and_takes_the_directory_lock() {
    let dir = tmp_dir("lock-first");
    let j = CommandJournal::open(&dir, small_cfg()).unwrap();
    assert!(
        dir.join(crate::lock::LOCK_FILE).exists(),
        "opening a journal materializes the interlock sentinel next to the segments"
    );
    assert_eq!(j.next_seq(), 0, "a fresh journal still starts at seq 0");
    drop(j);
}

/// (b) A SECOND open of the SAME directory while the first journal is ALIVE is refused — fast,
/// with an actionable error naming the directory, never a silent second writer. This is the
/// double-launch case: two writers interleave frames and break the replay determinism fence.
#[test]
fn a_second_open_of_the_same_dir_is_refused_while_the_first_lives() {
    let dir = tmp_dir("lock-second");
    let first = CommandJournal::open(&dir, small_cfg()).unwrap();
    // `CommandJournal` holds an `MmapMut` (no `Debug`), so `unwrap_err()` isn't available —
    // match, exactly as `open_refuses_to_resume_a_segment_with_an_unsupported_version` does.
    match CommandJournal::open(&dir, small_cfg()) {
        Ok(_) => panic!(
            "a second writer on one journal directory must be refused — two mmap writers \
                 interleave WAL frames and corrupt the determinism fence"
        ),
        Err(e) => {
            assert_eq!(
                e.kind(),
                io::ErrorKind::AddrInUse,
                "the refusal is the distinctive AddrInUse, not a generic io error"
            );
            let msg = e.to_string();
            let want = dir.display().to_string();
            assert!(msg.contains(want.as_str()), "the error names the journal dir: {msg}");
        }
    }
    drop(first);
}

/// (c) After the first guard DROPS the lock is released, so a later open of the same directory
/// succeeds and resumes the sequence. The leftover `LOCK` file is not a stale lock — which is
/// exactly the crash-restart shape (a killed process's handle is closed by the OS).
#[test]
fn a_new_open_succeeds_after_the_first_journal_drops() {
    let dir = tmp_dir("lock-release");
    let mut first = CommandJournal::open(&dir, small_cfg()).unwrap();
    first.append_cmd(1_000, &ingest(0)).unwrap();
    drop(first);
    assert!(
        dir.join(crate::lock::LOCK_FILE).exists(),
        "the sentinel FILE survives the drop (only the lock is released)"
    );
    let second = CommandJournal::open(&dir, small_cfg())
        .expect("a released directory re-opens — a leftover sentinel is never a stale lock");
    assert_eq!(second.next_seq(), 1, "the re-opened journal resumes the sequence");
    drop(second);
}

/// (d) Two DIFFERENT directories open fine at the same time — the interlock is per journal
/// directory, not a process-global singleton (multi-core/multi-profile sessions still work).
#[test]
fn two_different_journal_dirs_open_concurrently() {
    let a = tmp_dir("lock-dir-a");
    let b = tmp_dir("lock-dir-b");
    let mut ja = CommandJournal::open(&a, small_cfg()).unwrap();
    let mut jb = CommandJournal::open(&b, small_cfg())
        .expect("a DIFFERENT journal dir is unaffected by the first one's lock");
    ja.append_cmd(1_000, &ingest(0)).unwrap();
    jb.append_cmd(1_000, &ingest(1)).unwrap();
    drop(ja);
    drop(jb);
    assert_eq!(CommandJournal::read_all(&a).unwrap().len(), 1);
    assert_eq!(CommandJournal::read_all(&b).unwrap().len(), 1);
}

/// The OFF/unchanged path: the interlock adds a sibling sentinel FILE and NOTHING else. No
/// journal record, no segment byte, no new dir entry any reader sees — `read_all`,
/// `latest_segment_version` and `prune_before_latest_snap` all key off the `journal-*.vjl`
/// name, so the recorded stream (and therefore every `state_hash`) is byte-for-byte what it was
/// before the lock existed. Pruning likewise never touches the sentinel.
#[test]
fn the_interlock_adds_no_record_and_is_invisible_to_every_reader() {
    let dir = tmp_dir("lock-inert");
    let mut j = CommandJournal::open(&dir, small_cfg()).unwrap();
    for i in 0..5 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    assert!(dir.join(crate::lock::LOCK_FILE).exists(), "the sentinel is present");
    assert_eq!(seg_files(&dir).len(), 1, "the sentinel is NOT counted as a segment");
    let back = CommandJournal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 5, "the interlock appends no record of its own");
    let seqs: Vec<u64> = back.iter().map(record_seq).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4], "seqs are exactly what they were before the lock");
    assert_eq!(
        CommandJournal::latest_segment_version(&dir).unwrap(),
        Some(VERSION),
        "the sentinel is skipped by the version probe (it carries no MAGIC)"
    );

    // No Snap anywhere ⇒ prune is a no-op, and it never removes the sentinel.
    CommandJournal::prune_before_latest_snap(&dir).unwrap();
    assert_eq!(seg_files(&dir).len(), 1);
    assert!(dir.join(crate::lock::LOCK_FILE).exists(), "prune leaves the sentinel");
    assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 5);
}

/// A segment ROLL must CARRY the directory lock, not drop it: `roll` rebuilds the whole
/// `CommandJournal` and assigns it over `*self`, so a naive swap would release the interlock
/// mid-session and let a second instance in. Roll the journal, then prove a concurrent open is
/// STILL refused while the rolled journal lives.
#[test]
fn the_lock_survives_a_segment_roll() {
    let dir = tmp_dir("lock-roll");
    let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = CommandJournal::open(&dir, cfg.clone()).unwrap();
    for i in 0..200 {
        j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
    }
    assert!(seg_files(&dir).len() >= 2, "precondition: the journal actually rolled");
    match CommandJournal::open(&dir, cfg.clone()) {
        Ok(_) => panic!("the interlock must still be held after a segment roll"),
        Err(e) => assert_eq!(e.kind(), io::ErrorKind::AddrInUse),
    }
    drop(j);
    // and it IS released once the rolled journal drops
    let reopened = CommandJournal::open(&dir, cfg).expect("released after the rolled drop");
    drop(reopened);
}
