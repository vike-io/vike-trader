//! The frame codec — `[len: u32 LE][check: u32 FNV-1a32(payload)][payload: msgpack(record)]`.
//!
//! **The payload** ([`encode_into`], [`decode`]): msgpack since format 34 (`crate::format::BINARY_VERSION`), JSON in
//! every format before it. The writer writes msgpack only; every reader decodes both, told apart by the payload's
//! first byte, so a journal moved aside by the step still reads (a JSON record is an object, `{`; a msgpack record is
//! a one-entry map, `0x81`, and no record is a msgpack positive integer `0x7b`).
//!
//! [`fnv1a32`] is the check, and [`walk_frames`] is the ONE frame walk every reader in this module
//! layers over — the single site where the torn-tail ([`JournalWalkEnd::Corrupt`]) vs clean-end
//! ([`JournalWalkEnd::Clean`]) decision is made. Split out of `journal.rs` verbatim.
//!
//! [`walk_frames`] is also the only place that knows WHICH of the three torn-tail conditions fired
//! ([`JournalCorruptCause`]) — the check gates the parse, so the walk has already made the distinction by
//! the time it returns. It used to collapse all three into one word, which left a reader unable to
//! tell an expected post-crash truncation from a payload written by a DIFFERENT BUILD. Carrying the
//! cause out is diagnostics only: every caller's halt behaviour is unchanged.

use serde::{Deserialize, Serialize};

use super::record::JournalRecord;

/// Append `record`'s msgpack to `buffer`: every struct a map of its field names (the schema leans on
/// `skip_serializing_if`, which the positional form would misalign) and serde's human-readable mode, so every type
/// takes the shape it takes in the JSON the formats before 34 wrote. The one difference is a non-finite `f64`, which
/// msgpack keeps and JSON wrote as `null`.
pub(crate) fn encode_into(buffer: &mut Vec<u8>, record: &impl Serialize) {
    let mut s = rmp_serde::Serializer::new(buffer).with_struct_map().with_human_readable();
    record.serialize(&mut s).expect("a journal record serializes");
}

/// One frame's payload as the record it holds: JSON when it starts with `{` (formats 31..=33), msgpack otherwise.
/// `None` for bytes that are neither (a frame another build wrote).
pub(crate) fn decode(payload: &[u8]) -> Option<JournalRecord> {
    if payload.first() == Some(&b'{') {
        return serde_json::from_slice(payload).ok();
    }
    let mut d = rmp_serde::Deserializer::from_read_ref(payload).with_human_readable();
    JournalRecord::deserialize(&mut d).ok()
}

/// A payload as a JSON value, for a test's byte surgery (decode, doctor, [`payload_of_value`] again). TEST SUPPORT,
/// like [`end_of_frames`]; it panics on a payload that does not decode.
#[cfg(any(test, feature = "test-support"))]
pub fn payload_as_value(payload: &[u8]) -> serde_json::Value {
    if payload.first() == Some(&b'{') {
        return serde_json::from_slice(payload).expect("a JSON payload");
    }
    let mut d = rmp_serde::Deserializer::from_read_ref(payload).with_human_readable();
    serde_json::Value::deserialize(&mut d).expect("a msgpack payload")
}

/// The payload this build writes for `value` (a record a test doctored as JSON). TEST SUPPORT.
#[cfg(any(test, feature = "test-support"))]
pub fn payload_of_value(value: &serde_json::Value) -> Vec<u8> {
    let mut buffer = Vec::new();
    encode_into(&mut buffer, value);
    buffer
}

