//! The frame codec — `[len: u32 LE][check: u32 FNV-1a32(payload)][payload: serde_json(record)]`.
//!
//! [`fnv1a32`] is the check, and [`walk_frames`] is the ONE frame walk every reader in this module
//! layers over — the single site where the torn-tail ([`WalkEnd::Corrupt`]) vs clean-end
//! ([`WalkEnd::Clean`]) decision is made. Split out of `journal.rs` verbatim.
//!
//! [`walk_frames`] is also the only place that knows WHICH of the three torn-tail conditions fired
//! ([`CorruptCause`]) — the check gates the parse, so the walk has already made the distinction by
//! the time it returns. It used to collapse all three into one word, which left a reader unable to
//! tell an expected post-crash truncation from a payload written by a DIFFERENT BUILD. Carrying the
//! cause out is diagnostics only: every caller's halt behaviour is unchanged.

use super::record::JournalRecord;

pub(super) fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// WHICH of the three torn-tail conditions ended a [`walk_frames`] pass — the distinction the walk
/// already computes (the check gates the parse) and used to throw away.
///
/// They deserve OPPOSITE readings, which is the whole reason this type exists. [`Self::OobLen`] is
/// the EXPECTED, benign shape after any crash: stop reading, keep everything before it.
/// [`Self::ParseFailed`] is the loud one — the bytes checksum correctly, so nothing tore them; they
/// were written by a build whose `JournalRecord` shape differs from this one's, and reading them
/// could produce wrong values rather than fewer of them.
///
/// It is DIAGNOSTIC ONLY: [`WalkEnd::Corrupt`] halts every caller exactly as it did when it carried
/// no payload (`read_all` halts journal-wide, `scan_written` and `scan_segment_seqs` end their fold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorruptCause {
    /// An out-of-bounds length prefix — the frame claims more bytes than the segment holds. A TORN
    /// TAIL: the process died between writing the length and writing the payload. Expected and
    /// benign after a crash; every record before it is intact.
    OobLen,
    /// The FNV-1a32 check over the payload does not match the stored check. The frame's length was
    /// written whole but its payload was not (or the bytes rotted under it) — also a torn tail, one
    /// frame further along than [`Self::OobLen`].
    CheckMismatch,
    /// The payload PASSED the check — so it is exactly the bytes that were written, nothing tore —
    /// and still did not deserialize as a [`JournalRecord`]. That is a SCHEMA mismatch: the segment
    /// was written by a different build. Loud, not benign: unlike a torn tail it is not the ordinary
    /// consequence of a crash, and the version header ([`super::check_version`]) did not catch it.
    ParseFailed,
}

impl CorruptCause {
    /// The stable log token for the `cause` field (never a `Debug` render — this is read by
    /// operators and matched in log queries).
    pub fn as_str(self) -> &'static str {
        match self {
            CorruptCause::OobLen => "oob_len",
            CorruptCause::CheckMismatch => "check_mismatch",
            CorruptCause::ParseFailed => "parse_failed",
        }
    }

    /// The one-line operator reading — what this cause MEANS, which is the whole point of splitting
    /// the three apart. Emitted beside [`Self::as_str`] so a warn line answers "is this benign?"
    /// without the reader having to know the frame codec.
    pub fn reading(self) -> &'static str {
        match self {
            CorruptCause::OobLen => {
                "torn length prefix — the ordinary, benign consequence of a crash mid-write; \
                 every record before this offset is intact"
            }
            CorruptCause::CheckMismatch => {
                "payload does not match its checksum — a torn payload write (benign after a \
                 crash) or bytes rotted on disk (not benign)"
            }
            CorruptCause::ParseFailed => {
                "payload checksums CORRECTLY but is not a JournalRecord for this build — a SCHEMA \
                 mismatch, not a torn tail: these bytes came from a different build"
            }
        }
    }
}

impl std::fmt::Display for CorruptCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a [`walk_frames`] pass over one segment buffer stopped.
pub(super) enum WalkEnd {
    /// Clean end of this segment's written data: a `len==0` frame (segments are zero-filled at
    /// creation) or the cursor reached the buffer's end. NOT corruption — a caller that spans
    /// multiple segments crosses to the next one.
    Clean,
    /// A torn/corrupt tail, carrying WHICH condition fired ([`CorruptCause`]). The walk stopped at
    /// the last VALID record and never advanced into the suspect bytes.
    Corrupt(CorruptCause),
}

