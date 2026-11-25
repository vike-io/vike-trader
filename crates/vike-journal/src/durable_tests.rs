//! The durable copy and the boot repair (decision 0279). Each crash
//! state is CRAFTED from a clean reference run's segment bytes — what a power cut can leave on disk:
//! a working file that lost its newest pages (shorter, or zeroed), a torn last frame, a copy longer
//! or shorter than the working file, no copy at all, a crash between a roll's two files, a crash
//! mid-repair. Each repaired directory is then compared BYTE FOR BYTE with the reference and read
//! back record for record, which is what a replay reads.

use super::*;
use crate::format::HEADER;
use crate::frame::walk_frames;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};

/// A segment large enough that the reference does not roll.
const SEGMENT: u64 = 1024 * 1024;

fn config() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: SEGMENT, flush_every: 64 }
}

/// A clean run of `n` records: the dir, and its one segment's bytes. No copy outlives the run.
fn reference(tag: &str, n: u64) -> (Scratch, Vec<u8>) {
    let dir = tmp_dir(tag);
    let mut j = Journal::open(&dir, config()).unwrap();
    for i in 0..n {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    assert!(!durable_path(&dir, 0).exists(), "a clean stop leaves no durable copy");
    let bytes = std::fs::read(segment_path(&dir, 0)).unwrap();
    (dir, bytes)
}

/// The end of the whole frames in `b`.
fn frames_end(b: &[u8]) -> usize {
    walk_frames(b, HEADER, |_| {}).0
}

/// The offset where frame number `k` starts in `b`.
fn frame_start(b: &[u8], k: usize) -> usize {
    let mut at = HEADER;
    for _ in 0..k {
        let len = u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        at += 8 + len;
    }
    at
}

/// A fresh directory holding the given working and copy bytes for segment 0 (`None` = no file).
fn crafted(tag: &str, work: Option<&[u8]>, durable: Option<&[u8]>) -> Scratch {
    let dir = tmp_dir(tag);
    if let Some(w) = work {
        std::fs::write(segment_path(&dir, 0), w).unwrap();
    }
    if let Some(d) = durable {
        std::fs::write(durable_path(&dir, 0), d).unwrap();
    }
    dir
}

/// `bytes` with everything from `from` on zeroed: the pages a power cut lost.
fn zeroed_from(bytes: &[u8], from: usize) -> Vec<u8> {
    let mut b = bytes.to_vec();
    b[from..].fill(0);
    b
}

fn records(dir: &std::path::Path) -> usize {
    Journal::read_all(dir).unwrap().len()
}

#[test]
fn a_working_file_that_lost_its_tail_is_repaired_from_the_copy() {
    let (r, good) = reference("dur-ref-lost", 300);
    let end = frames_end(&good);
    let lost = zeroed_from(&good, frame_start(&good, 200));
    let dir = crafted("dur-lost", Some(&lost), Some(&good[..end]));
    let notes = repair_from_durable_copies(&dir).unwrap();
    assert_eq!(notes.len(), 1, "one segment repaired: {notes:?}");
    assert_eq!(
        std::fs::read(segment_path(&dir, 0)).unwrap(),
        good,
        "byte-identical to the clean run"
    );
    assert!(!durable_path(&dir, 0).exists(), "the copy is deleted once the working file is synced");
    assert_eq!(replayed(&dir), replayed(&r), "the replay after the repair is the clean run's");
}

#[test]
fn a_truncated_working_file_is_regrown_from_the_copy() {
    let (_r, good) = reference("dur-ref-trunc", 300);
    let end = frames_end(&good);
    let cut = frame_start(&good, 120);
    let dir = crafted("dur-trunc", Some(&good[..cut]), Some(&good[..end]));
    repair_from_durable_copies(&dir).unwrap();
    let work = std::fs::read(segment_path(&dir, 0)).unwrap();
    assert_eq!(work[..], good[..work.len()], "every byte the repair wrote is the clean run's");
    assert!(good[work.len()..].iter().all(|&b| b == 0), "and nothing past it was written");
    assert_eq!(records(&dir), 300);
}

#[test]
fn a_torn_last_frame_is_replaced_by_the_copys_whole_frame() {
    let (_r, good) = reference("dur-ref-torn", 300);
    let end = frames_end(&good);
    let last = frame_start(&good, 299);
    let mut torn = good.clone();
    torn[last + 10] ^= 0xFF; // the length stands, the payload does not match its check
    let dir = crafted("dur-torn", Some(&torn), Some(&good[..end]));
    repair_from_durable_copies(&dir).unwrap();
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert_eq!(records(&dir), 300);
}

#[test]
fn a_frame_torn_in_both_files_is_never_accepted() {
    let (_r, good) = reference("dur-ref-torn2", 300);
    let last = frame_start(&good, 299);
    let mut torn = good.clone();
    torn[last + 10] ^= 0xFF;
    // The copy's last frame is torn too (its sync did not complete): only its whole frames count.
    let dir = crafted("dur-torn2", Some(&torn), Some(&torn[..frames_end(&good)]));
    repair_from_durable_copies(&dir).unwrap();
    assert_eq!(
        std::fs::read(segment_path(&dir, 0)).unwrap(),
        torn,
        "nothing to take, nothing written"
    );
    assert_eq!(records(&dir), 299, "the torn frame is not read");
}

#[test]
fn a_copy_shorter_than_the_working_file_changes_nothing() {
    let (_r, good) = reference("dur-ref-short", 300);
    let dir = crafted("dur-short", Some(&good), Some(&good[..frame_start(&good, 100)]));
    let notes = repair_from_durable_copies(&dir).unwrap();
    assert!(notes.is_empty(), "the working file already holds more: {notes:?}");
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert!(!durable_path(&dir, 0).exists(), "a stale copy is deleted all the same");
}

#[test]
fn a_missing_copy_leaves_the_working_file_as_it_is() {
    let (_r, good) = reference("dur-ref-none", 300);
    let lost = zeroed_from(&good, frame_start(&good, 200));
    let dir = crafted("dur-none", Some(&lost), None);
    assert!(repair_from_durable_copies(&dir).unwrap().is_empty());
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), lost);
    assert_eq!(records(&dir), 200, "what the disk kept, and nothing torn");
}

