//! WAL crash-recovery for in-flight appends, and the recovery sweep that replays it on open.
//!
//! FORMAT (`_wal.arrow`): an append-only sequence of self-describing frames, each EITHER the
//! CURRENT (v2) shape:
//!     [b"VWL2"][key_len: u32-le][commit_key: utf8]
//!     [has_supersede: u8][supersede_len: u32-le][supersede_key: utf8]   -- supersede_len+key OMITTED when has_supersede=0
//!     [ipc_len: u64-le][Arrow-IPC stream: schema + 1 batch]
//! OR the OLD (v1, pre-`supersede_key`) shape, which [`wal_read`] still recognizes and decodes (as
//! `supersede_key = None`, always — v1 predates the concept) but which [`encode_wal_frame`] never
//! writes any more:
//!     [b"VWAL"][key_len: u32-le][commit_key: utf8][ipc_len: u64-le][Arrow-IPC stream: schema + 1 batch]
//! The rows travel as Arrow IPC (the spec's named WAL format); the small outer frame carries the
//! commit_key (plus an OPTIONAL supersede_key in v2 — see below) and delimits records so the file
//! stays append-only (each record is its own complete IPC stream). A torn tail — a crash mid-write
//! of a frame — is tolerated on read: [`wal_read`] stops at the first frame that is short, carries
//! neither magic, or (v2 only) carries a `has_supersede` byte that is neither 0 nor 1, dropping only
//! that partial record, which by definition never reached its fsync and so never durably committed.
//!
//! ⚠ **The `supersede_key` field exists so a crash cannot separate "these rows are durable" from
//! "this OTHER file must be removed with them".** `DataFusionHist::commit_rows_inner`'s superseding
//! commit seals a new part AND removes a superseded one in ONE locked manifest publish — but the
//! publish is preceded by this WAL fsync, exactly like every other keyed append. Before this field
//! existed, the WAL recorded ONLY `(commit_key, batch)`: a crash between this fsync and that publish
//! meant [`DataFusionHist::recover_series`] replayed the seal alone on reopen, with no record that a
//! supersede had ever been requested — the superseded part survived untouched, the recovered
//! canonical part landed ALONGSIDE it, and the overlap was permanently doubled (a retry can never
//! reach the supersede logic again: it hits `Manifest::has_commit(commit_key)` and returns `Ok(0)`
//! before ever getting there). So `supersede_key` rides in the SAME WAL record as the rows it is
//! paired with, and [`DataFusionHist::recover_series`] replays the SAME exact-match removal the live
//! path would have made, in the SAME publish that recovers the seal.
//!
//! ⚠ **The magic bytes changed (`VWAL` -> `VWL2`) for that same field, and NOT reusing `VWAL` for
//! the new shape is load-bearing, not cosmetic.** A v1 record has no `has_supersede` byte at all —
//! the byte immediately after the key is actually the LOW byte of v1's `ipc_len: u64`. Had the new
//! reader kept the old magic and just started expecting a `has_supersede` byte there, it would
//! misinterpret that byte on every v1 record still on disk, and — per the "any anomaly is a torn
//! tail" posture above — silently discard an otherwise-durable, already-fsynced commit. That is
//! reachable on the ordinary deploy of the commit that introduced `supersede_key`: any box running
//! the OLD binary that gets killed (a deploy restart, a stop-timeout SIGKILL) with a WAL record
//! fsynced but not yet published is exactly the window this file exists to protect, and the NEW
//! binary's first `open()` is what would have silently dropped it. Giving v2 its own magic instead
//! means a v1 record is simply read under the v1 layout — nothing about it needs reinterpreting.
//!
//! KEYLESS appends (commit_key = None) are deliberately NOT WAL'd. Recovery replay is made
//! idempotent by the `Manifest::has_commit` guard; a keyless append has no key, so a replay could
//! not tell "already applied" from "not yet" and would duplicate rows. Keyless appends therefore
//! keep exactly today's durability (the manifest boundary) — no regression, just no extra
//! protection.
//!
//! **The manifest is the durability boundary**: `commit_rows` seals parquet part(s) THEN publishes
//! the manifest, so a crash BETWEEN those two steps would strand the sealed part(s) under no
//! manifest — invisible to reads, and the accepted append's rows lost. This WAL closes the window:
//! the accepted batch is logged + fsynced BEFORE sealing, and [`DataFusionHist::open`] replays any
//! WAL record whose commit_key is not yet in the manifest, then clears it.
//!
//! ⚠ **That boundary MOVED at manifest v3 and this file's ordering depends on where it is.** It
//! used to be "the whole manifest has been rewritten and renamed"; it is now "this publish's delta
//! frame has been appended and fsynced" (`super::manifest::publish`). The property
//! [`wal_rewrite_keeping_unapplied`] relies on is unchanged — a WAL record may be dropped only once
//! the commit it describes is DURABLE — but what it waits for is a few hundred bytes instead of the
//! whole manifest, which on the live box's largest series was 104 MB. Clearing the WAL before that
//! fsync, at either boundary, is the ordering inversion that would strand an append.

