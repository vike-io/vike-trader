//! WAL crash-recovery for in-flight appends, and the recovery sweep that replays it on open.
//!
//! FORMAT (`_wal.arrow`): an append-only sequence of self-describing frames, each in one of two
//! shapes. The v1 shape — the only one any release through v0.1.35 writes or reads:
//!     [b"VWAL"][key_len: u32-le][commit_key: utf8][ipc_len: u64-le][Arrow-IPC stream: schema + 1 batch]
//! and the v2 shape, which adds an optional `supersede_key`:
//!     [b"VWL2"][key_len: u32-le][commit_key: utf8]
//!     [has_supersede: u8][supersede_len: u32-le][supersede_key: utf8]   -- supersede_len+key OMITTED when has_supersede=0
//!     [ipc_len: u64-le][Arrow-IPC stream: schema + 1 batch]
//! [`wal_read_to_undecodable`] decodes both, interleaved in one file — a v1 record as
//! `supersede_key = None`, always, since v1 predates the concept.
//!
//! ⚠ **A record is WRITTEN in the OLDEST shape that can represent it** ([`encode_wal_frame`]): v1
//! when it carries no `supersede_key`, v2 only when it does. That rule is a ROLLBACK property, and
//! it exists because the release that introduced v2 did not follow it. A v0.1.35-or-older reader
//! stops at the first frame whose magic is not `VWAL`, and its replay then REMOVES the file — so a
//! box rolled back to such a release deletes, unread, every record from the first v2 frame onwards.
//! v0.1.36 and v0.1.37 wrote v2 for EVERY keyed append (`has_supersede = 0` for all but Dukascopy's
//! settled chunks), every recorder flush included: a crash inside the seal→publish window followed
//! by a rollback lost recorder tape, which cannot be re-fetched. Under the rule, what a rollback
//! loses is limited to the records that USE the new field — a pending superseding commit (a
//! Dukascopy settled chunk, which can be re-fetched) and the records after it in that one series.
//! A `has_supersede = 0` v2 frame is still READ, because those two releases left them on disk, and
//! becomes v1 the next time [`wal_rewrite_keeping_unapplied`] carries it, since a rewrite re-encodes
//! through the same writer. The next optional field follows the same rule: it gets a shape of its
//! own, written only by the records that carry it.
//!
//! The rows travel as Arrow IPC (the spec's named WAL format); the small outer frame carries the
//! commit_key (plus an OPTIONAL supersede_key in v2 — see below) and delimits records so the file
//! stays append-only (each record is its own complete IPC stream). A torn tail — a crash mid-write
//! of a frame — is tolerated on read: [`wal_read_to_undecodable`] stops at the first frame that is
//! short, carries neither magic, or (v2 only) carries a `has_supersede` byte that is neither 0 nor
//! 1, dropping only that partial record, which by definition never reached its fsync and so never
//! durably committed.
//! A length that runs past the end of the file, however large, is that same torn tail: every
//! length read off the disk is added with `checked_add`, as `super::delta`'s reader does.
//!
//! ⚠ **A record whose frame is whole but whose rows do not decode, or decode with no Int64 `ts`
//! column, sets THAT series' WAL aside, and the rest of the store opens**
//! ([`HistoryStoreDataFusion::recover_series`]). A power cut can keep a frame's header and length
//! while its payload pages read back as zeros; that one record used to fail `open` for the whole
//! store, and the datahub restart-looped. Now the intact records before it are replayed as usual,
//! the file is renamed beside itself to `_wal.arrow.undecodable-<UTC stamp>` ([`WAL_SET_ASIDE`];
//! never deleted, never read again), and an ERROR names the series and that file. The record and
//! every record after it are NOT replayed: none of them was acknowledged (an append answers only
//! after its publish), so a re-run backfill re-fetches them under the same commit keys. A LIVE
//! write that meets such a record after its own publish sets the file aside the same way
//! ([`wal_rewrite_keeping_unapplied`]), so it answers `Ok` and the next write on that series does
//! not meet the record again; it writes the unapplied records in front of it back to a fresh
//! `_wal.arrow`. Frames carry no checksum, so a payload whose bytes changed and still decode is not
//! detected; a checksum needs a new frame shape (the oldest-shape rule above) and is left for the
//! next format step.
//!
//! ⚠ **The `supersede_key` field exists so a crash cannot separate "these rows are durable" from
//! "this OTHER file must be removed with them".** `HistoryStoreDataFusion::commit_rows_inner`'s superseding
//! commit seals a new part AND removes a superseded one in ONE locked manifest publish — but the
//! publish is preceded by this WAL fsync, exactly like every other keyed append. Before this field
//! existed, the WAL recorded ONLY `(commit_key, batch)`: a crash between this fsync and that publish
//! meant [`HistoryStoreDataFusion::recover_series`] replayed the seal alone on reopen, with no record that a
//! supersede had ever been requested — the superseded part survived untouched, the recovered
//! canonical part landed ALONGSIDE it, and the overlap was permanently doubled (a retry can never
//! reach the supersede logic again: it hits `Manifest::has_commit(commit_key)` and returns `Ok(0)`
//! before ever getting there). So `supersede_key` rides in the SAME WAL record as the rows it is
//! paired with, and [`HistoryStoreDataFusion::recover_series`] replays the SAME exact-match removal the live
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
//! the accepted batch is logged + fsynced BEFORE sealing, and [`HistoryStoreDataFusion::open`] replays any
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