#[test]
fn a_stale_copy_left_between_a_seal_and_its_delete_is_dropped() {
    let (_r, good) = reference("dur-ref-stale", 300);
    let dir = crafted("dur-stale", Some(&good), Some(&good[..frames_end(&good)]));
    assert!(repair_from_durable_copies(&dir).unwrap().is_empty());
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert!(!durable_path(&dir, 0).exists());
}

#[test]
fn a_working_file_without_a_header_takes_the_copys() {
    let (_r, good) = reference("dur-ref-hdr", 300);
    let lost = zeroed_from(&good, 0); // not even the header page reached the disk
    let dir = crafted("dur-hdr", Some(&lost), Some(&good[..frames_end(&good)]));
    repair_from_durable_copies(&dir).unwrap();
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert_eq!(records(&dir), 300);
}

#[test]
fn a_version_the_copy_raised_is_raised_in_the_working_file() {
    let (_r, good) = reference("dur-ref-ver", 300);
    // Format 30 made every segment the write version from its first byte, so no raise can reach a copy
    // past the base: the WORKING file keeps a stamp one below, the copy holds the raised one.
    let copy = good[..frames_end(&good)].to_vec();
    let raised = u32::from_le_bytes(good[4..8].try_into().unwrap());
    let mut lost = zeroed_from(&good, frame_start(&good, 250));
    lost[4..8].copy_from_slice(&(raised - 1).to_le_bytes());
    let dir = crafted("dur-ver", Some(&lost), Some(&copy));
    repair_from_durable_copies(&dir).unwrap();
    let work = std::fs::read(segment_path(&dir, 0)).unwrap();
    assert_eq!(u32::from_le_bytes(work[4..8].try_into().unwrap()), raised);
    assert_eq!(work, good, "the version and the lost frames came back from the copy");
}