use std::fs::OpenOptions;
use std::io::{Cursor, ErrorKind, Write};
use std::path::{Path, PathBuf};

use datafusion::arrow::array::{ArrayRef, UInt32Array};
use datafusion::arrow::compute::take;
use datafusion::arrow::ipc::reader::StreamReader;
use datafusion::arrow::ipc::writer::StreamWriter;
use datafusion::arrow::record_batch::RecordBatch;

use crate::hist::DataError;
use crate::hist_maint::{Durability, WriteOpts};

use super::codec::i64_col;
use super::manifest::{Manifest, SeriesLock, publish, read_manifest, seal_into_manifest};
use super::{DataFusionHist, SupersedePlan, apply_supersede, io, plan_supersede, q};

const WAL: &str = "_wal.arrow";
const WAL_TMP: &str = "_wal.arrow.tmp";
/// Per-record frame magic for the CURRENT (v2, carries `supersede_key`) format — the only shape
/// [`encode_wal_frame`] ever WRITES. Also lets [`wal_read`] detect the end of the last intact
/// record (a torn tail from a crash mid-write) and stop cleanly instead of misparsing partial
/// bytes.
const WAL_MAGIC: &[u8; 4] = b"VWL2";
/// Per-record frame magic for the OLD (v1, pre-`supersede_key`) format — READ-ONLY: nothing in this
/// crate writes it any more. [`wal_read`] must recognize it and decode it under the v1 layout (no
/// `has_supersede` field) rather than the v2 one — see this module's doc for what silently
/// misreading a leftover v1 record as v2 would cost.
const WAL_MAGIC_V1: &[u8; 4] = b"VWAL";

/// Serialize one WAL record `(commit_key, supersede_key, rows)` to its on-disk frame (Arrow-IPC
/// payload + header). `supersede_key` rides beside `commit_key` so a crash before the publish that
/// would apply both cannot separate them on replay — see this module's doc.
fn encode_wal_frame(
    key: &str,
    supersede_key: Option<&str>,
    batch: &RecordBatch,
) -> Result<Vec<u8>, DataError> {
    let mut ipc = Vec::new();
    {
        let schema = batch.schema();
        let mut w = StreamWriter::try_new(&mut ipc, &schema).map_err(q)?;
        w.write(batch).map_err(q)?;
        w.finish().map_err(q)?;
    }
    let key = key.as_bytes();
    let sk = supersede_key.map(str::as_bytes);
    let mut frame = Vec::with_capacity(
        WAL_MAGIC.len() + 4 + key.len() + 1 + sk.map_or(0, |b| 4 + b.len()) + 8 + ipc.len(),
    );
    frame.extend_from_slice(WAL_MAGIC);
    frame.extend_from_slice(&(key.len() as u32).to_le_bytes());
    frame.extend_from_slice(key);
    match sk {
        Some(b) => {
            frame.push(1u8);
            frame.extend_from_slice(&(b.len() as u32).to_le_bytes());
            frame.extend_from_slice(b);
        }
        None => frame.push(0u8),
    }
    frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
    frame.extend_from_slice(&ipc);
    Ok(frame)
}

/// Append one accepted-append record to the series WAL and fsync it. This is the durability barrier
/// that MUST land before the parquet part is sealed (so a crash before the manifest publish replays).
/// `supersede_key` — see this module's doc — is carried through unchanged to [`recover_series`].
pub(super) fn wal_append(
    series_dir: &Path,
    key: &str,
    supersede_key: Option<&str>,
    batch: &RecordBatch,
) -> Result<(), DataError> {
    let frame = encode_wal_frame(key, supersede_key, batch)?;
    let mut f =
        OpenOptions::new().create(true).append(true).open(series_dir.join(WAL)).map_err(io)?;
    f.write_all(&frame).map_err(io)?;
    f.sync_all().map_err(io)?; // fsync: the record is durable before we proceed to seal
    Ok(())
}