/// The ONE `[len: u32 LE][check: u32 FNV-1a32(payload)][payload: serde_json(JournalRecord)]`
/// frame walk, shared by all three journal readers (`scan_written` resume-cursor, `read_all`
/// replay, `scan_segment_seqs` prune-bounds) — each layers its own aggregation over `on_record`.
/// Walks from `start`, invoking `on_record` with every successfully-decoded owned record, and
/// returns the cursor where it stopped plus WHY ([`WalkEnd`], carrying [`CorruptCause`] on a torn
/// end — the cursor and the cause are what a caller needs to LOG the halt, and `read_all` does).
///
/// OOB-LEN HALT POLICY (canonical, adopted from `read_all` and now applied uniformly here): an
/// out-of-bounds length prefix is a torn LENGTH write, so it is classified as `Corrupt` — the
/// walk STOPS at it and never advances past it, exactly like a check mismatch or a parse failure.
/// It is deliberately kept DISTINCT from the clean `len==0` end so a multi-segment caller
/// (`read_all`) can halt replay journal-WIDE on corruption while still crossing a clean segment
/// boundary; the single-segment callers (`scan_written`, `scan_segment_seqs`) stop either way.
/// Single-siting this walk keeps every torn-tail decision identical and stops the three former
/// copies from drifting apart.
pub(super) fn walk_frames(
    buf: &[u8],
    start: usize,
    mut on_record: impl FnMut(JournalRecord),
) -> (usize, WalkEnd) {
    let mut cur = start;
    loop {
        if cur + 8 > buf.len() {
            return (cur, WalkEnd::Clean);
        }
        let len = u32::from_le_bytes(buf[cur..cur + 4].try_into().unwrap()) as usize;
        if len == 0 {
            return (cur, WalkEnd::Clean); // clean end of written data (zero-filled tail)
        }
        if cur + 8 + len > buf.len() {
            // torn length prefix — never advance past it
            return (cur, WalkEnd::Corrupt(CorruptCause::OobLen));
        }
        let check = u32::from_le_bytes(buf[cur + 4..cur + 8].try_into().unwrap());
        let payload = &buf[cur + 8..cur + 8 + len];
        if fnv1a32(payload) != check {
            // torn payload write / rotted bytes
            return (cur, WalkEnd::Corrupt(CorruptCause::CheckMismatch));
        }
        match serde_json::from_slice::<JournalRecord>(payload) {
            Ok(rec) => on_record(rec),
            // The check PASSED and the payload still is not a record for this build — the one arm
            // that is a schema mismatch rather than a torn tail. This is the ONLY site that can
            // tell the two apart, because the check above has already gated it.
            Err(_) => return (cur, WalkEnd::Corrupt(CorruptCause::ParseFailed)),
        }
        cur += 8 + len;
    }
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::*;
    use crate::journal::testutil::*;
    use crate::journal::{CommandJournal, JournalFileConfig, HEADER};

    /// Frame `payload` exactly as `CommandJournal::write_framed` does:
    /// `[len u32 LE][fnv1a32(payload) u32 LE][payload]` — a VALID frame, so a fixture can then
    /// break exactly ONE of the three things `walk_frames` checks and nothing else.
    fn framed(payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(8 + payload.len());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(&fnv1a32(payload).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// One valid `Cmd` frame — the "good record before the tear" every fixture below leads with, so
    /// each proves the walk actually REACHED its planted frame rather than tripping at offset 0.
    fn good_frame() -> Vec<u8> {
        let rec = JournalRecord::Cmd { seq: 0, now_ms: 1_000, msg: ingest(0) };
        framed(&serde_json::to_vec(&rec).expect("JournalRecord serializes"))
    }

    /// A payload that CHECKSUMS CORRECTLY and is not a `JournalRecord` for this build — an unknown
    /// externally-tagged variant. This is what a segment written by a DIFFERENT build looks like:
    /// nothing tore, the bytes are exactly what was written, and they still do not parse.
    fn schema_mismatch_payload() -> Vec<u8> {
        br#"{"NotAVariantOfThisBuild":{"seq":0}}"#.to_vec()
    }

    /// FIXTURE 1 of 3 — an out-of-bounds length prefix is `CorruptCause::OobLen`, and NOT either of
    /// the other two. Asserting the SPECIFIC variant is the point: a three-way enum whose arms no
    /// test tells apart is decoration. (Mutation-checked: pointing this arm at `CheckMismatch` or
    /// `ParseFailed` in `walk_frames` reddens this test and only this test.)
    #[test]
    fn oob_len_is_reported_as_oob_len() {
        let mut buf = good_frame();
        // a length prefix claiming far more bytes than the buffer holds, with a plausible check
        buf.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        let tear = good_frame().len();

        let mut seen = 0usize;
        let (cur, end) = walk_frames(&buf, 0, |_| seen += 1);
        assert_eq!(seen, 1, "the walk delivered the one valid record BEFORE the tear");
        assert_eq!(cur, tear, "the walk halted AT the torn frame, never past it");
        match end {
            WalkEnd::Corrupt(cause) => assert_eq!(
                cause,
                CorruptCause::OobLen,
                "an out-of-bounds len is a torn LENGTH write, not a check or parse failure"
            ),
            WalkEnd::Clean => panic!("an out-of-bounds len must not read as a clean end"),
        }
    }

    /// FIXTURE 2 of 3 — a flipped payload byte under an untouched check is
    /// `CorruptCause::CheckMismatch`, and the walk never reaches the parse.
    #[test]
    fn check_mismatch_is_reported_as_check_mismatch() {
        let good = good_frame();
        let mut buf = good.clone();
        let tear = buf.len();
        let mut torn = good.clone();
        let last = torn.len() - 1;
        torn[last] ^= 0xFF; // payload changed, stored check NOT recomputed
        buf.extend_from_slice(&torn);

        let mut seen = 0usize;
        let (cur, end) = walk_frames(&buf, 0, |_| seen += 1);
        assert_eq!(seen, 1, "only the intact record is delivered");
        assert_eq!(cur, tear);
        match end {
            WalkEnd::Corrupt(cause) => assert_eq!(
                cause,
                CorruptCause::CheckMismatch,
                "the payload does not match its own check — the parse is never attempted"
            ),
            WalkEnd::Clean => panic!("a check mismatch must not read as a clean end"),
        }
    }

    /// FIXTURE 3 of 3 — the arm this file had NEVER covered: a payload framed with a **VALID** FNV
    /// check that is not a `JournalRecord`. It must be `CorruptCause::ParseFailed`, because it is the
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

        let mut buf = good_frame();
        let tear = buf.len();
        buf.extend_from_slice(&planted);

        let mut seen = 0usize;
        let (cur, end) = walk_frames(&buf, 0, |_| seen += 1);
        assert_eq!(seen, 1, "the non-record frame is NOT delivered as a record");
        assert_eq!(cur, tear, "the walk halted at the unparseable frame, not past it");
        match end {
            WalkEnd::Corrupt(cause) => assert_eq!(
                cause,
                CorruptCause::ParseFailed,
                "checksum-valid bytes that are not a JournalRecord are a SCHEMA mismatch"
            ),
            WalkEnd::Clean => panic!("an unparseable payload must not read as a clean end"),
        }
    }

    /// The three causes are pairwise DISTINCT and each carries its own operator reading — the
    /// property the three fixtures above rely on, asserted directly so collapsing two arms into one
    /// variant cannot pass by making both fixtures agree.
    #[test]
    fn the_three_causes_are_distinct_and_each_reads_differently() {
        let all = [CorruptCause::OobLen, CorruptCause::CheckMismatch, CorruptCause::ParseFailed];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(a.as_str(), b.as_str(), "log tokens must not collide");
                assert_ne!(a.reading(), b.reading(), "operator readings must not collide");
            }
        }
        // the log token is the field a query keys on — pin the spellings
        assert_eq!(CorruptCause::OobLen.as_str(), "oob_len");
        assert_eq!(CorruptCause::CheckMismatch.as_str(), "check_mismatch");
        assert_eq!(CorruptCause::ParseFailed.as_str(), "parse_failed");
        // and only the schema-mismatch reading says SCHEMA — the one that must be read as loud
        assert!(CorruptCause::ParseFailed.reading().contains("SCHEMA"));
        assert!(!CorruptCause::OobLen.reading().contains("SCHEMA"));
        assert!(!CorruptCause::CheckMismatch.reading().contains("SCHEMA"));
    }

    /// The `ParseFailed` arm through the REAL on-disk reader: plant a checksum-valid non-record
    /// frame after 10 good records in a real segment and confirm `read_all` keeps the valid prefix
    /// and drops it — behaviour UNCHANGED (this change is diagnostic only), reached by the parse arm
    /// rather than by a check mismatch.
    #[test]
    fn read_all_drops_a_checksum_valid_non_record_frame() {
        let dir = tmp_dir("parse_failed");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..10 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);

        // Walk to the write cursor (past the 10 records) the way this file's other on-disk fixtures
        // do — the cursor is not exposed. `seg_files`, not the first `read_dir` entry: the dir also
        // holds the `LOCK` sentinel.
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        let mut cur = HEADER;
        for _ in 0..10 {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            assert_ne!(len, 0, "expected 10 valid records while walking to the write cursor");
            cur += 8 + len;
        }
        let planted = framed(&schema_mismatch_payload());
        bytes[cur..cur + planted.len()].copy_from_slice(&planted);
        std::fs::write(&seg, bytes).unwrap();

        // The planted frame is reached through the PARSE arm, not the check arm.
        let bytes = std::fs::read(&seg).unwrap();
        let (halt, end) = walk_frames(&bytes, HEADER, |_| {});
        assert_eq!(halt, cur, "the halt offset is the planted frame's own offset");
        assert!(matches!(end, WalkEnd::Corrupt(CorruptCause::ParseFailed)));

        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(back.len(), 10, "the 10 valid records survive; the non-record frame is dropped");
    }

    /// The seam that ACTS on the halt logs it, once, naming the segment, the halt offset and the
    /// cause. Before this the halt decision was taken in silence — a reader of a live node's logs
    /// could not tell that replay had stopped early, let alone why.
    #[traced_test]
    #[test]
    fn read_all_warns_once_naming_the_segment_offset_and_cause() {
        let dir = tmp_dir("halt_warn");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..4 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);

        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        let mut cur = HEADER;
        for _ in 0..3 {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            cur += 8 + len;
        }
        // the 4th record's length prefix, torn to an out-of-bounds value
        bytes[cur..cur + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();

        assert_eq!(CommandJournal::read_all(&dir).unwrap().len(), 3);
        assert!(logs_contain("journal read halted at a corrupt frame"));
        assert!(logs_contain("oob_len"), "the CAUSE is named, not just the fact of a halt");
        assert!(logs_contain(&format!("offset={cur}")), "the halt OFFSET is named");
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
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..10 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // corrupt the last record's payload byte (simulates a torn page after power cut).
        // Select the SEGMENT explicitly (`seg_files`) rather than the first `read_dir` entry: the
        // dir also holds the interlock sentinel (`LOCK`), whose position in the directory listing
        // is filesystem-dependent.
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        let n = bytes.len();
        // walk to the last record start is overkill — flip a byte near the end of written data
        let last_nonzero = bytes.iter().rposition(|&b| b != 0).unwrap();
        bytes[last_nonzero] ^= 0xFF;
        std::fs::write(&seg, bytes).unwrap();
        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(
            back.len(),
            9,
            "replay stops at the last VALID record, no panic (had {n} bytes)"
        );
    }

    #[test]
    fn torn_tail_out_of_bounds_len() {
        let dir = tmp_dir("oob_len");
        let cfg = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..10 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);
        // Corrupt the LAST written record's 4 len-prefix bytes to an out-of-bounds huge length
        // (simulates a torn length-prefix write after a power cut — distinct from the CRC-flip
        // torn-tail case above). Records are appended contiguously from HEADER as
        // `[len][crc][payload]` frames, and `next_seq`/the write cursor aren't exposed, so walk
        // the frames: skip the first 9 records to land on the 10th (last) record's start offset.
        // `seg_files` (not the first `read_dir` entry) — the dir also holds the `LOCK` sentinel.
        let seg = seg_files(&dir).remove(0);
        let mut bytes = std::fs::read(&seg).unwrap();
        let mut cur = HEADER;
        for _ in 0..9 {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            assert_ne!(len, 0, "expected a valid record while walking frames to the 10th record");
            cur += 8 + len;
        }
        let last_record_start = cur; // offset of the 10th (last) record's [len] prefix
        bytes[last_record_start..last_record_start + 4]
            .copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        std::fs::write(&seg, bytes).unwrap();
        let back = CommandJournal::read_all(&dir).unwrap();
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
        let cfg = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
        let mut j = CommandJournal::open(&dir, cfg).unwrap();
        for i in 0..200 {
            j.append_cmd(1_000 + i as i64, &ingest(i)).unwrap();
        }
        drop(j);

        // The scenario is only meaningful if there is a LATER segment whose records could be
        // wrongly replayed past the corruption gap.
        let mut segs = seg_files(&dir);
        segs.sort();
        assert!(segs.len() >= 2, "scenario must span multiple segments (got {})", segs.len());

        // Corrupt the 6th record (index 5) of the FIRST segment: overwrite its len prefix with an
        // out-of-bounds huge length (a torn length-prefix write). Walk the frames to find it.
        let seg0 = &segs[0];
        let mut bytes = std::fs::read(seg0).unwrap();
        let mut cur = HEADER;
        for _ in 0..5 {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            assert_ne!(len, 0, "expected valid records while walking to the 6th record");
            cur += 8 + len;
        }
        bytes[cur..cur + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        std::fs::write(seg0, bytes).unwrap();

        let back = CommandJournal::read_all(&dir).unwrap();
        assert_eq!(
            back.len(),
            5,
            "replay halts journal-wide at the corrupt early segment; later segments are NOT replayed"
        );
        for (i, r) in back.iter().enumerate() {
            assert!(
                matches!(r, JournalRecord::Cmd { seq, .. } if *seq == i as u64),
                "the 5 records before the corruption survive, in order"
            );
        }
    }
}
