//! The manifest DELTA LOG: the append-only half of the base+delta manifest, and the framing that
//! makes a crash mid-append indistinguishable from a commit that never happened.
//!
//! # Why this file exists
//!
//! [`super::manifest::write_manifest`] publishes a WHOLE manifest by atomic rename. That buys
//! all-or-nothing atomicity, and it costs one rewrite of every entry a series has ever recorded, on
//! every append. MEASURED on the live data box (2026-09-16,
//! `docs/decisions/0060-the-manifest-rewrite-is-the-write-amplification.md`): the recorder wrote
//! **2.72 TB/day** to persist **~0.74 GB/day** of tape, and **96.5%** of those device writes were
//! this rewrite. One series' manifest is 104 MB; adding a part to it rewrote all 104 MB.
//!
//! So a publish becomes an APPEND: one [`DeltaFrame`] describing what changed, fsynced, and the
//! whole-file write happens only at a FOLD (see [`super::manifest::fold_base`]). The base manifest
//! keeps exactly the role it had — it is still the atomically-published, rebuildable file index —
//! it is just published every few thousand commits instead of every one.
//!
//! # FORMAT
//!
//! ```text
//! [b"VMDF"][payload_len: u32-le][crc32: u32-le][payload: compact JSON of `DeltaFrame`]
//! ```
//!
//! The outer framing is deliberately the shape [`super::wal`]'s `_wal.arrow` already uses — a
//! magic, a length, a payload — because that file had already solved the torn-tail problem this one
//! inherits, and inventing a second answer to it would mean two crash stories to keep in step.
//!
//! # Crash safety: what a torn frame is, and why dropping it is correct
//!
//! [`delta_read_with_end`] stops at the first frame that is short, lacks the magic, or fails its
//! CRC, and returns everything before it. A frame reaches disk only through [`delta_append`], which issues
//! one `write_all` and then an fsync before returning — so a frame that is not intact on disk is
//! one whose fsync never returned, which means its caller never returned either, which means the
//! commit it describes never durably committed. Dropping it is not a repair; it is the definition
//! of where the log ends.
//!
//! ⚠ **The CRC is the one place this is STRICTER than `wal.rs`, and it is not decoration.** A
//! frame's header and its payload can land in different pages, so a crash can leave a
//! structurally-complete header pointing at a payload that is partly the previous contents of the
//! block, or zeroes. `wal.rs` survives that by accident — its payload is an Arrow IPC stream, which
//! carries its own internal framing, so a mangled one fails to parse. This payload is JSON, and a
//! truncated JSON document is *usually* a parse error but a mangled one need not be. The CRC turns
//! "usually" into "always", and it costs a few hundred bytes of hashing per commit.
//!
//! # Ordering: a reader reads the LOG FIRST, then the BASE
//!
//! A fold publishes the new base and THEN clears the log. A reader that read the base first and the
//! log second could read an OLD base (from before the fold's rename) and an ALREADY-CLEARED log
//! (from after the truncation), and would then be missing every frame the fold had just folded in.
//! Reading in the other order cannot lose anything: if the log is empty then the fold that cleared
//! it had already published its base, so the base read AFTERWARDS carries those frames; and if the
//! log is not empty its frames are replayed over whatever base is read, with [`DeltaFrame::version`]
//! deciding which of them that base already contains. [`super::manifest::read_manifest`] performs
//! the two reads in that order and says so.
//!
//! # Idempotent replay is what makes a crashed FOLD harmless
//!
//! Every frame carries the manifest `version` it produces, and replay applies only frames whose
//! version is ABOVE the base's. A fold that publishes its base and is killed before clearing the
//! log therefore replays frames the base already holds — and skips every one of them. Without that
//! guard a crashed fold would double-apply, which for a `files_add` means one `FileEntry` twice and
//! so one part read twice.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hist::DataError;
use crate::hist_maint::Durability;

use super::manifest::FileEntry;
use super::{fsync_dir, io, q};

/// The delta log's filename, beside `_manifest.json` in the series leaf.
pub(super) const DELTA: &str = "_manifest.delta";

