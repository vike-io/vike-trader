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
    /// consequence of a crash, and the version header ([`super::format::check_version`]) did not catch it.
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
#[derive(Debug)]
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

#[path = "frame_tests.rs"]
#[cfg(test)]
mod frame_tests;
