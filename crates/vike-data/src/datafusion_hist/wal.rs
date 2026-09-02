//! WAL crash-recovery for in-flight appends, and the recovery sweep that replays it on open.
//!
//! FORMAT (`_wal.arrow`): an append-only sequence of self-describing frames, each
//!     [b"VWAL"][key_len: u32-le][commit_key: utf8][ipc_len: u64-le][Arrow-IPC stream: schema + 1 batch]
//! The rows travel as Arrow IPC (the spec's named WAL format); the small outer frame carries the
//! commit_key and delimits records so the file stays append-only (each record is its own complete IPC
//! stream). A torn tail — a crash mid-write of a frame — is tolerated on read: [`wal_read`] stops at
//! the first frame that is short or lacks the magic, dropping only that partial record, which by
//! definition never reached its fsync and so never durably committed.
//!
//! KEYLESS appends (commit_key = None) are deliberately NOT WAL'd. Recovery replay is made idempotent
//! by the "commit_key already in manifest.commits" guard; a keyless append has no key, so a replay
//! could not tell "already applied" from "not yet" and would duplicate rows. Keyless appends therefore
//! keep exactly today's durability (the manifest boundary) — no regression, just no extra protection.
//!
//! **The manifest is the durability boundary**: `commit_rows` seals parquet part(s) THEN republishes
//! the manifest by atomic rename, so a crash BETWEEN those two steps would strand the sealed part(s)
//! under no manifest — invisible to reads, and the accepted append's rows lost. This WAL closes the
//! window: the accepted batch is logged + fsynced BEFORE sealing, and [`DataFusionHist::open`]
//! replays any WAL record whose commit_key is not yet in the manifest, then clears it.

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
use super::manifest::{read_manifest, seal_into_manifest, write_manifest, Manifest, SeriesLock};
use super::{io, q, DataFusionHist};

const WAL: &str = "_wal.arrow";
const WAL_TMP: &str = "_wal.arrow.tmp";
/// Per-record frame magic — lets [`wal_read`] detect the end of the last intact record (a torn tail
/// from a crash mid-write) and stop cleanly instead of misparsing partial bytes.
const WAL_MAGIC: &[u8; 4] = b"VWAL";

/// Serialize one WAL record `(commit_key, rows)` to its on-disk frame (Arrow-IPC payload + header).
fn encode_wal_frame(key: &str, batch: &RecordBatch) -> Result<Vec<u8>, DataError> {
    let mut ipc = Vec::new();
    {
        let schema = batch.schema();
        let mut w = StreamWriter::try_new(&mut ipc, &schema).map_err(q)?;
        w.write(batch).map_err(q)?;
        w.finish().map_err(q)?;
    }
    let key = key.as_bytes();
    let mut frame = Vec::with_capacity(WAL_MAGIC.len() + 4 + key.len() + 8 + ipc.len());
    frame.extend_from_slice(WAL_MAGIC);
    frame.extend_from_slice(&(key.len() as u32).to_le_bytes());
    frame.extend_from_slice(key);
    frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
    frame.extend_from_slice(&ipc);
    Ok(frame)
}

/// Append one accepted-append record to the series WAL and fsync it. This is the durability barrier
/// that MUST land before the parquet part is sealed (so a crash before the manifest publish replays).
pub(super) fn wal_append(
    series_dir: &Path,
    key: &str,
    batch: &RecordBatch,
) -> Result<(), DataError> {
    let frame = encode_wal_frame(key, batch)?;
    let mut f =
        OpenOptions::new().create(true).append(true).open(series_dir.join(WAL)).map_err(io)?;
    f.write_all(&frame).map_err(io)?;
    f.sync_all().map_err(io)?; // fsync: the record is durable before we proceed to seal
    Ok(())
}

/// Read every INTACT WAL record `(commit_key, rows)`, in append order. A torn tail (partial final
/// frame from a crash mid-append) is tolerated: framing stops at the first short/!magic frame.
fn wal_read(series_dir: &Path) -> Result<Vec<(String, RecordBatch)>, DataError> {
    let bytes = match std::fs::read(series_dir.join(WAL)) {
        Ok(b) => b,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    let n = bytes.len();
    loop {
        if p + 8 > n || &bytes[p..p + 4] != WAL_MAGIC {
            break; // clean EOF, or a torn/garbage tail — stop
        }
        let key_len = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap()) as usize;
        p += 8;
        if p + key_len + 8 > n {
            break; // torn tail
        }
        let key = match std::str::from_utf8(&bytes[p..p + key_len]) {
            Ok(s) => s.to_string(),
            Err(_) => break, // corrupt key → treat as a torn tail
        };
        p += key_len;
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
        out.push((key, batch));
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
    let keep: Vec<(String, RecordBatch)> = wal_read(series_dir)?
        .into_iter()
        .filter(|(k, _)| !m.commits.iter().any(|c| c == k))
        .collect();
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
        for (k, b) in &keep {
            f.write_all(&encode_wal_frame(k, b)?).map_err(io)?;
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
    /// manifest, re-seal a part + publish (reusing [`seal_into_manifest`] — the exact live-append
    /// path), then clear the WAL. Idempotent: a record whose key is already committed is skipped, so
    /// replaying an already-applied log (or replaying twice) adds nothing. Returns rows recovered.
    fn recover_series(&self, series_dir: &Path) -> Result<usize, DataError> {
        let _guard = SeriesLock::acquire(series_dir)?;
        let mut m = read_manifest(series_dir)?;
        let records = wal_read(series_dir)?;
        let mut recovered = 0usize;
        for (key, batch) in &records {
            if m.commits.iter().any(|c| c == key) {
                continue; // already applied — idempotent no-op
            }
            let ts: Vec<i64> = i64_col(batch, "ts")?.values().to_vec();
            let schema = batch.schema();
            recovered += seal_into_manifest(
                series_dir,
                &mut m,
                Some(key),
                &ts,
                &schema,
                |idxs| take_rows(batch, idxs),
                WriteOpts::live(),
            )?;
            write_manifest(series_dir, &m, Durability::Fsync)?; // publish per record (each is atomic + idempotent)
        }
        // every WAL record is now applied (replayed above, or already in the manifest) → clear it
        wal_rewrite_keeping_unapplied(series_dir, &m)?;
        Ok(recovered)
    }
}