/// Per-frame magic. Deliberately NOT `wal.rs`'s `VWAL`, so neither file can be replayed as the
/// other by a tool that guesses from the first four bytes.
const DELTA_MAGIC: &[u8; 4] = b"VMDF";

/// Bytes of header before every payload: magic + `payload_len` + `crc32`.
const HEADER_LEN: usize = 4 + 4 + 4;

/// One published change to a series manifest — the unit the log frames and the unit replay applies.
///
/// Every collection field is skipped when empty, so the common case (a live append: one part added,
/// carrying its own commit key) serialises to a couple of hundred bytes rather than to a schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct DeltaFrame {
    /// The manifest `version` AFTER this frame is applied. Replay skips a frame whose version the
    /// base already carries, which is what makes a crashed fold a no-op rather than a doubling.
    ///
    /// ⚠ Monotonicity is load-bearing beyond bookkeeping: `DataFusionHist::compact_dir_inner`
    /// embeds `version + 1` in its merge output's FILE NAME, so a version that went backwards would
    /// rename a live part onto another one.
    pub(super) version: u64,
    /// Parts sealed by this publish. Their bytes and their directory entries are already fsynced by
    /// the time this frame is written — see `DataFusionHist::commit_rows`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) files_add: Vec<FileEntry>,
    /// Parts this publish removes, as `(name, date)`. Matching is on BOTH, because part names are
    /// unique within a `date=` directory and NOT across a series.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) files_rm: Vec<(String, String)>,
    /// ⚠ **The Q2 hatch, and deliberately unused by every producer in this tree today.** Commit
    /// keys that join the idempotency log carrying NO file — the shape `Manifest::orphan_commits`
    /// holds. It exists so a future answer to "what does the log retain, and for how long"
    /// (`docs/decisions/0060-…`'s still-open Q2) can be expressed as a FRAME rather than as a
    /// second format bump. Nothing writes a non-empty value; the v2 migration seeds the base's
    /// orphan set directly instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) keys_add: Vec<String>,
    /// ⚠ **The other half of the Q2 hatch.** Commit keys that LEAVE the idempotency log while the
    /// part carrying them STAYS — i.e. an expiry policy, as opposed to today's rule where a key
    /// lives exactly as long as some part that carries it. Nothing writes a non-empty value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) keys_rm: Vec<String>,
}