#[test]
fn a_resumed_segments_copy_repairs_above_its_hole() {
    let (_r, good) = reference("dur-ref-hole", 300);
    let end = frames_end(&good);
    // A copy made at a resume: nothing below the resume point (frame 150), the rest as written.
    let base = frame_start(&good, 150);
    let mut copy = vec![0u8; end];
    copy[base..end].copy_from_slice(&good[base..end]);
    let lost = zeroed_from(&good, frame_start(&good, 220));
    let dir = crafted("dur-hole", Some(&lost), Some(&copy));
    repair_from_durable_copies(&dir).unwrap();
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
}

#[test]
fn a_crash_mid_repair_is_repaired_again() {
    let (_r, good) = reference("dur-ref-again", 300);
    let end = frames_end(&good);
    // The first repair wrote part of the span (up to a frame AND into the next one) and died before
    // its sync and its delete: the copy is still there.
    let half = zeroed_from(&good, frame_start(&good, 240) + 5);
    let dir = crafted("dur-again", Some(&half), Some(&good[..end]));
    repair_from_durable_copies(&dir).unwrap();
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert!(repair_from_durable_copies(&dir).unwrap().is_empty(), "idempotent");
}

#[test]
fn a_crash_mid_roll_repairs_both_segments_and_makes_the_missing_one() {
    // A clean two-segment run.
    let small = JournalFileConfig { segment_bytes: 16 * 1024, flush_every: 64 };
    let r = tmp_dir("dur-ref-roll");
    let mut j = Journal::open(&r, small.clone()).unwrap();
    // 400 cancels: ~30 KB of msgpack frames (format 34's are smaller than the JSON ones were), two segments.
    for i in 0..400u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    assert!(j.segment_index >= 1, "precondition: the reference rolls");
    let last = j.segment_index;
    drop(j);
    let segment0 = std::fs::read(segment_path(&r, 0)).unwrap();
    let segl = std::fs::read(segment_path(&r, last)).unwrap();
    // The crash: segment 0 lost its tail (its seal never ran, so its copy is there), the last
    // segment's working file was never made (its copy was: the core tells the sync thread at the
    // roll, before the write thread makes the file).
    let dir = tmp_dir("dur-roll");
    for i in 1..last {
        std::fs::copy(segment_path(&r, i), segment_path(&dir, i)).unwrap();
    }
    std::fs::write(segment_path(&dir, 0), zeroed_from(&segment0, frame_start(&segment0, 10)))
        .unwrap();
    std::fs::write(durable_path(&dir, 0), &segment0[..frames_end(&segment0)]).unwrap();
    std::fs::write(durable_path(&dir, last), &segl[..frames_end(&segl)]).unwrap();
    let notes = repair_from_durable_copies(&dir).unwrap();
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), segment0);
    let made = std::fs::read(segment_path(&dir, last)).unwrap();
    assert_eq!(made[..], segl[..made.len()]);
    assert_eq!(replayed(&dir), replayed(&r), "the replay reads exactly the clean run's records");
}

/// What a replay reads, record by record, as bytes (`JournalRecord` has no `PartialEq`).
fn replayed(dir: &std::path::Path) -> Vec<String> {
    Journal::read_all(dir).unwrap().iter().map(|r| serde_json::to_string(r).unwrap()).collect()
}