/// Read every INTACT WAL record `(commit_key, supersede_key, rows)`, in append order. A torn tail
/// (partial final frame from a crash mid-append) is tolerated: framing stops at the first
/// short/!magic/malformed frame.
fn wal_read(series_dir: &Path) -> Result<Vec<(String, Option<String>, RecordBatch)>, DataError> {
    let bytes = match std::fs::read(series_dir.join(WAL)) {
        Ok(b) => b,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    let n = bytes.len();
    loop {
        if p + 8 > n {
            break; // clean EOF, or too short to even hold a magic + key_len — stop
        }
        // Recognize EITHER magic — see this module's doc for why v1 (`VWAL`, no `has_supersede`
        // field) must be decoded under its OWN layout rather than the v2 one.
        let magic = &bytes[p..p + 4];
        let is_v2 = magic == WAL_MAGIC;
        let is_v1 = magic == WAL_MAGIC_V1;
        if !is_v2 && !is_v1 {
            break; // clean EOF, or a torn/garbage tail — stop
        }
        let key_len = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        // v1 has no `has_supersede` byte, so its minimum trailing requirement is one field
        // shorter than v2's.
        if p + key_len + if is_v2 { 1 } else { 0 } > n {
            break; // torn tail: not even room for the key (+ the has_supersede byte, for v2)
        }
        let key = match std::str::from_utf8(&bytes[p..p + key_len]) {
            Ok(s) => s.to_string(),
            Err(_) => break, // corrupt key → treat as a torn tail
        };
        p += key_len;
        let supersede_key = if !is_v2 {
            None // v1 predates the concept — every v1 record decodes as supersede_key = None
        } else {
            let has_supersede = bytes[p];
            p += 1;
            if has_supersede == 0 {
                None
            } else if has_supersede == 1 {
                if p + 4 > n {
                    break; // torn tail
                }
                let sk_len = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
                p += 4;
                if p + sk_len > n {
                    break; // torn tail
                }
                let sk = match std::str::from_utf8(&bytes[p..p + sk_len]) {
                    Ok(s) => s.to_string(),
                    Err(_) => break, // corrupt supersede key → treat as a torn tail
                };
                p += sk_len;
                Some(sk)
            } else {
                break; // corrupt has_supersede byte (neither 0 nor 1) → treat as a torn/garbage tail
            }
        };
        if p + 8 > n {
            break; // torn tail
        }
        let ipc_len = u64::from_le_bytes(bytes[p..p + 8].try_into().unwrap()) as usize;
        p += 8;
        if p + ipc_len > n {
            break; // torn tail
        }
        let mut reader =
            StreamReader::try_new(Cursor::new(&bytes[p..p + ipc_len]), None).map_err(q)?;
        let batch = reader
            .next()
            .ok_or_else(|| DataError::Query("WAL record has no Arrow batch".into()))?
            .map_err(q)?;
        p += ipc_len;
        out.push((key, supersede_key, batch));
    }
    Ok(out)
}

/// Drop WAL records whose commit_key is now durable in the manifest (applied), keeping the rest.
/// Atomic: rewrite the survivors to a tmp file + rename, or unlink the WAL when nothing remains. In
/// steady state the just-committed record is the only one, so this simply removes the WAL file.
pub(super) fn wal_rewrite_keeping_unapplied(
    series_dir: &Path,
    m: &Manifest,
) -> Result<(), DataError> {
    let keep: Vec<(String, Option<String>, RecordBatch)> =
        wal_read(series_dir)?.into_iter().filter(|(k, _, _)| !m.has_commit(k)).collect();
    let wal_path = series_dir.join(WAL);
    if keep.is_empty() {
        match std::fs::remove_file(&wal_path) {
            Ok(()) => {}
            Err(ref e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        return Ok(());
    }
    let tmp = series_dir.join(WAL_TMP);
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        for (k, sk, b) in &keep {
            f.write_all(&encode_wal_frame(k, sk.as_deref(), b)?).map_err(io)?;
        }
        f.sync_all().map_err(io)?;
    }
    if std::fs::rename(&tmp, &wal_path).is_err() {
        let _ = std::fs::remove_file(&wal_path);
        std::fs::rename(&tmp, &wal_path).map_err(io)?;
    }
    Ok(())
}

/// Gather rows `idxs` from every column of `batch` (arrow `take`) — rebuilds the per-`date=` column
/// subsets a WAL batch is re-sealed from during recovery. `idxs` are always in range (they index the
/// batch's own rows), so `take` cannot fail here.
fn take_rows(batch: &RecordBatch, idxs: &[usize]) -> Vec<ArrayRef> {
    let indices = UInt32Array::from(idxs.iter().map(|&i| i as u32).collect::<Vec<u32>>());
    batch
        .columns()
        .iter()
        .map(|c| take(c.as_ref(), &indices, None).expect("wal recovery: take indices in range"))
        .collect()
}

/// Recursively collect series dirs (those directly containing a `_wal.arrow`) under `dir`.
/// Recovery-only tree walk — not the read path.
fn find_wal_series_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), DataError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    let mut has_wal = false;
    let mut subdirs = Vec::new();
    for entry in rd {
        let entry = entry.map_err(io)?;
        let ft = entry.file_type().map_err(io)?;
        if ft.is_dir() {
            subdirs.push(entry.path());
        } else if entry.path().file_name().and_then(|n| n.to_str()) == Some(WAL) {
            has_wal = true;
        }
    }
    if has_wal {
        out.push(dir.to_path_buf());
    }
    for sub in subdirs {
        find_wal_series_dirs(&sub, out)?;
    }
    Ok(())
}

