use std::assert_matches;
use tracing_test::traced_test;

use super::*;
use crate::format::HEADER;
use crate::testutil::*;
use crate::{Journal, JournalFileConfig};

/// Frame `payload` exactly as `Journal::write_framed` does:
/// `[len u32 LE][fnv1a32(payload) u32 LE][payload]` — a VALID frame, so a fixture can then
/// break exactly ONE of the three things `walk_frames` checks and nothing else.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(&fnv1a32(payload).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// One valid `Command` frame — the "good record before the tear" every fixture below leads with, so
/// each proves the walk actually REACHED its planted frame rather than tripping at offset 0.
fn good_frame() -> Vec<u8> {
    let rec = JournalRecord::Command { sequence: 0, now_ms: 1_000, message: ingest(0) };
    framed(&serde_json::to_vec(&rec).expect("JournalRecord serializes"))
}

/// A payload that CHECKSUMS CORRECTLY and is not a `JournalRecord` for this build — an unknown
/// externally-tagged variant. This is what a segment written by a DIFFERENT build looks like:
/// nothing tore, the bytes are exactly what was written, and they still do not parse.
fn schema_mismatch_payload() -> Vec<u8> {
    br#"{"NotAVariantOfThisBuild":{"sequence":0}}"#.to_vec()
}

/// FIXTURE 1 of 3 — an out-of-bounds length prefix is `JournalCorruptCause::OobLen`, and NOT either of
/// the other two. Asserting the SPECIFIC variant is the point: a three-way enum whose arms no
/// test tells apart is decoration. (Mutation-checked: pointing this arm at `CheckMismatch` or
/// `ParseFailed` in `walk_frames` reddens this test and only this test.)
#[test]
fn oob_len_is_reported_as_oob_len() {
    let mut buffer = good_frame();
    // a length prefix claiming far more bytes than the buffer holds, with a plausible check
    buffer.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());
    let tear = good_frame().len();

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "the walk delivered the one valid record BEFORE the tear");
    assert_eq!(current, tear, "the walk halted AT the torn frame, never past it");
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::OobLen,
            "an out-of-bounds len is a torn LENGTH write, not a check or parse failure"
        ),
        JournalWalkEnd::Clean => panic!("an out-of-bounds len must not read as a clean end"),
    }
}

/// FIXTURE 2 of 3 — a flipped payload byte under an untouched check is
/// `JournalCorruptCause::CheckMismatch`, and the walk never reaches the parse.
#[test]
fn check_mismatch_is_reported_as_check_mismatch() {
    let good = good_frame();
    let mut buffer = good.clone();
    let tear = buffer.len();
    let mut torn = good.clone();
    let last = torn.len() - 1;
    torn[last] ^= 0xFF; // payload changed, stored check NOT recomputed
    buffer.extend_from_slice(&torn);

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "only the intact record is delivered");
    assert_eq!(current, tear);
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::CheckMismatch,
            "the payload does not match its own check — the parse is never attempted"
        ),
        JournalWalkEnd::Clean => panic!("a check mismatch must not read as a clean end"),
    }
}

/// FIXTURE 3 of 3 — the arm this file had NEVER covered: a payload framed with a **VALID** FNV
/// check that is not a `JournalRecord`. It must be `JournalCorruptCause::ParseFailed`, because it is the
/// one condition that is a SCHEMA mismatch (a different build wrote it) rather than a torn tail,
/// and the two want opposite operator readings.
#[test]
fn valid_check_over_a_non_record_payload_is_reported_as_parse_failed() {
    let payload = schema_mismatch_payload();
    // the fixture is only honest if the check REALLY passes — otherwise it would trip one arm
    // earlier and "prove" nothing about the parse arm
    let planted = framed(&payload);
    assert_eq!(
        fnv1a32(&payload),
        u32::from_le_bytes(planted[4..8].try_into().unwrap()),
        "the planted frame's check must be VALID — that is the whole premise of this fixture"
    );

    let mut buffer = good_frame();
    let tear = buffer.len();
    buffer.extend_from_slice(&planted);

    let mut seen = 0usize;
    let (current, end) = walk_frames(&buffer, 0, |_| seen += 1);
    assert_eq!(seen, 1, "the non-record frame is NOT delivered as a record");
    assert_eq!(current, tear, "the walk halted at the unparseable frame, not past it");
    match end {
        JournalWalkEnd::Corrupt(cause) => assert_eq!(
            cause,
            JournalCorruptCause::ParseFailed,
            "checksum-valid bytes that are not a JournalRecord are a SCHEMA mismatch"
        ),
        JournalWalkEnd::Clean => panic!("an unparseable payload must not read as a clean end"),
    }
}