use crate::hist_maint::WriteOpts;
use vike_data::store::hist::HistoryStoreError;

use super::codec::i64_col;
use super::manifest::{Manifest, SeriesLock, seal_into_manifest};
use super::{HistoryStoreDataFusion, SupersedeStep, fsync_dir, io, q, unlink_superseded};

pub(super) const WAL: &str = "_wal.arrow";
/// The name a WAL holding a record that does not decode is renamed to, a UTC stamp after it
/// ([`set_wal_aside`]): evidence beside the series, which no reader replays and only a series
/// delete removes.
pub(super) const WAL_SET_ASIDE: &str = "_wal.arrow.undecodable-";
/// Per-record frame magic for the v2 shape, the one that can carry a `supersede_key` — WRITTEN only
/// for a record that carries one (the oldest-shape rule in this module's doc). Also lets
/// [`wal_read_to_undecodable`] detect the end of the last intact record (a torn tail from a crash
/// mid-write) and stop cleanly instead of misparsing partial bytes.
const WAL_MAGIC: &[u8; 4] = b"VWL2";
/// Per-record frame magic for the v1 shape, which has no `has_supersede` field: every record any
/// release through v0.1.35 wrote, and every record [`encode_wal_frame`] writes today WITHOUT a
/// supersede key — so that a box rolled back to such a release can still replay it.
/// [`wal_read_to_undecodable`] must decode it under the v1 layout rather than the v2 one — see this
/// module's doc for what silently misreading a v1 record as v2 would cost.
const WAL_MAGIC_V1: &[u8; 4] = b"VWAL";

/// Serialize one WAL record `(commit_key, supersede_key, rows)` to its on-disk frame (Arrow-IPC
/// payload + header), in the OLDEST shape that can represent it: v1 (`VWAL`) when there is no
/// `supersede_key`, v2 (`VWL2`) only when there is one to carry — this module's doc says what a
/// rollback loses when that rule is not kept. `supersede_key` rides beside `commit_key` so a crash
/// before the publish that would apply both cannot separate them on replay.
fn encode_wal_frame(
    key: &str,
    supersede_key: Option<&str>,
    batch: &RecordBatch,
) -> Result<Vec<u8>, HistoryStoreError> {
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
        WAL_MAGIC.len() + 4 + key.len() + sk.map_or(0, |b| 1 + 4 + b.len()) + 8 + ipc.len(),
    );
    frame.extend_from_slice(if sk.is_some() { WAL_MAGIC } else { WAL_MAGIC_V1 });
    frame.extend_from_slice(&(key.len() as u32).to_le_bytes());
    frame.extend_from_slice(key);
    if let Some(b) = sk {
        // v2 only. A v2 frame with `has_supersede = 0` is never written any more; it is still READ,
        // because v0.1.36 and v0.1.37 wrote one for every plain keyed append.
        frame.push(1u8);
        frame.extend_from_slice(&(b.len() as u32).to_le_bytes());
        frame.extend_from_slice(b);
    }
    frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
    frame.extend_from_slice(&ipc);
    Ok(frame)
}

/// Append one accepted-append record to the series WAL and fsync it. This is the durability barrier
/// that MUST land before the parquet part is sealed (so a crash before the manifest publish replays).
/// `supersede_key` — see this module's doc — is carried through unchanged to [`recover_series`].
/// A record that CREATES the file also fsyncs the directory, as `super::delta`'s `delta_append`
/// does: a fresh name is durable only then. The WAL is removed after each applied commit, so that is
/// every keyed commit in steady state; measured on the CI box's NVMe, about 0.3 ms each.
pub(super) fn wal_append(
    series_dir: &Path,
    key: &str,
    supersede_key: Option<&str>,
    batch: &RecordBatch,
) -> Result<(), HistoryStoreError> {
    let frame = encode_wal_frame(key, supersede_key, batch)?;
    let path = series_dir.join(WAL);
    let is_new = !path.exists();
    let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(io)?;
    f.write_all(&frame).map_err(io)?;
    f.sync_all().map_err(io)?; // fsync: the record is durable before we proceed to seal
    if is_new {
        fsync_dir(series_dir)?;
    }
    Ok(())
}

/// What [`wal_read_to_undecodable`] read.
struct WalRead {
    /// Every intact record in front of the first whole frame whose rows do not decode, in append
    /// order — all of them when there is no such frame.
    records: Vec<(String, Option<String>, RecordBatch)>,
    /// That frame's byte offset in the file, and why its rows did not decode.
    undecodable: Option<(usize, HistoryStoreError)>,
}