/// The frame CHECK over `payload`. Public (re-exported at the crate root) so a consumer's byte
/// surgery can re-frame a doctored payload with the one spelling of it instead of a twin.
pub fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The byte offset just past the last written frame of the segment file `seg` (its first zero length
/// prefix): where a test's byte surgery plants a torn or foreign tail. TEST SUPPORT — `cfg(test)`,
/// or the `test-support` feature, which only DEV edges turn on — and it panics on an unreadable file.
#[cfg(any(test, feature = "test-support"))]
pub fn end_of_frames(seg: &std::path::Path) -> usize {
    let bytes = std::fs::read(seg).unwrap();
    let mut at = crate::format::HEADER;
    loop {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if len == 0 {
            return at;
        }
        at += 8 + len;
    }
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
/// It is DIAGNOSTIC ONLY: [`JournalWalkEnd::Corrupt`] halts every caller exactly as it did when it carried
/// no payload (`read_all` halts journal-wide, `scan_written` and `scan_segment_sequences` end their fold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalCorruptCause {
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

impl JournalCorruptCause {
    /// The stable log token for the `cause` field (never a `Debug` render — this is read by
    /// operators and matched in log queries).
    pub fn as_str(self) -> &'static str {
        match self {
            JournalCorruptCause::OobLen => "oob_len",
            JournalCorruptCause::CheckMismatch => "check_mismatch",
            JournalCorruptCause::ParseFailed => "parse_failed",
        }
    }

    /// The one-line operator reading — what this cause MEANS, which is the whole point of splitting
    /// the three apart. Emitted beside [`Self::as_str`] so a warn line answers "is this benign?"
    /// without the reader having to know the frame codec.
    pub fn reading(self) -> &'static str {
        match self {
            JournalCorruptCause::OobLen => {
                "torn length prefix — the ordinary, benign consequence of a crash mid-write; \
                 every record before this offset is intact"
            }
            JournalCorruptCause::CheckMismatch => {
                "payload does not match its checksum — a torn payload write (benign after a \
                 crash) or bytes rotted on disk (not benign)"
            }
            JournalCorruptCause::ParseFailed => {
                "payload checksums CORRECTLY but is not a JournalRecord for this build — a SCHEMA \
                 mismatch, not a torn tail: these bytes came from a different build"
            }
        }
    }
}

impl std::fmt::Display for JournalCorruptCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a [`walk_frames`] pass over one segment buffer stopped.
#[derive(Debug)]
pub(super) enum JournalWalkEnd {
    /// Clean end of this segment's written data: a `len==0` frame (segments are zero-filled at
    /// creation) or the cursor reached the buffer's end. NOT corruption — a caller that spans
    /// multiple segments crosses to the next one.
    Clean,
    /// A torn/corrupt tail, carrying WHICH condition fired ([`JournalCorruptCause`]). The walk stopped at
    /// the last VALID record and never advanced into the suspect bytes.
    Corrupt(JournalCorruptCause),
}

/// The ONE `[len: u32 LE][check: u32 FNV-1a32(payload)][payload: msgpack or JSON (JournalRecord)]`
/// frame walk, shared by all three journal readers (`scan_written` resume-cursor, `read_all`
/// replay, `scan_segment_sequences` prune-bounds) — each layers its own aggregation over `on_record`.
/// Walks from `start`, invoking `on_record` with every successfully-decoded owned record, and
/// returns the cursor where it stopped plus WHY ([`JournalWalkEnd`], carrying [`JournalCorruptCause`] on a torn
/// end — the cursor and the cause are what a caller needs to LOG the halt, and `read_all` does).
///
/// OOB-LEN HALT POLICY (canonical, adopted from `read_all` and now applied uniformly here): an
/// out-of-bounds length prefix is a torn LENGTH write, so it is classified as `Corrupt` — the
/// walk STOPS at it and never advances past it, exactly like a check mismatch or a parse failure.
/// It is deliberately kept DISTINCT from the clean `len==0` end so a multi-segment caller
/// (`read_all`) can halt replay journal-WIDE on corruption while still crossing a clean segment
/// boundary; the single-segment callers (`scan_written`, `scan_segment_sequences`) stop either way.
/// Single-siting this walk keeps every torn-tail decision identical and stops the three former
/// copies from drifting apart.
pub(super) fn walk_frames(
    buffer: &[u8],
    start: usize,
    mut on_record: impl FnMut(JournalRecord),
) -> (usize, JournalWalkEnd) {
    walk_frames_until(buffer, start, |rec, _| {
        on_record(rec);
        true
    })
}

/// [`walk_frames`], handing `on_record` each record with the offset its frame ENDS at (the next
/// frame's start), and stopping right after a record it answers `false` for: then the walk returns
/// that frame's end and [`JournalWalkEnd::Clean`], which the caller tells apart by its own answer.
pub(super) fn walk_frames_until(
    buffer: &[u8],
    start: usize,
    mut on_record: impl FnMut(JournalRecord, usize) -> bool,
) -> (usize, JournalWalkEnd) {
    let mut current = start;
    loop {
        let payload = match checked_payload(buffer, current) {
            Ok(Some(payload)) => payload,
            Ok(None) => return (current, JournalWalkEnd::Clean),
            Err(cause) => return (current, JournalWalkEnd::Corrupt(cause)),
        };
        let Some(rec) = decode(payload) else {
            // The check PASSED and the payload still is not a record for this build — the one arm
            // that is a schema mismatch rather than a torn tail. This is the ONLY site that can
            // tell the two apart, because the check above has already gated it.
            return (current, JournalWalkEnd::Corrupt(JournalCorruptCause::ParseFailed));
        };
        current += 8 + payload.len();
        if !on_record(rec, current) {
            return (current, JournalWalkEnd::Clean);
        }
    }
}