/// The three causes are pairwise DISTINCT and each carries its own operator reading — the
/// property the three fixtures above rely on, asserted directly so collapsing two arms into one
/// variant cannot pass by making both fixtures agree.
#[test]
fn the_three_causes_are_distinct_and_each_reads_differently() {
    let all = [
        JournalCorruptCause::OobLen,
        JournalCorruptCause::CheckMismatch,
        JournalCorruptCause::ParseFailed,
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
            assert_ne!(a.as_str(), b.as_str(), "log tokens must not collide");
            assert_ne!(a.reading(), b.reading(), "operator readings must not collide");
        }
    }
    // the log token is the field a query keys on — pin the spellings
    assert_eq!(JournalCorruptCause::OobLen.as_str(), "oob_len");
    assert_eq!(JournalCorruptCause::CheckMismatch.as_str(), "check_mismatch");
    assert_eq!(JournalCorruptCause::ParseFailed.as_str(), "parse_failed");
    // and only the schema-mismatch reading says SCHEMA — the one that must be read as loud
    assert!(JournalCorruptCause::ParseFailed.reading().contains("SCHEMA"));
    assert!(!JournalCorruptCause::OobLen.reading().contains("SCHEMA"));
    assert!(!JournalCorruptCause::CheckMismatch.reading().contains("SCHEMA"));
}

/// The `ParseFailed` arm through the REAL on-disk reader: plant a checksum-valid non-record
/// frame after 10 good records in a real segment and confirm `read_all` keeps the valid prefix
/// and drops it — behaviour UNCHANGED (this change is diagnostic only), reached by the parse arm
/// rather than by a check mismatch.
#[test]
fn read_all_drops_a_checksum_valid_non_record_frame() {
    let dir = tmp_dir("parse_failed");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    // Walk to the write cursor (past the 10 records) the way this file's other on-disk fixtures
    // do — the cursor is not exposed. `segment_files`, not the first `read_dir` entry: the dir also
    // holds the `LOCK` sentinel.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..10 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected 10 valid records while walking to the write cursor");
        current += 8 + len;
    }
    let planted = framed(&schema_mismatch_payload());
    bytes[current..current + planted.len()].copy_from_slice(&planted);
    std::fs::write(&seg, bytes).unwrap();

    // The planted frame is reached through the PARSE arm, not the check arm.
    let bytes = std::fs::read(&seg).unwrap();
    let (halt, end) = walk_frames(&bytes, HEADER, |_| {});
    assert_eq!(halt, current, "the halt offset is the planted frame's own offset");
    assert_matches!(end, JournalWalkEnd::Corrupt(JournalCorruptCause::ParseFailed));

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 10, "the 10 valid records survive; the non-record frame is dropped");
}

/// The seam that ACTS on the halt logs it, once, naming the segment, the halt offset and the
/// cause. Before this the halt decision was taken in silence — a reader of a live node's logs
/// could not tell that replay had stopped early, let alone why.
#[traced_test]
#[test]
fn read_all_warns_once_naming_the_segment_offset_and_cause() {
    let dir = tmp_dir("halt_warn");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..4 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..3 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        current += 8 + len;
    }
    // the 4th record's length prefix, torn to an out-of-bounds value
    bytes[current..current + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();

    assert_eq!(Journal::read_all(&dir).unwrap().len(), 3);
    assert!(logs_contain("journal read halted at a corrupt frame"));
    assert!(logs_contain("oob_len"), "the CAUSE is named, not just the fact of a halt");
    assert!(logs_contain(&format!("offset={current}")), "the halt OFFSET is named");
    assert!(
        logs_contain(&seg.file_name().unwrap().to_string_lossy()),
        "the SEGMENT is named — a journal has many"
    );
    assert!(
        !logs_contain("parse_failed") && !logs_contain("check_mismatch"),
        "one cause per halt: a torn length must not also report the other two"
    );
}