/// Read every INTACT WAL record `(commit_key, supersede_key, rows)`, in append order. A torn tail
/// (partial final frame from a crash mid-append) is tolerated: framing stops at the first
/// short/!magic/malformed frame. Reading also stops at the first whole frame whose rows do not
/// decode ([`decode_wal_rows`]) and says where; its callers set that WAL aside. An I/O error
/// reading the file is an `Err`.
fn wal_read_to_undecodable(series_dir: &Path) -> Result<WalRead, HistoryStoreError> {
    let bytes = match std::fs::read(series_dir.join(WAL)) {
        Ok(b) => b,
        Err(ref e) if e.kind() == ErrorKind::NotFound => {
            return Ok(WalRead { records: Vec::new(), undecodable: None });
        }
        Err(e) => return Err(io(e)),
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    let n = bytes.len();
    // `start + len` when that fits in the file; a length off the disk can be anything.
    let end_within = |start: usize, len: usize| start.checked_add(len).filter(|&end| end <= n);
    loop {
        let frame_start = p;
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
        let Some(key_end) = end_within(p, key_len).filter(|&end| !is_v2 || end < n) else {
            break; // torn tail: not even room for the key (+ the has_supersede byte, for v2)
        };
        let key = match std::str::from_utf8(&bytes[p..key_end]) {
            Ok(s) => s.to_string(),
            Err(_) => break, // corrupt key → treat as a torn tail
        };
        p = key_end;
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
                let Some(sk_end) = end_within(p, sk_len) else {
                    break; // torn tail
                };
                let sk = match std::str::from_utf8(&bytes[p..sk_end]) {
                    Ok(s) => s.to_string(),
                    Err(_) => break, // corrupt supersede key → treat as a torn tail
                };
                p = sk_end;
                Some(sk)
            } else {
                break; // corrupt has_supersede byte (neither 0 nor 1) → treat as a torn/garbage tail
            }
        };
        if p + 8 > n {
            break; // torn tail
        }
        let ipc_len = u64::from_le_bytes(bytes[p..p + 8].try_into().unwrap());
        p += 8;
        let Some(ipc_end) = usize::try_from(ipc_len).ok().and_then(|len| end_within(p, len)) else {
            break; // torn tail, however large the length
        };
        match decode_wal_rows(&bytes[p..ipc_end]) {
            Ok(batch) => out.push((key, supersede_key, batch)),
            Err(cause) => {
                return Ok(WalRead { records: out, undecodable: Some((frame_start, cause)) });
            }
        }
        p = ipc_end;
    }
    Ok(WalRead { records: out, undecodable: None })
}

/// The one batch of a record's Arrow-IPC stream. It must carry the Int64 `ts` column every codec
/// writes, because the replay re-seals a record by it: a batch without one is a bad record too.
fn decode_wal_rows(ipc: &[u8]) -> Result<RecordBatch, HistoryStoreError> {
    let batch = StreamReader::try_new(Cursor::new(ipc), None)
        .map_err(q)?
        .next()
        .ok_or_else(|| HistoryStoreError::Query("WAL record has no Arrow batch".into()))?
        .map_err(q)?;
    i64_col(&batch, "ts")?;
    Ok(batch)
}

/// Drop WAL records whose commit_key is now durable in the manifest (applied), keeping the rest.
/// Atomic: `vike_utility::atomic_file`'s `write_atomic` replaces the file with the survivors, or the WAL
/// is unlinked when nothing remains. In steady state the just-committed record is the only one, so
/// this simply removes the WAL file.
/// ⚠ **The WAL is never removed while it holds a survivor**, even when the replace fails: recovery
/// replays `_wal.arrow` and no other name, so a survivor that is only in a temp file is lost.
/// Survivors are re-encoded through [`encode_wal_frame`], so a plain record v0.1.36 or v0.1.37 left
/// as a `has_supersede = 0` v2 frame comes out of a rewrite as a v1 frame, content unchanged.
///
/// ⚠ **A record that does not decode sets the WAL aside here too, as recovery does** (this module's
/// doc). The live write calling this has already published, so failing here used to fail that
/// write and every later one on the series until the next `open`. The file is renamed
/// ([`set_wal_aside`]), an ERROR names the series, and the unapplied records in front of the bad
/// one are the survivors written back to a fresh `_wal.arrow`.
pub(super) fn wal_rewrite_keeping_unapplied(
    series_dir: &Path,
    m: &Manifest,
) -> Result<(), HistoryStoreError> {
    let WalRead { records, undecodable } = wal_read_to_undecodable(series_dir)?;
    if let Some((offset, cause)) = undecodable {
        let aside = set_wal_aside(series_dir)?;
        tracing::error!(
            series = %series_dir.display(),
            set_aside_to = %aside.display(),
            undecodable_at_byte = offset,
            intact_records_before_it = records.len(),
            cause = %cause,
            "WAL record does not decode, met by a live write after its publish: this series' \
             _wal.arrow was renamed aside whole and is never replayed; the write itself is \
             durable, the unapplied records before that record stay in a fresh _wal.arrow, it and \
             every record after it are not replayed (none was acknowledged); no other series is \
             touched. Remedy: re-run the backfill over this series' recent window (its commit keys \
             skip what is already stored)"
        );
    }
    wal_write_keeping_unapplied(series_dir, m, records)
}