/// The whole cycle through the public door: `open` repairs before it resumes, appends go on, and a
/// clean stop leaves the directory as the clean run left it, byte for byte.
#[test]
fn open_repairs_before_it_resumes_and_the_result_is_the_clean_run() {
    let (_r, good) = reference("dur-ref-open", 300);
    let end = frames_end(&good);
    let lost = zeroed_from(&good, frame_start(&good, 180));
    let dir = crafted("dur-open", Some(&lost), Some(&good[..end]));
    let j = Journal::open(&dir, config()).unwrap();
    assert_eq!(j.next_sequence(), 300, "resumed after the repaired frames");
    drop(j);
    assert_eq!(std::fs::read(segment_path(&dir, 0)).unwrap(), good);
    assert!(!durable_path(&dir, 0).exists());
}

/// **A reader racing the write thread never reads a partial frame.** The write thread hands each span to
/// the kernel in ONE `pwrite`, so a reader can catch a frame whose length is there and whose payload is
/// not. Fat records (each spans pages) across many rolls, appended as fast as the core can, while a
/// second thread reads the directory over and over: every read is a contiguous run of whole records from
/// seq 0, it never shrinks, and when it stops early it stops at a PENDING frame at the end of the last
/// segment (a check that does not match), never anywhere else and never at an out-of-bounds length or a
/// parse failure. After the race every record reads back.
#[test]
fn a_reader_racing_the_write_thread_never_reads_a_partial_frame() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tmp_dir("dur-race");
    let small = JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 64 };
    let n = 3_000u64;
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let (path, stop) = (dir.to_path_buf(), Arc::clone(&stop));
        std::thread::spawn(move || {
            let (mut reads, mut seen, mut pending) = (0usize, 0usize, 0usize);
            while !stop.load(Ordering::Acquire) {
                let (records, halt) = match Journal::read_all_located(&path) {
                    Ok(r) => r,
                    Err(_) => continue, // a segment the write thread is renaming into place
                };
                for (k, r) in records.iter().enumerate() {
                    assert_eq!(crate::record_sequence(r), k as u64, "only whole records, in order");
                }
                assert!(records.len() >= seen, "a read never goes backwards");
                seen = records.len();
                if let Some(h) = halt {
                    // The write thread makes segment k+1 only after k is whole, so a stop is in the
                    // live segment, with nothing readable after it.
                    assert!(
                        h.in_tail && h.cause == crate::JournalCorruptCause::CheckMismatch,
                        "a read stops early only at a frame not yet written: {h:?}"
                    );
                    pending += 1;
                }
                reads += 1;
            }
            (reads, seen, pending)
        })
    };
    let mut j = Journal::open(&dir, small).unwrap();
    for i in 0..n {
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    j.write_fence().wait().unwrap();
    stop.store(true, Ordering::Release);
    let (reads, _seen, _pending) = reader.join().unwrap();
    drop(j);
    assert!(reads > 0, "the reader ran");
    assert_eq!(records(&dir), n as usize, "every record reads back after the race");
}

/// While a segment is live the sync thread keeps its copy: after a flush the copy holds every byte the
/// working file holds above the resume point, at the same offsets; a clean stop deletes it.
#[test]
fn the_sync_thread_keeps_a_durable_copy_of_the_live_segment() {
    let dir = tmp_dir("dur-live");
    let mut j = Journal::open(&dir, config()).unwrap();
    for i in 0..100u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Resume: the copy now holds nothing below the resume point.
    let mut j = Journal::open(&dir, config()).unwrap();
    let base = j.cursor;
    for i in 100..250u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.flush().unwrap();
    let work = std::fs::read(segment_path(&dir, 0)).unwrap();
    let copy = std::fs::read(durable_path(&dir, 0)).unwrap();
    assert_eq!(copy.len(), j.cursor, "the copy ends where the frames end");
    assert_eq!(copy[base..], work[base..j.cursor], "the same bytes at the same offsets");
    assert!(copy[..base].iter().all(|&b| b == 0), "a hole below the resume point");
    drop(j);
    assert!(!durable_path(&dir, 0).exists(), "a clean stop deletes it");
    assert_eq!(records(&dir), 250);
}