#[test]
fn torn_tail_is_detected_and_stops_cleanly() {
    let dir = tmp_dir("torn");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // corrupt the last record's payload byte (simulates a torn page after power cut).
    // Select the SEGMENT explicitly (`segment_files`) rather than the first `read_dir` entry: the
    // dir also holds the interlock sentinel (`LOCK`), whose position in the directory listing
    // is filesystem-dependent.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let n = bytes.len();
    // walk to the last record start is overkill — flip a byte near the end of written data
    let last_nonzero = bytes.iter().rposition(|&b| b != 0).unwrap();
    bytes[last_nonzero] ^= 0xFF;
    std::fs::write(&seg, bytes).unwrap();
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 9, "replay stops at the last VALID record, no panic (had {n} bytes)");
}

#[test]
fn torn_tail_out_of_bounds_len() {
    let dir = tmp_dir("oob_len");
    let config = small_config();
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..10 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Corrupt the LAST written record's 4 len-prefix bytes to an out-of-bounds huge length
    // (simulates a torn length-prefix write after a power cut — distinct from the CRC-flip
    // torn-tail case above). Records are appended contiguously from HEADER as
    // `[len][crc][payload]` frames, and `next_sequence`/the write cursor aren't exposed, so walk
    // the frames: skip the first 9 records to land on the 10th (last) record's start offset.
    // `segment_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
    let seg = segment_files(&dir).remove(0);
    let mut bytes = std::fs::read(&seg).unwrap();
    let mut current = HEADER;
    for _ in 0..9 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected a valid record while walking frames to the 10th record");
        current += 8 + len;
    }
    let last_record_start = current; // offset of the 10th (last) record's [len] prefix
    bytes[last_record_start..last_record_start + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(&seg, bytes).unwrap();
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 9, "replay stops before the out-of-bounds-len record, no panic");
}

/// Cross-segment guard for the unified `walk_frames` halt policy: a corruption in an EARLY
/// segment must stop replay across the WHOLE journal — records in LATER segments (past the
/// corruption gap) are NEVER returned. Every other corruption test here is single-segment, so
/// this is the case that would silently regress if `read_all`'s journal-wide halt were ever
/// turned into a per-file break (which would still pass all the single-segment tests).
#[test]
fn read_all_halts_journal_wide_at_a_corrupt_early_segment() {
    let dir = tmp_dir("cross-seg-halt");
    // small segments so the journal rolls into several files
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..200 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);

    // The scenario is only meaningful if there is a LATER segment whose records could be
    // wrongly replayed past the corruption gap.
    let mut segments = segment_files(&dir);
    segments.sort();
    assert!(segments.len() >= 2, "scenario must span multiple segments (got {})", segments.len());

    // Corrupt the 6th record (index 5) of the FIRST segment: overwrite its len prefix with an
    // out-of-bounds huge length (a torn length-prefix write). Walk the frames to find it.
    let segment0 = &segments[0];
    let mut bytes = std::fs::read(segment0).unwrap();
    let mut current = HEADER;
    for _ in 0..5 {
        let len = u32::from_le_bytes(bytes[current..current + 4].try_into().unwrap()) as usize;
        assert_ne!(len, 0, "expected valid records while walking to the 6th record");
        current += 8 + len;
    }
    bytes[current..current + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    std::fs::write(segment0, bytes).unwrap();

    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(
        back.len(),
        5,
        "replay halts journal-wide at the corrupt early segment; later segments are NOT replayed"
    );
    for (i, r) in back.iter().enumerate() {
        assert_matches!(
            r, JournalRecord::Command { sequence: seq, .. } if *seq == i as u64,
            "the 5 records before the corruption survive, in order"
        );
    }
}