/// CRC-32/IEEE, bitwise and table-free.
///
/// Written out rather than taken from a crate because this workspace's dependency surface is part
/// of the merge gate (`deny.toml`), because a frame is a few hundred bytes so a 1 KiB static table
/// would not pay for itself, and because this is the whole of the algorithm. The polynomial is the
/// reflected `0xEDB88320` — the CRC zlib, gzip and PNG use — chosen because its check value is
/// published, which is what lets the test below pin this loop against something outside itself.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            // Branch-free: `mask` is all-ones when the low bit is set, all-zeros otherwise.
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Serialize one frame to its on-disk bytes (header + compact JSON payload).
fn encode_frame(f: &DeltaFrame) -> Result<Vec<u8>, DataError> {
    // COMPACT, never `to_vec_pretty`: this is the byte count the whole record exists to reduce, and
    // pretty-printing a `FileEntry` roughly triples it.
    let payload = serde_json::to_vec(f).map_err(q)?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(DELTA_MAGIC);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Append one frame to the series' delta log and — under [`Durability::Fsync`] — make it durable.
///
/// **This is the new durability boundary**, and it replaces the manifest publish in exactly one
/// ordering: `DataFusionHist::commit_rows` may clear an applied WAL record only once this has
/// returned, for the same reason it previously waited on [`super::manifest::write_manifest`] — a
/// crash that lost the publish while the WAL was already gone would strand the append. The fsync
/// here is over a few hundred bytes where that one was over the whole manifest.
///
/// The FIRST frame CREATES the file, and a fresh file's NAME is durable only once its containing
/// directory is fsynced; without that a crash could lose the whole log while its bytes survived
/// under no name. Subsequent frames need no directory fsync — the name already exists.
///
/// [`Durability::Bulk`] skips both fsyncs, exactly as `write_manifest` does and for the same
/// reason: a bulk import keeps no WAL and its source file is still on disk, so a lost publish costs
/// a re-run rather than data.
pub(super) fn delta_append(
    series_dir: &Path,
    frame: &DeltaFrame,
    durability: Durability,
) -> Result<(), DataError> {
    let path = series_dir.join(DELTA);
    let is_new = !path.exists();
    let bytes = encode_frame(frame)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(io)?;
    f.write_all(&bytes).map_err(io)?;
    if durability == Durability::Fsync {
        f.sync_all().map_err(io)?;
        if is_new {
            fsync_dir(series_dir)?;
        }
    }
    Ok(())
}

/// Every INTACT frame in the log, in append order, **and the byte length of that intact prefix**.
///
/// The second half of the return is not bookkeeping. A torn tail is dropped on READ, but the bytes
/// are still on disk, and [`delta_append`] opens the file `append` — so without this the next
/// commit would land AFTER the garbage, where no reader would ever reach it, and every commit after
/// that too. The store would accept writes and silently lose them from the instant of one crash.
/// (`wal.rs` never had this problem because `wal_rewrite_keeping_unapplied` rewrites its whole file
/// after every publish; this log is only ever truncated at a fold.)
///
/// So the length is carried up to [`super::manifest::publish`], which truncates the tail off UNDER
/// THE SERIES LOCK before appending. Repair is a writer's job: a reader is lock-free and must never
/// rewrite a file another process may be appending to.
///
/// A missing log is an EMPTY log, not an error: that is the state of a store whose base was just
/// folded, and of every store this build has not yet written to.
pub(super) fn delta_read_with_end(series_dir: &Path) -> Result<(Vec<DeltaFrame>, u64), DataError> {
    let bytes = match std::fs::read(series_dir.join(DELTA)) {
        Ok(b) => b,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(io(e)),
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    let n = bytes.len();
    loop {
        if p + HEADER_LEN > n || &bytes[p..p + 4] != DELTA_MAGIC {
            break; // clean EOF, or a torn/garbage tail — stop
        }
        let len = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().expect("4 bytes")) as usize;
        let want = u32::from_le_bytes(bytes[p + 8..p + 12].try_into().expect("4 bytes"));
        let start = p + HEADER_LEN;
        // `checked_add`, not `start + len`: `len` comes off a header that may be garbage, so it can
        // be any u32. On a 64-bit target the sum cannot overflow and the plain form would do; on a
        // narrower one it wraps, the bounds check below passes, and the slice panics on a file the
        // reader is supposed to tolerate. A torn tail must never be able to abort a read.
        let Some(end) = start.checked_add(len) else {
            break;
        };
        if end > n {
            break; // torn tail: the header landed and the payload did not
        }
        let payload = &bytes[start..end];
        if crc32(payload) != want {
            break; // the header landed and the payload is damaged — see the module doc
        }
        // A payload that passes its CRC and still fails to parse is NOT a torn tail: those bytes
        // are exactly what some writer produced and fsynced. That is a build disagreeing with its
        // own format, and silently stopping there would drop a commit that durably happened.
        let frame: DeltaFrame = serde_json::from_slice(payload).map_err(|e| {
            DataError::Query(format!(
                "manifest delta frame at byte {p} of {} is intact (CRC ok) but did not parse — \
                 this is a format disagreement, not a torn tail: {e}",
                series_dir.join(DELTA).display()
            ))
        })?;
        out.push(frame);
        p = end;
    }
    Ok((out, p as u64))
}

/// The log's size in bytes — what the fold threshold is measured against. A missing log is 0.
pub(super) fn delta_len(series_dir: &Path) -> u64 {
    std::fs::metadata(series_dir.join(DELTA)).map(|m| m.len()).unwrap_or(0)
}

/// Cut a torn tail off the log, leaving its intact prefix. **Callers must hold the series lock.**
///
/// The bytes removed are, by construction, a frame that never reached its fsync — see the module
/// doc. Truncating is what lets the NEXT append land somewhere a reader will reach; leaving them
/// would make the store accept commits and silently lose them from the instant of one crash.
///
/// It is LOUD, at `warn`, naming the offset and the byte count. A torn tail is a crash artifact and
/// should be visible as one; and if this ever fires because the READER is wrong rather than because
/// the file is torn, the log line is the only thing that would say so before the bytes go.
pub(super) fn delta_truncate(series_dir: &Path, intact_len: u64) -> Result<(), DataError> {
    let path = series_dir.join(DELTA);
    let f = OpenOptions::new().write(true).open(&path).map_err(io)?;
    let had = f.metadata().map_err(io)?.len();
    if had <= intact_len {
        return Ok(());
    }
    tracing::warn!(
        log = %path.display(),
        intact_len,
        dropped_bytes = had - intact_len,
        "manifest delta log: a torn tail is being cut back to its last intact frame (a crash \
         mid-append). Those bytes never reached their fsync, so the commit they describe never \
         durably happened."
    );
    f.set_len(intact_len).map_err(io)?;
    f.sync_all().map_err(io)?;
    Ok(())
}

/// Remove the log. Called ONLY after a base carrying every one of its frames has been durably
/// published — the ordering the module doc's fold section argues.
pub(super) fn delta_clear(series_dir: &Path) -> Result<(), DataError> {
    match std::fs::remove_file(series_dir.join(DELTA)) {
        Ok(()) => Ok(()),
        Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fe(name: &str, key: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            date: "2026-09-16".to_string(),
            ts_min: 1,
            ts_max: 2,
            rows: 3,
            commit_keys: vec![key.to_string()],
        }
    }

    fn frame(version: u64, name: &str, key: &str) -> DeltaFrame {
        DeltaFrame { version, files_add: vec![fe(name, key)], ..Default::default() }
    }

    /// The CRC only earns its lines if it has the property it was added for, so it is pinned
    /// against the polynomial's own PUBLISHED check value rather than against this loop's output —
    /// otherwise a rewrite of the loop would silently redefine what "intact" means.
    #[test]
    fn crc32_is_the_ieee_one_and_separates_neighbouring_payloads() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926, "the IEEE CRC-32 check value");
        assert_eq!(crc32(b""), 0);
        assert_ne!(crc32(br#"{"version":1}"#), crc32(br#"{"version":2}"#));
    }

    /// Round trip: what is appended is what is read, in order.
    #[test]
    fn frames_read_back_in_append_order() {
        let dir = tempfile::tempdir().unwrap();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            delta_append(dir.path(), &frame(i as u64 + 1, name, "k"), Durability::Fsync).unwrap();
        }
        let (got, end) = delta_read_with_end(dir.path()).unwrap();
        assert_eq!(
            end,
            std::fs::metadata(dir.path().join(DELTA)).unwrap().len(),
            "an intact log has no tail to cut"
        );
        assert_eq!(got.len(), 3);
        assert_eq!(got.iter().map(|f| f.version).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(got[2].files_add[0].name, "c");
    }

    /// A live append's frame is the byte count this whole design exists to shrink, so the size is
    /// ASSERTED rather than hoped for. The bound is deliberately loose; what it pins is the ORDER
    /// OF MAGNITUDE — hundreds of bytes, against the 104 MB the whole-file publish it replaces
    /// costs on the live box's largest series.
    #[test]
    fn a_live_appends_frame_is_hundreds_of_bytes_not_megabytes() {
        let f = frame(
            672_126,
            "part-00042.parquet",
            "live-polymarket-btc-updown-5m-book-1789540211281-1789540222464-17010",
        );
        let bytes = encode_frame(&f).unwrap();
        assert!(bytes.len() < 512, "a one-part append frame grew to {} bytes", bytes.len());
    }

    /// An absent log is an empty log — the state after every fold, and of a store this build has
    /// never written to.
    #[test]
    fn an_absent_log_is_empty_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(delta_read_with_end(dir.path()).unwrap().0.is_empty());
        assert_eq!(delta_len(dir.path()), 0);
        delta_clear(dir.path()).expect("clearing an absent log is a no-op");
    }

    /// Empty collections must not reach the wire — that is what keeps a frame small, and a
    /// regression here is paid on every commit the live box makes.
    #[test]
    fn empty_frame_fields_do_not_reach_the_wire() {
        let payload =
            serde_json::to_string(&DeltaFrame { version: 7, ..Default::default() }).unwrap();
        assert_eq!(payload, r#"{"version":7}"#);
    }
}