/// The payload of the frame at `current` when its length and its check hold: `Ok(None)` at the clean end of the
/// written data (no room for a frame header, or a zero length), `Err` for a torn length or a failed check. The one
/// place a frame's bytes are judged, shared by [`walk_frames_until`] and [`walk_frame_payloads`].
pub(super) fn checked_payload(
    buffer: &[u8],
    current: usize,
) -> Result<Option<&[u8]>, JournalCorruptCause> {
    if current + 8 > buffer.len() {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buffer[current..current + 4].try_into().unwrap()) as usize;
    if len == 0 {
        return Ok(None); // clean end of written data (zero-filled tail)
    }
    if current + 8 + len > buffer.len() {
        // torn length prefix — never advance past it
        return Err(JournalCorruptCause::OobLen);
    }
    let check = u32::from_le_bytes(buffer[current + 4..current + 8].try_into().unwrap());
    let payload = &buffer[current + 8..current + 8 + len];
    if fnv1a32(payload) != check {
        // torn payload write / rotted bytes
        return Err(JournalCorruptCause::CheckMismatch);
    }
    Ok(Some(payload))
}

/// [`walk_frames`] WITHOUT decoding: `on_frame` gets each frame's start offset and its checked payload, and the walk
/// stops where [`walk_frames`] would at a torn length or a failed check. A payload that does not parse is not seen
/// here (nothing is parsed): the boot's search for its newest `Snapshot` (`Journal::read_from_latest_snap_located`)
/// reads a whole segment this way and decodes only from that frame on.
pub(super) fn walk_frame_payloads(
    buffer: &[u8],
    start: usize,
    mut on_frame: impl FnMut(usize, &[u8]),
) -> (usize, JournalWalkEnd) {
    let mut current = start;
    loop {
        match checked_payload(buffer, current) {
            Ok(Some(payload)) => {
                on_frame(current, payload);
                current += 8 + payload.len();
            }
            Ok(None) => return (current, JournalWalkEnd::Clean),
            Err(cause) => return (current, JournalWalkEnd::Corrupt(cause)),
        }
    }
}

/// The first bytes of every msgpack `JournalRecord::Snapshot` payload: the externally tagged enum is a one-entry map
/// (`0x81`) whose key is the variant's name as a fixstr of 8 bytes (`0xa8`). Pinned against a real encoded `Snapshot`
/// by `frame_tests.rs`'s `a_snapshot_payload_starts_with_its_tag_and_no_other_record_does`.
const SNAPSHOT_TAG: &[u8] = b"\x81\xa8Snapshot";

/// Whether `payload` (a frame whose check held) is a msgpack `Snapshot` record, read off its first bytes alone.
pub(super) fn is_snapshot_payload(payload: &[u8]) -> bool {
    payload.starts_with(SNAPSHOT_TAG)
}

/// Whether a walk that stopped at `cause` in the LAST listed segment stopped at a frame that may
/// simply not be written YET: its length is there and its payload or check is not, which reads as a
/// check mismatch. Since decision 0279 the write thread hands a span to the
/// kernel in ONE `pwrite`, so a reader racing it can see exactly that. The readers stop before such a
/// frame as they always did (never accepting it, never erroring) and stay SILENT about it; they still
/// return the halt, so the boot restore, which reads with no writer running, tells a crash's torn tail
/// apart as before.
///
/// Only a check mismatch: a frame in flight never runs past the file (a segment is sized before it is
/// written, a frame that does not fit rolls first, and a length copied in part is never LARGER than the
/// whole one), so an out-of-bounds length is a fault and is still warned about; nor does a frame whose
/// check passed fail to parse.
pub(super) fn is_pending_tail(last_segment: bool, cause: JournalCorruptCause) -> bool {
    last_segment && cause == JournalCorruptCause::CheckMismatch
}

#[path = "frame_tests.rs"]
#[cfg(test)]
mod frame_tests;