/// [`wal_rewrite_keeping_unapplied`] over `records` already read under the series lock.
fn wal_write_keeping_unapplied(
    series_dir: &Path,
    m: &Manifest,
    records: Vec<(String, Option<String>, RecordBatch)>,
) -> Result<(), HistoryStoreError> {
    let keep: Vec<(String, Option<String>, RecordBatch)> =
        records.into_iter().filter(|(k, _, _)| !m.has_commit(k)).collect();
    let wal_path = series_dir.join(WAL);
    if keep.is_empty() {
        match std::fs::remove_file(&wal_path) {
            Ok(()) => {}
            Err(ref e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io(e)),
        }
        return Ok(());
    }
    let mut frames = Vec::new();
    for (k, sk, b) in &keep {
        frames.extend_from_slice(&encode_wal_frame(k, sk.as_deref(), b)?);
    }
    vike_utility::atomic_file::write_atomic(&wal_path, &frames).map_err(io)
}

/// Rename `series_dir`'s `_wal.arrow` to [`WAL_SET_ASIDE`] + a UTC stamp beside it, and say where it
/// went; nothing is deleted. A `-<n>` suffix keeps an earlier set-aside file, because a rename onto
/// an existing name replaces it on Unix and fails on Windows (as
/// `crates/vike-journal/src/writer/offline.rs`'s `set_aside` does for the journal).
fn set_wal_aside(series_dir: &Path) -> Result<PathBuf, HistoryStoreError> {
    let (y, mo, d, h, mi, s) = vike_model::time::utc_parts(vike_model::now_ms());
    let stamped = format!("{WAL_SET_ASIDE}{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z");
    let mut aside = series_dir.join(&stamped);
    let mut n = 1u32;
    while aside.exists() {
        aside = series_dir.join(format!("{stamped}-{n}"));
        n += 1;
    }
    std::fs::rename(series_dir.join(WAL), &aside).map_err(io)?;
    Ok(aside)
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
fn find_wal_series_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), HistoryStoreError> {
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

impl HistoryStoreDataFusion {
    // ---- WAL crash recovery (runs from `open`) ---------------------------------------------

    /// For every series under the root that still has a `_wal.arrow`, replay its un-published
    /// appends. A clean store (no WAL files) is a cheap tree walk that finds nothing. This is a
    /// startup maintenance sweep, NOT the hot read path, so walking the tree to locate WALs is fine
    /// (the "no directory LIST" rule is a read-path invariant). A series whose replay meets a
    /// supersede REFUSAL keeps that record, and a series whose WAL holds a record that does not
    /// decode has that WAL set aside; either way the sweep goes on to the next series — see
    /// [`Self::recover_series`]. Any other per-series error still fails the sweep, and `open`.
    pub(super) fn recover(&self) -> Result<(), HistoryStoreError> {
        let mut wal_dirs = Vec::new();
        find_wal_series_dirs(&self.root, &mut wal_dirs)?;
        for dir in wal_dirs {
            self.recover_series(&dir)?;
        }
        Ok(())
    }

    /// Replay one series' WAL under its lock: for each record whose commit_key is NOT yet in the
    /// manifest, decide its supersede through the SAME [`SupersedeStep`] a live call does (against
    /// the manifest as it stands here, BEFORE re-sealing — `plan_supersede`'s doc says why the order
    /// matters), re-seal a part (reusing [`seal_into_manifest`] — the exact live-append path), and
    /// publish the seal and the step's removal in ONE frame, then clear the WAL. Idempotent: a
    /// record whose key is already committed is skipped, so replaying an already-applied log (or
    /// replaying twice) adds nothing. Returns rows recovered.
    ///
    /// ⚠ **A supersede REFUSAL stops THIS series' replay, and nothing else.** A record whose
    /// `supersede_key` was folded into a multi-key part by compaction before this replay ran (the
    /// hazard `plan_supersede` refuses live — see its doc) cannot be applied exactly, and sealing it
    /// without its removal would double rows for good. That refusal used to fail the whole sweep,
    /// and with it [`HistoryStoreDataFusion::open`] for the whole store — so one series in that state held
    /// the datahub, and the recorder that runs inside it, in a restart loop that recorded nothing
    /// (`deploy/vike-datahub.service` restarts on failure). Now the refused record and every record
    /// after it stay in `_wal.arrow`, in order and unapplied (the ones before it are already
    /// published, per record, and the closing rewrite drops them as always); the refusal is logged
    /// at ERROR level with its remedy; and the sweep goes on to the next series. Nothing is written
    /// for the refused record and nothing is deleted, so the series holds less than the crash was
    /// about to commit, never more — and it is logged again at every `open` until someone resolves
    /// it. The later records wait rather than being replayed past it because order matters between
    /// a provisional record and its canonical twin: a series left exactly as the crash left it, minus
    /// what was already safe, is the one state that is easy to reason about. ⚠ Only the refusal takes
    /// this path (`HistoryStoreError::is_supersede_refusal`).
    ///
    /// ⚠ **A record whose rows do not decode, or carry no `ts`, sets this series' WAL aside, and
    /// nothing else** (this module's doc says why). The intact records in front of it are replayed above, THEN the file
    /// is renamed ([`set_wal_aside`]), so a crash in between replays them again, idempotently; a
    /// record a refusal kept is written back to a fresh `_wal.arrow`, so it is still met at every
    /// open. A lock timeout, an I/O error or an unreadable manifest still fail `open`, as before.
    fn recover_series(&self, series_dir: &Path) -> Result<usize, HistoryStoreError> {
        let guard = SeriesLock::acquire(series_dir)?;
        let mut m = self.manifest_to_write(&guard, series_dir)?;
        let WalRead { records, undecodable } = wal_read_to_undecodable(series_dir)?;
        let mut recovered = 0usize;
        for (i, (key, supersede_key, batch)) in records.iter().enumerate() {
            if m.has_commit(key) {
                continue; // already applied — idempotent no-op
            }
            // Decided by the live path's own step, and just as strictly BEFORE sealing.
            let step = match SupersedeStep::decide(&m, supersede_key.as_deref()) {
                Ok(step) => step,
                Err(refusal) if refusal.is_supersede_refusal() => {
                    let kept = records[i..].iter().filter(|(k, _, _)| !m.has_commit(k)).count();
                    tracing::error!(
                        series = %series_dir.display(),
                        commit_key = %key,
                        supersede_key = supersede_key.as_deref().unwrap_or_default(),
                        kept_records = kept,
                        refusal = %refusal,
                        "WAL replay REFUSED a superseding commit and STOPPED this series' replay: \
                         the refused record and every record after it stay in _wal.arrow, \
                         unapplied, and are met again at every open; the rest of the store opened \
                         normally. Remedy: delete this series (and any bar series resampled from \
                         it) and re-backfill; deleting a series removes its _wal.arrow with it"
                    );
                    break;
                }
                Err(e) => return Err(e),
            };
            let ts: Vec<i64> = i64_col(batch, "ts")?.values().to_vec();
            let schema = batch.schema();
            let (rows, frame) = seal_into_manifest(
                series_dir,
                &mut m,
                Some(key),
                step.stamp.as_slice(),
                &ts,
                &schema,
                |idxs| take_rows(batch, idxs),
                WriteOpts::live(),
            )?;
            recovered += rows;
            // publish per record (each is atomic + idempotent). `fold_bytes` is read off the store
            // so a recovery on a test handle folds at the same threshold its appends do.
            let published = step.publish(series_dir, &mut m, frame, self.fold_bytes())?;
            // manifest-first: only NOW unlink a superseded file, same as the live path.
            unlink_superseded(published.to_unlink);
        }
        if let Some((offset, cause)) = undecodable {
            let aside = set_wal_aside(series_dir)?;
            tracing::error!(
                series = %series_dir.display(),
                set_aside_to = %aside.display(),
                undecodable_at_byte = offset,
                intact_records_before_it = records.len(),
                rows_replayed = recovered,
                cause = %cause,
                "WAL record does not decode: this series' _wal.arrow was renamed aside whole and \
                 is never replayed; the intact records before that record were replayed, it and \
                 every record after it were not (none was acknowledged); the rest of the store \
                 opened normally. Remedy: re-run the backfill over this series' recent window (its \
                 commit keys skip what is already stored)"
            );
        }
        // Drop every record now applied (replayed above, or already in the manifest) — which is all
        // of them, unless a refusal stopped the loop and left its record and those after it.
        wal_write_keeping_unapplied(series_dir, &m, records)?;
        self.keep_manifest(&guard, series_dir, m);
        Ok(recovered)
    }
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};

    use super::*;

    fn one_col_batch(ts: &[i64]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("ts", DataType::Int64, false)]));
        let col: ArrayRef = Arc::new(Int64Array::from(ts.to_vec()));
        RecordBatch::try_new(schema, vec![col]).unwrap()
    }

    /// Every intact record, or the cause of the first one whose rows do not decode.
    fn wal_read(
        series_dir: &Path,
    ) -> Result<Vec<(String, Option<String>, RecordBatch)>, HistoryStoreError> {
        let read = wal_read_to_undecodable(series_dir)?;
        match read.undecodable {
            None => Ok(read.records),
            Some((_, cause)) => Err(cause),
        }
    }

    /// `batch` as the one-batch Arrow-IPC stream every frame shape carries.
    fn ipc_stream(batch: &RecordBatch) -> Vec<u8> {
        let mut ipc = Vec::new();
        {
            let schema = batch.schema();
            let mut w = StreamWriter::try_new(&mut ipc, &schema).unwrap();
            w.write(batch).unwrap();
            w.finish().unwrap();
        }
        ipc
    }

    /// The v1 frame, byte for byte: `[VWAL][key_len][key][ipc_len][ipc]`, no `has_supersede` field
    /// at all. It is what every release through v0.1.35 wrote, so a box upgraded across one can
    /// hold it on disk unrecovered — and it is the PINNED EXPECTATION for what [`encode_wal_frame`]
    /// writes today for a record with no supersede key, which is why it spells the magic as a
    /// literal rather than reading `WAL_MAGIC_V1`: an expectation that followed the constant would
    /// follow a mistake in it.
    fn encode_v1_frame(key: &str, batch: &RecordBatch) -> Vec<u8> {
        let ipc = ipc_stream(batch);
        let key_b = key.as_bytes();
        let mut frame = Vec::with_capacity(4 + 4 + key_b.len() + 8 + ipc.len());
        frame.extend_from_slice(b"VWAL");
        frame.extend_from_slice(&(key_b.len() as u32).to_le_bytes());
        frame.extend_from_slice(key_b);
        frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
        frame.extend_from_slice(&ipc);
        frame
    }

    /// FROZEN: what v0.1.36 and v0.1.37's `encode_wal_frame` wrote — EVERY record as a v2 frame, a
    /// record with no supersede key as `has_supersede = 0`. Those frames are on disk wherever either
    /// release ran, so the current reader must go on reading them; this helper is how a test plants
    /// one without the current writer, which no longer produces the plain form.
    fn v0_1_36_frame(key: &str, supersede_key: Option<&str>, batch: &RecordBatch) -> Vec<u8> {
        let ipc = ipc_stream(batch);
        let key_b = key.as_bytes();
        let mut frame = Vec::new();
        frame.extend_from_slice(b"VWL2");
        frame.extend_from_slice(&(key_b.len() as u32).to_le_bytes());
        frame.extend_from_slice(key_b);
        match supersede_key {
            Some(sk) => {
                frame.push(1u8);
                frame.extend_from_slice(&(sk.len() as u32).to_le_bytes());
                frame.extend_from_slice(sk.as_bytes());
            }
            None => frame.push(0u8),
        }
        frame.extend_from_slice(&(ipc.len() as u64).to_le_bytes());
        frame.extend_from_slice(&ipc);
        frame
    }

    /// FROZEN: the `v0.1.35` tag's `wal_read` loop — what a box rolled back to v0.1.35, or to any
    /// release since the WAL landed on 2026-07-06, runs on its first `open` before its replay
    /// REMOVES the file. Copied rather than derived, down to its own magic literal and file name, so
    /// that it keeps answering what the OLD binary does whatever this file's constants become. Only
    /// the name changed; like that release's reader, it returns `(commit_key, rows)` pairs.
    fn v0_1_35_wal_read(
        series_dir: &Path,
    ) -> Result<Vec<(String, RecordBatch)>, HistoryStoreError> {
        const WAL_MAGIC: &[u8; 4] = b"VWAL";
        let bytes = match std::fs::read(series_dir.join("_wal.arrow")) {
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
                .ok_or_else(|| HistoryStoreError::Query("WAL record has no Arrow batch".into()))?
                .map_err(q)?;
            p += ipc_len;
            out.push((key, batch));
        }
        Ok(out)
    }

    #[test]
    fn a_record_without_a_supersede_key_is_byte_identical_to_the_v1_frame() {
        let batch = one_col_batch(&[0, 1_000]);
        assert_eq!(
            encode_wal_frame("plain", None, &batch).unwrap(),
            encode_v1_frame("plain", &batch),
            "a record with no supersede key must be written as the v1 frame, byte for byte — the \
             only shape a release through v0.1.35 can read"
        );
    }

    #[test]
    fn the_v0_1_35_reader_reads_a_plain_record_the_current_writer_writes() {
        // THE rollback property itself: records appended by this writer with no supersede key are
        // replayed by a v0.1.35 binary, not deleted by it.
        let dir = tempfile::tempdir().unwrap();
        wal_append(dir.path(), "plain-1", None, &one_col_batch(&[0, 1_000])).unwrap();
        wal_append(dir.path(), "plain-2", None, &one_col_batch(&[2_000])).unwrap();
        let old = v0_1_35_wal_read(dir.path()).unwrap();
        let keys: Vec<(&str, usize)> =
            old.iter().map(|(k, b)| (k.as_str(), b.num_rows())).collect();
        assert_eq!(keys, [("plain-1", 2), ("plain-2", 1)], "v0.1.35 replays every plain record");

        // The residual the owner accepted (the follow-ups spec's question 3), pinned so it cannot
        // quietly widen: a SUPERSEDING record is still a v2 frame, so v0.1.35 stops at it — and at
        // every record after it in this series, plain or not.
        wal_append(dir.path(), "canonical", Some("provisional"), &one_col_batch(&[3_000])).unwrap();
        wal_append(dir.path(), "plain-3", None, &one_col_batch(&[4_000])).unwrap();
        assert_eq!(v0_1_35_wal_read(dir.path()).unwrap().len(), 2, "stops at the superseding one");
        assert_eq!(wal_read(dir.path()).unwrap().len(), 4, "the current reader reads all four");
    }

    #[test]
    fn a_superseding_record_still_writes_the_v2_frame() {
        let dir = tempfile::tempdir().unwrap();
        let batch = one_col_batch(&[0]);
        let frame = encode_wal_frame("canonical", Some("provisional"), &batch).unwrap();
        assert_eq!(&frame[..4], b"VWL2", "only the v2 shape can carry a supersede key");
        assert_eq!(
            frame,
            v0_1_36_frame("canonical", Some("provisional"), &batch),
            "the superseding v2 frame is byte-identical to what v0.1.36 wrote for it"
        );

        wal_append(dir.path(), "canonical", Some("provisional"), &batch).unwrap();
        let records = wal_read(dir.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "canonical");
        assert_eq!(records[0].1.as_deref(), Some("provisional"), "the supersede key round-trips");
        assert_eq!(records[0].2.num_rows(), 1);
    }

    #[test]
    fn a_rewrite_turns_a_keyless_v2_frame_into_a_v1_frame() {
        // What v0.1.36 and v0.1.37 left on disk for a plain keyed append that never published.
        let dir = tempfile::tempdir().unwrap();
        let batch = one_col_batch(&[0, 1_000]);
        std::fs::write(dir.path().join(WAL), v0_1_36_frame("pending", None, &batch)).unwrap();
        assert!(v0_1_35_wal_read(dir.path()).unwrap().is_empty(), "v0.1.35 cannot read it as is");

        // Nothing is committed, so the rewrite carries the record — through the current writer.
        wal_rewrite_keeping_unapplied(dir.path(), &Manifest::empty()).unwrap();

        assert_eq!(
            std::fs::read(dir.path().join(WAL)).unwrap(),
            encode_v1_frame("pending", &batch),
            "the carried record is now a v1 frame, byte for byte"
        );
        let old = v0_1_35_wal_read(dir.path()).unwrap();
        assert_eq!(old.len(), 1, "...which a rollback to v0.1.35 can replay again");
        assert_eq!(old[0].0, "pending");
    }

    #[cfg(unix)]
    #[test]
    fn creating_the_wal_syncs_the_directory_that_names_it() {
        // A fresh file's NAME is durable only once its directory is fsynced, as `delta_append` says
        // for the delta log; a power loss could otherwise keep the record's bytes under no name.
        use crate::datafusion_hist::parquet_io::DIRS_SYNCED;
        let dir = tempfile::tempdir().unwrap();
        let synced = || {
            DIRS_SYNCED.with(|d| d.borrow().iter().filter(|p| p.as_path() == dir.path()).count())
        };
        wal_append(dir.path(), "first", None, &one_col_batch(&[0])).unwrap();
        assert_eq!(synced(), 1, "creating the WAL syncs its directory");
        wal_append(dir.path(), "second", None, &one_col_batch(&[1_000])).unwrap();
        assert_eq!(synced(), 1, "appending to an existing WAL needs no directory sync");
    }

    #[test]
    fn a_failed_rename_leaves_every_unapplied_record_in_the_wal() {
        // A vanished source is the one rename failure an unprivileged test can cause where `rename`
        // replaces its destination: a sweeper deletes every `*.tmp` as soon as it appears, so the
        // rewrite's real `rename` meets ENOENT. The kept rows must be in `_wal.arrow` whatever the
        // rewrite answered, because no other name is ever replayed.
        let dir = tempfile::tempdir().unwrap();
        // Big enough that writing it keeps the temp file visible far longer than one sweep takes.
        let rows = 500_000usize;
        let ts: Vec<i64> = (0..rows as i64).collect();
        wal_append(dir.path(), "unapplied", None, &one_col_batch(&ts)).unwrap();

        let sweeping = AtomicBool::new(true);
        let (mut failed, mut kept) = (false, rows);
        let deadline = Instant::now() + Duration::from_secs(60);
        std::thread::scope(|s| {
            s.spawn(|| {
                while sweeping.load(Ordering::Relaxed) {
                    for e in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
                        if e.file_name().to_string_lossy().ends_with(".tmp") {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            });
            // Nothing in here may panic: the scope would wait forever on the spinning sweeper.
            while !failed && kept == rows && Instant::now() < deadline {
                failed = wal_rewrite_keeping_unapplied(dir.path(), &Manifest::empty()).is_err();
                kept = wal_read(dir.path())
                    .map_or(0, |records| records.iter().map(|(_, _, b)| b.num_rows()).sum());
            }
            sweeping.store(false, Ordering::Relaxed);
        });
        assert_eq!(kept, rows, "a failed rename must not cost a kept row");
        assert!(failed, "no rename failed within the deadline, so this run proved nothing");
    }

    #[test]
    fn the_current_reader_reads_every_frame_shape_any_release_wrote() {
        // One file holding each shape ever written, interleaved: v1 (every release through v0.1.35,
        // and a plain record today), v2 with `has_supersede = 0` (v0.1.36 and v0.1.37's plain
        // record), and v2 with a supersede key (v0.1.36 onwards).
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = encode_v1_frame("v0.1.35", &one_col_batch(&[0]));
        bytes.extend(v0_1_36_frame("v0.1.36-plain", None, &one_col_batch(&[1_000])));
        bytes.extend(v0_1_36_frame("v0.1.36-superseding", Some("p1"), &one_col_batch(&[2_000])));
        bytes.extend(encode_wal_frame("now-plain", None, &one_col_batch(&[3_000])).unwrap());
        bytes.extend(
            encode_wal_frame("now-superseding", Some("p2"), &one_col_batch(&[4_000])).unwrap(),
        );
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();

        let records = wal_read(dir.path()).unwrap();
        let got: Vec<(&str, Option<&str>, usize)> =
            records.iter().map(|(k, sk, b)| (k.as_str(), sk.as_deref(), b.num_rows())).collect();
        assert_eq!(
            got,
            [
                ("v0.1.35", None, 1),
                ("v0.1.36-plain", None, 1),
                ("v0.1.36-superseding", Some("p1"), 1),
                ("now-plain", None, 1),
                ("now-superseding", Some("p2"), 1),
            ],
            "every shape is read, in append order, with its supersede key intact"
        );
    }

    #[test]
    fn a_length_past_the_end_of_the_file_is_a_torn_tail_however_large() {
        // `p + ipc_len` with `ipc_len = u64::MAX` used to wrap (or overflow in a debug build) and
        // the slice after it panicked inside `open`.
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = encode_wal_frame("intact", None, &one_col_batch(&[0])).unwrap();
        for (shape, length) in [(b"VWAL", u64::MAX), (b"VWAL", u64::MAX - 7), (b"VWL2", 1 << 40)] {
            let mut torn = bytes.clone();
            torn.extend_from_slice(shape);
            torn.extend_from_slice(&3u32.to_le_bytes());
            torn.extend_from_slice(b"big");
            if shape == b"VWL2" {
                torn.push(0u8);
            }
            torn.extend_from_slice(&length.to_le_bytes());
            torn.extend_from_slice(&[0u8; 16]);
            std::fs::write(dir.path().join(WAL), &torn).unwrap();
            let read = wal_read_to_undecodable(dir.path()).unwrap();
            let keys: Vec<&str> = read.records.iter().map(|(k, _, _)| k.as_str()).collect();
            assert_eq!(keys, ["intact"], "length {length}: only the intact record is read");
            assert!(read.undecodable.is_none(), "length {length}: a torn tail, not a bad record");
        }
        // A hostile supersede-key length is the same torn tail.
        bytes.extend_from_slice(b"VWL2");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"big");
        bytes.push(1u8);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();
        assert_eq!(wal_read(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn a_whole_frame_whose_rows_do_not_decode_is_reported_with_the_records_before_it() {
        // A power cut that kept a frame's header and length but not its payload pages: the rows
        // read back as zeros. The reader says where it stopped and why.
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = encode_wal_frame("first", None, &one_col_batch(&[0])).unwrap();
        let bad_at = bytes.len();
        bytes.extend_from_slice(b"VWAL");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"bad");
        bytes.extend_from_slice(&64u64.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 64]);
        bytes.extend(encode_wal_frame("after", None, &one_col_batch(&[1_000])).unwrap());
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();

        let read = wal_read_to_undecodable(dir.path()).unwrap();
        let keys: Vec<&str> = read.records.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, ["first"], "the records in front of the bad one, and none after it");
        assert_matches!(read.undecodable, Some((at, HistoryStoreError::Query(_))) if at == bad_at);
        assert_matches!(wal_read(dir.path()), Err(HistoryStoreError::Query(_)));
    }

    #[test]
    fn a_record_whose_rows_carry_no_ts_is_a_bad_record_where_it_stands() {
        // The replay re-seals a record by its `ts`; one without it used to fail `open` for the
        // whole store at that re-seal.
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = encode_wal_frame("first", None, &one_col_batch(&[0])).unwrap();
        let bad_at = bytes.len();
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let col: ArrayRef = Arc::new(Int64Array::from(vec![7]));
        let no_ts = RecordBatch::try_new(schema, vec![col]).unwrap();
        bytes.extend(encode_wal_frame("no-ts", None, &no_ts).unwrap());
        bytes.extend(encode_wal_frame("after", None, &one_col_batch(&[1_000])).unwrap());
        std::fs::write(dir.path().join(WAL), &bytes).unwrap();

        let read = wal_read_to_undecodable(dir.path()).unwrap();
        let keys: Vec<&str> = read.records.iter().map(|(k, _, _)| k.as_str()).collect();
        assert_eq!(keys, ["first"], "the records in front of it, and none after it");
        assert_matches!(read.undecodable, Some((at, HistoryStoreError::Query(_))) if at == bad_at);
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