impl DataFusionHist {
    // ---- WAL crash recovery (runs from `open`) ---------------------------------------------

    /// For every series under the root that still has a `_wal.arrow`, replay its un-published
    /// appends. A clean store (no WAL files) is a cheap tree walk that finds nothing. This is a
    /// startup maintenance sweep, NOT the hot read path, so walking the tree to locate WALs is fine
    /// (the "no directory LIST" rule is a read-path invariant).
    pub(super) fn recover(&self) -> Result<(), DataError> {
        let mut wal_dirs = Vec::new();
        find_wal_series_dirs(&self.root, &mut wal_dirs)?;
        for dir in wal_dirs {
            self.recover_series(&dir)?;
        }
        Ok(())
    }

    /// Replay one series' WAL under its lock: for each record whose commit_key is NOT yet in the
    /// manifest, decide the SAME `plan_supersede` a live call would (against the manifest as it
    /// stands here, BEFORE re-sealing — see that function's doc for why the order matters), re-seal
    /// a part (reusing [`seal_into_manifest`] — the exact live-append path), and — when the plan is
    /// an exact match — `apply_supersede` it in the SAME publish, then clear the WAL. Idempotent: a
    /// record whose key is already committed is skipped, so replaying an already-applied log (or
    /// replaying twice) adds nothing. Returns rows recovered.
    ///
    /// ⚠ A record whose `supersede_key` was folded into a multi-key part by compaction BEFORE this
    /// replay runs (the same hazard `plan_supersede` refuses live — see its doc) makes this whole
    /// recovery sweep fail rather than silently double rows; that failure surfaces from
    /// [`DataFusionHist::open`], refusing to hand back a store it cannot correctly recover.
    fn recover_series(&self, series_dir: &Path) -> Result<usize, DataError> {
        let _guard = SeriesLock::acquire(series_dir)?;
        let mut m = read_manifest(series_dir)?;
        let records = wal_read(series_dir)?;
        let mut recovered = 0usize;
        for (key, supersede_key, batch) in &records {
            if m.has_commit(key) {
                continue; // already applied — idempotent no-op
            }
            // Decide the SAME way the live path does, and just as strictly BEFORE sealing — see
            // `plan_supersede`'s doc for why checking after would let the freshly-sealed part
            // answer its own "already folded away" question.
            let plan: Option<SupersedePlan> =
                supersede_key.as_deref().map(|sk| plan_supersede(&m, sk)).transpose()?;
            let exact_matches: Option<&[(String, String)]> = match &plan {
                Some(SupersedePlan::ExactMatch(parts)) => Some(parts.as_slice()),
                _ => None,
            };
            let stamp_extra_key =
                if exact_matches.is_some() { supersede_key.as_deref() } else { None };

            let ts: Vec<i64> = i64_col(batch, "ts")?.values().to_vec();
            let schema = batch.schema();
            let (rows, mut frame) = seal_into_manifest(
                series_dir,
                &mut m,
                Some(key),
                stamp_extra_key,
                &ts,
                &schema,
                |idxs| take_rows(batch, idxs),
                WriteOpts::live(),
            )?;
            recovered += rows;
            let mut to_unlink: Vec<PathBuf> = Vec::new();
            if let Some(parts) = exact_matches {
                to_unlink = apply_supersede(series_dir, &mut m, &mut frame, parts);
            }
            // publish per record (each is atomic + idempotent). `fold_bytes` is read off the store
            // so a recovery on a test handle folds at the same threshold its appends do.
            publish(series_dir, &mut m, frame, Durability::Fsync, self.fold_bytes())?;
            // manifest-first: only NOW unlink a superseded file, same as the live path.
            for p in to_unlink {
                let _ = std::fs::remove_file(p);
            }
        }
        // every WAL record is now applied (replayed above, or already in the manifest) → clear it
        wal_rewrite_keeping_unapplied(series_dir, &m)?;
        Ok(recovered)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};

    use super::*;

    fn one_col_batch(ts: &[i64]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("ts", DataType::Int64, false)]));
        let col: ArrayRef = Arc::new(Int64Array::from(ts.to_vec()));
        RecordBatch::try_new(schema, vec![col]).unwrap()
    }

    /// Byte-for-byte what the PRE-this-task `encode_wal_frame` produced: `[VWAL][key_len][key]
    /// [ipc_len][ipc]` — no `has_supersede` field at all. A binary built before `supersede_key`
    /// existed could still leave exactly this on disk, unrecovered, across a deploy to a binary
    /// carrying the v2 format — see this module's doc for why that must not be silently discarded.
    fn encode_v1_frame(key: &str, batch: &RecordBatch) -> Vec<u8> {
        let mut ipc = Vec::new();
        {
            let schema = batch.schema();
            let mut w = StreamWriter::try_new(&mut ipc, &schema).unwrap();
            w.write(batch).unwrap();
            w.finish().unwrap();
        }
        let key_b = key.as_bytes();
        let mut frame = Vec::with_capacity(WAL_MAGIC_V1.len() + 4 + key_b.len() + 8 + ipc.len());
        frame.extend_from_slice(WAL_MAGIC_V1);
        frame.extend_from_slice(&(key_b.len() as u32).to_le_bytes());
        frame.extend_from_slice(key_b);
        frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
        frame.extend_from_slice(&ipc);
        frame
    }

    #[test]
    fn a_pre_supersede_v1_wal_record_is_still_read_not_discarded_as_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let batch = one_col_batch(&[0, 1_000]);
        let bytes = encode_v1_frame("legacy-key", &batch);
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();

        let records = wal_read(dir.path()).unwrap();
        assert_eq!(
            records.len(),
            1,
            "a v1 record must be read, not silently dropped as a torn tail"
        );
        let (key, supersede_key, read_batch) = &records[0];
        assert_eq!(key, "legacy-key");
        assert_eq!(
            *supersede_key, None,
            "v1 predates supersede_key — it must always decode as None"
        );
        assert_eq!(read_batch.num_rows(), 2);
    }

    #[test]
    fn a_v1_record_followed_by_a_v2_record_reads_both_in_order() {
        // Realistic shape for the actual deploy hazard: an old-format record left over from BEFORE
        // the upgrade, immediately followed by a new-format record appended AFTER it (the box
        // restarted on the new binary and made one more commit before anyone reopened the store).
        let dir = tempfile::tempdir().unwrap();
        let v1 = encode_v1_frame("old-key", &one_col_batch(&[0]));
        let v2 =
            encode_wal_frame("new-key", Some("provisional"), &one_col_batch(&[1_000])).unwrap();
        let mut bytes = v1;
        bytes.extend_from_slice(&v2);
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();

        let records = wal_read(dir.path()).unwrap();
        assert_eq!(records.len(), 2, "both records must be read, in append order");
        assert_eq!(records[0].0, "old-key");
        assert_eq!(records[0].1, None);
        assert_eq!(records[1].0, "new-key");
        assert_eq!(records[1].1, Some("provisional".to_string()));
    }
}
