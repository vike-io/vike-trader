//! The per-series manifest: file index + ingest commit-log, and the sealing of new parts into it.
//!
//! The manifest (JSON, NOT a database) is the file index a series' reads consult to pick which
//! Parquet parts overlap a query range — no directory LIST (the spec's must-fix #5). It also
//! carries the commit-log of ingested batch keys so appends are idempotent by COMMIT KEY, never by
//! row value (must-fix #1: trades carry no id). [`SeriesLock`] serializes the read-modify-write
//! across `commit_rows` / `compact_series` / `apply_retention` so a concurrent compaction + append
//! can't lost-update.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use datafusion::arrow::array::ArrayRef;
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

use crate::hist::{DataError, TsRange};
use crate::hist_maint::{Durability, WriteOpts};
use vike_model::time::epoch_ms_to_utc_date;

use super::{COMMIT_KEYS_META, fsync_dir, io, part_dir, q, write_parquet};

const MANIFEST: &str = "_manifest.json";
const MANIFEST_LOCK: &str = "_manifest.lock";
/// On-disk manifest format. v2 added `FileEntry.date` (the `date=` partition key) + `commit_key`
/// lineage + this `format` tag. A v1 (slice-3) manifest fails the format check on read — the data
/// tree is gitignored + regenerable, so this is a hard break, not a silent misparse.
const MANIFEST_FORMAT: u32 = 2;

/// One sealed Parquet part, as the manifest records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct FileEntry {
    pub(super) name: String,
    /// `YYYY-MM-DD` — the `date=` sub-partition this part lives under (UTC day of its rows).
    pub(super) date: String,
    pub(super) ts_min: i64,
    pub(super) ts_max: i64,
    pub(super) rows: usize,
    /// Batch keys whose rows are in this part (retention GCs the commit-log by them). A hot append
    /// carries its single writing key (empty for keyless appends); a compacted part carries the
    /// union of its inputs' keys, so retention AFTER compaction still GCs correctly.
    pub(super) commit_keys: Vec<String>,
}

impl FileEntry {
    /// Does this part's `[ts_min, ts_max]` overlap the query range? (coarse file-level prune)
    pub(super) fn overlaps(&self, r: TsRange) -> bool {
        if let Some(s) = r.start
            && self.ts_max < s
        {
            return false;
        }
        if let Some(e) = r.end
            && self.ts_min > e
        {
            return false;
        }
        true
    }
}

/// A per-series manifest: the file index + the ingest commit-log. Bumped + atomically republished
/// on every append/compaction/retention; the reader's source of truth for which files to open.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    /// On-disk format tag (see [`MANIFEST_FORMAT`]); refuse-to-read on mismatch.
    #[serde(default)]
    pub(super) format: u32,
    pub(super) version: u64,
    pub(super) files: Vec<FileEntry>,
    /// batch keys already ingested (idempotency; the spec's must-fix #1 lives here)
    pub(super) commits: Vec<String>,
}

impl Manifest {
    /// A fresh (empty) manifest stamped with the current format — for a series with no parts yet.
    pub(super) fn empty() -> Self {
        Manifest { format: MANIFEST_FORMAT, version: 0, files: Vec::new(), commits: Vec::new() }
    }
}

/// Reconstruct a series' manifest from the PARTS THEMSELVES, ignoring any existing `_manifest.json`.
///
/// This is the property that demotes the manifest from ground truth to a rebuildable CACHE — the
/// posture every comparable system takes and this store did not. NautilusTrader ships
/// `reset_all_file_names()`, which re-derives its filename index from Parquet row-group statistics;
/// ArcticDB treats its version-ref key as a cache with an explicit "FALLBACK TO ITERATION: we also
/// have an alternative method to fetch all version keys which is to fall back to iterating the
/// storage… useful in case we have consistency issues in the ref keys". Before this, a lost or
/// corrupt `_manifest.json` meant the parts beneath it were unreachable: the read path never LISTs
/// directories (deliberately — spec must-fix #5), so data that exists on disk was simply invisible.
///
/// Every field is recovered from the file itself:
///   - `name` from the path, `date` from the enclosing `date=` directory
///   - `rows` from the Parquet footer's row count
///   - `ts_min`/`ts_max` from the `ts` column's row-group statistics
///   - `commit_keys` from the footer's [`COMMIT_KEYS_META`] key — the reason that key exists
///
/// ⚠ **Parts written before `COMMIT_KEYS_META` existed carry no keys**, so a rebuild over an older
/// store recovers the file index (reads work again) but not the idempotency log — re-running a
/// backfill against it would re-admit already-applied appends and DUPLICATE rows. The rebuild
/// reports that case rather than hiding it: [`RebuildReport::parts_without_keys`] counts them, and
/// the caller decides. It is not an error, because "I can read my data again" is worth having even
/// when the commit log is gone.
///
/// # A crashed compaction leaves a merge's INPUTS and its OUTPUT in the same directory
///
/// Reading a directory means reading whatever a crash left in it, and compaction's two crash windows
/// both leave the merged rows on disk TWICE — once as the fragments, once inside the sealed part
/// that replaces them. Indexing both is not a partial recovery, it is a SILENT DOUBLING: measured by
/// SIGKILLing a store mid-compaction, 34–68% row inflation in 7 of 10 kill trials. Two rules answer
/// it, and both make the input/output distinction EXPLICIT rather than inferring it from the
/// filesystem:
///
/// 1. **[`MERGE_TMP_PREFIX`]** — a merge output carries that prefix until the publish renames it
///    under the series lock, so "written but never published" is a fact about the NAME, not a guess.
///    Skipped and counted ([`RebuildReport::parts_unpublished_merge`]). This is the window that
///    matters: the merge runs UNLOCKED (holding the lock across it starves the live recorder), so it
///    is nearly all of compaction's wall clock, and it is also the window in which a rebuild may run
///    CONCURRENTLY with a live merge — which is why the answer cannot be "delete what the manifest
///    does not list".
/// 2. **Commit-key containment** — after the publish, the output sits at its final name and only its
///    CONTENT distinguishes it. A part's `commit_keys` are exactly the batches whose rows it holds,
///    and a merge output carries the UNION of its inputs' keys, so a part whose key set is a strict
///    subset of another part's in the same `date=` is contained in that part and is skipped
///    ([`RebuildReport::parts_superseded`]). Exact, not heuristic — and immune to part-name reuse,
///    which is why the rule keys on content and never on names (`next_part_name` deliberately
///    recycles `part-NNNNN` indices after a compaction).
///
/// ⚠ Two residuals, both narrower than the bug they replace and neither silent-by-design:
/// a KEYLESS part (no `commit_key` at the append, or a part predating `COMMIT_KEYS_META`) carries no
/// content identity, so rule 2 cannot see it — [`RebuildReport::parts_without_keys`] is where that
/// shows up; and two compaction passes racing on ONE date can leave two outputs whose key sets
/// overlap without either containing the other, which rule 2 keeps both of.
///
/// Does NOT write anything — the caller publishes the result (or compares it, as the round-trip
/// test does). Reads footers only, never row data.
pub(super) fn rebuild_manifest(dir: &Path) -> Result<(Manifest, RebuildReport), DataError> {
    let mut m = Manifest::empty();
    let mut report = RebuildReport::default();

    // Series leaves hold `date=` dirs and nothing else; a missing dir is an empty series, not an
    // error (same posture as `read_manifest`'s NotFound arm).
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok((m, report)),
        Err(e) => return Err(io(e)),
    };
    let mut dates: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries {
        let p = entry.map_err(io)?.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if let Some(date) = name.strip_prefix("date=")
            && p.is_dir()
        {
            dates.push((date.to_string(), p));
        }
    }
    // Deterministic order: date, then part name within it — so a rebuild is reproducible and can be
    // compared field-for-field against the manifest it replaces.
    dates.sort();

    for (date, date_dir) in dates {
        let mut parts: Vec<PathBuf> = std::fs::read_dir(&date_dir)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("parquet"))
            .collect();
        parts.sort();
        // Read every candidate's footer FIRST: rule 2 above compares each part against the others in
        // its own `date=`, so the whole date has to be in hand before anything is admitted.
        let mut cands: Vec<(String, PartMeta)> = Vec::new();
        for part in parts {
            let Some(name) = part.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            // An unpublished merge output: its rows are still in the inputs it was built from, which
            // are also right here. Never indexed — see rule 1 above.
            if name.starts_with(MERGE_TMP_PREFIX) {
                report.parts_unpublished_merge += 1;
                continue;
            }
            match part_metadata(&part)? {
                Some(meta) => cands.push((name, meta)),
                // A part with no `ts` statistics cannot be range-pruned, so admitting it would let
                // the read path skip a file that actually overlaps the query. Skipped and COUNTED,
                // never silently included with a fabricated range.
                None => report.parts_unreadable += 1,
            }
        }
        for i in 0..cands.len() {
            if let Some(by) = superseding_part(&cands, i) {
                tracing::info!(
                    part = %cands[i].0,
                    superseded_by = %by,
                    date = %date,
                    "manifest rebuild: this part's rows are already inside another part in the \
                     same date (a compaction that crashed before unlinking its inputs) — skipping \
                     it rather than counting its rows twice"
                );
                report.parts_superseded += 1;
                continue;
            }
            let (name, meta) = &cands[i];
            if meta.commit_keys.is_empty() {
                report.parts_without_keys += 1;
            }
            for k in &meta.commit_keys {
                if !m.commits.iter().any(|c| c == k) {
                    m.commits.push(k.clone());
                }
            }
            m.files.push(FileEntry {
                name: name.clone(),
                date: date.clone(),
                ts_min: meta.ts_min,
                ts_max: meta.ts_max,
                rows: meta.rows,
                commit_keys: meta.commit_keys.clone(),
            });
            report.parts_recovered += 1;
        }
    }
    m.version = 1;
    Ok((m, report))
}

/// Filename prefix of a compaction output that has been MERGED but not yet PUBLISHED.
///
/// This is the whole "which of these two files is the merge's output" question, answered by making
/// the answer explicit instead of inferring it. The merge runs with the series lock RELEASED (see
/// `DataFusionHist::compact_dir_inner` — holding it across a merge starves the live recorder), so
/// during it the output and every one of its inputs are in the same `date=` directory, holding the
/// same rows. Under this prefix that state is unambiguous to anyone who reads the directory: a
/// prefixed file is provably not part of the series, whether it was left by a crash or is being
/// written RIGHT NOW by a live compaction in another process. Compaction renames it to its final
/// `part-c…` name inside the publish, under the lock, immediately before the manifest naming it is
/// published.
///
/// ⚠ **The rename direction is load-bearing and cannot be inverted into a sweep.** The tempting fix
/// for a crashed compaction — delete files the manifest does not list — deletes exactly this file
/// out from under a live merge. Nothing here ever deletes a part it did not create.
///
/// A prefixed file left by a crash is INERT: never read (the read path only opens what the manifest
/// names), never indexed by [`rebuild_manifest`], and invisible to compaction planning (which reads
/// the manifest). The next pass over the same manifest version writes the same name and truncates it
/// in place, so the common case self-heals; one that does not is a stale file and nothing else.
/// Deleting it is deliberately NOT automated, for the reason above.
pub(super) const MERGE_TMP_PREFIX: &str = "_tmp-merge-";

/// The unpublished name a compaction writes its merge output under. See [`MERGE_TMP_PREFIX`].
pub(super) fn merge_tmp_name(out_name: &str) -> String {
    format!("{MERGE_TMP_PREFIX}{out_name}")
}

/// Is `cands[i]`'s content already inside another part of the same `date=`? Returns that part's
/// name, for the log line that says so.
///
/// The relation is COMMIT-KEY CONTAINMENT: a part's `commit_keys` are exactly the batches whose rows
/// it holds — one key for an append, the UNION of its inputs' keys for a compaction output — so
/// `mine ⊊ theirs` means every row of `mine` is also in `theirs`. That is an implication about
/// content, not a guess about filenames, which matters because part names ARE recycled
/// ([`next_part_name`] restarts at `part-00001` once a date holds only compaction output), so a rule
/// that remembered "this merge consumed part-00001" would eventually skip a live append.
///
/// EQUAL non-empty key sets mean the same batches, i.e. the same rows in two files; exactly one is
/// kept, the first in the directory's sorted order, so the choice is deterministic and reproducible.
/// A part with NO keys is never superseded and never supersedes: the empty set is a subset of
/// everything, and treating it as contained would drop every keyless part in a date that also holds
/// a keyed one.
fn superseding_part(cands: &[(String, PartMeta)], i: usize) -> Option<&str> {
    let mine: BTreeSet<&str> = cands[i].1.commit_keys.iter().map(String::as_str).collect();
    if mine.is_empty() {
        return None;
    }
    for (j, (name, meta)) in cands.iter().enumerate() {
        if i == j {
            continue;
        }
        let theirs: BTreeSet<&str> = meta.commit_keys.iter().map(String::as_str).collect();
        if !mine.is_subset(&theirs) {
            continue;
        }
        // Strict superset — `theirs` holds batches `mine` does not, so it is the merge output and
        // `mine` one of its inputs. Equal — the same batches twice; keep the earlier name.
        if theirs.len() > mine.len() || j < i {
            return Some(name);
        }
    }
    None
}

/// What a [`rebuild_manifest`] pass recovered — and, as importantly, what it could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub parts_recovered: usize,
    /// Parts whose footer carried no [`COMMIT_KEYS_META`] — written before that key existed. Their
    /// rows are readable again, but they contribute nothing to the idempotency log. ⚠ They are also
    /// the one shape the containment rule behind [`RebuildReport::parts_superseded`] cannot see, so
    /// a non-zero count here is also the measure of how much of a crashed compaction this pass had
    /// to take on trust.
    pub parts_without_keys: usize,
    /// Parts skipped: unreadable footer, or no `ts` statistics to derive a range from.
    pub parts_unreadable: usize,
    /// Parts skipped because another part in the same `date=` provably holds their rows — the
    /// fragments of a compaction that published its output and crashed before unlinking them.
    /// Counting these would have doubled every merged row.
    pub parts_superseded: usize,
    /// Merge outputs skipped for not being published yet ([`MERGE_TMP_PREFIX`]). Either a compaction
    /// died before its publish, or one is running in another process right now — indistinguishable
    /// here, and identical in consequence: the rows are still in the inputs, which ARE indexed.
    pub parts_unpublished_merge: usize,
}

/// Everything a [`FileEntry`] needs that lives inside the part itself.
struct PartMeta {
    rows: usize,
    ts_min: i64,
    ts_max: i64,
    commit_keys: Vec<String>,
}

/// Read a part's [`PartMeta`] from its FOOTER only — never row data.
/// `None` when the footer is unreadable or carries no `ts` statistics to bound the part by.
fn part_metadata(path: &Path) -> Result<Option<PartMeta>, DataError> {
    let Ok(file) = std::fs::File::open(path) else { return Ok(None) };
    let Ok(builder) = ParquetRecordBatchReaderBuilder::try_new(file) else { return Ok(None) };
    let md = builder.metadata();
    let ts_idx = match md.file_metadata().schema_descr().columns().iter().position(|c| {
        // The leaf's own name; every codec in this store names its time column `ts`.
        c.name() == "ts"
    }) {
        Some(i) => i,
        None => return Ok(None),
    };
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    let mut rows = 0usize;
    for rg in md.row_groups() {
        rows += rg.num_rows() as usize;
        let Some(stats) = rg.column(ts_idx).statistics() else { return Ok(None) };
        let (Some(a), Some(b)) = (stats.min_bytes_opt(), stats.max_bytes_opt()) else {
            return Ok(None);
        };
        let (Ok(a), Ok(b)) = (<[u8; 8]>::try_from(a), <[u8; 8]>::try_from(b)) else {
            return Ok(None);
        };
        lo = lo.min(i64::from_le_bytes(a));
        hi = hi.max(i64::from_le_bytes(b));
    }
    if rows == 0 || lo > hi {
        return Ok(None);
    }
    let keys = md
        .file_metadata()
        .key_value_metadata()
        .and_then(|kv| kv.iter().find(|e| e.key == COMMIT_KEYS_META))
        .and_then(|e| e.value.clone())
        .map(|v| v.lines().map(str::to_string).collect::<Vec<String>>())
        .unwrap_or_default();
    Ok(Some(PartMeta { rows, ts_min: lo, ts_max: hi, commit_keys: keys }))
}

pub(super) fn read_manifest(dir: &Path) -> Result<Manifest, DataError> {
    match std::fs::read(dir.join(MANIFEST)) {
        Ok(bytes) => {
            let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
                DataError::Query(format!(
                    "manifest parse at {} (incompatible format? this build reads v{MANIFEST_FORMAT}; \
                     the data tree is derived — regenerate): {e}",
                    dir.display()
                ))
            })?;
            if m.format != MANIFEST_FORMAT {
                return Err(DataError::Query(format!(
                    "manifest format v{} != supported v{MANIFEST_FORMAT} at {} (regenerate the store)",
                    m.format,
                    dir.display()
                )));
            }
            Ok(m)
        }
        Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(Manifest::empty()),
        Err(e) => Err(io(e)),
    }
}

/// Publish a manifest by atomic rename (write tmp → replace). `fs::rename` replaces on modern
/// Windows + unix; the remove-then-rename fallback covers any platform that won't.
pub(super) fn write_manifest(
    dir: &Path,
    m: &Manifest,
    durability: Durability,
) -> Result<(), DataError> {
    let bytes = serde_json::to_vec_pretty(m).map_err(q)?;
    let tmp = dir.join("_manifest.json.tmp");
    // fsync the new manifest's bytes BEFORE the rename so the published manifest is durable. This is
    // what lets `commit_rows` safely clear the WAL only AFTER a durable publish — without it a crash
    // could lose the (not-yet-durable) manifest while the WAL was already removed (an ordering
    // inversion that would strand the append).
    //
    // [`Durability::Bulk`] skips it: bulk import keeps no WAL, so there is no such inversion to
    // avoid, and losing the publish is not a loss — the manifest reverts to its previous version and
    // the re-run redoes that commit_key. What bulk must NOT skip is the PART fsync, and it doesn't
    // (see `write_parquet`) — that is what keeps a surviving manifest entry from naming a torn part.
    {
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(&bytes).map_err(io)?;
        if durability == Durability::Fsync {
            f.sync_all().map_err(io)?;
        }
    }
    let final_path = dir.join(MANIFEST);
    if std::fs::rename(&tmp, &final_path).is_err() {
        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&tmp, &final_path).map_err(io)?;
    }
    // On POSIX the rename above is durable only once the containing directory is fsynced — without
    // this the manifest's own bytes survive under a name that does not.
    if durability == Durability::Fsync {
        fsync_dir(dir)?;
    }
    Ok(())
}

/// A per-series write lock. Serializes the manifest read-modify-write across `commit_rows` /
/// `compact_series` / `apply_retention` so a concurrent compaction + append can't lost-update.
///
/// **The lock is the OS advisory lock on `_manifest.lock` — NOT the existence of that file.** That
/// distinction is the whole design: the kernel releases an advisory lock when the holder's
/// descriptor closes, which it does on a clean drop, on a panic, on SIGKILL, on an OOM kill, and on
/// a reboot. A dead writer therefore cannot leave a lock behind, and no reader of the lock has to
/// guess whether an owner is still alive.
///
/// It used to be the file's existence (`create_new`), which has no such property. the CI box,
/// 2026-08-04: the recorder's compaction thread was OOM-killed by its 4 GB cgroup while holding
/// this lock. SIGKILL runs no `Drop`, so the zero-byte `_manifest.lock` survived its owner — and
/// carrying neither an owner id nor a timestamp, it was indistinguishable from a live holder.
/// Every subsequent start timed out in WAL recovery and exited 1; systemd restarted the daemon
/// **2,728 times over ~11 h**, recording nothing, until an operator deleted the file by hand. A
/// second series lost 104,650 rows the same way, silently: its flushes timed out and the sink
/// discards on failure.
///
/// The file is created-if-absent and **never unlinked**. Unlinking would reintroduce the same class
/// of bug in a subtler form: a second process can create a NEW inode at the same path and lock
/// that, so two writers would each hold a lock and each believe it was alone. Leaving one empty
/// file per series is the price of the guarantee. A leftover file from any older build is inert —
/// the first `acquire` simply locks it.
pub(super) struct SeriesLock(File);

/// `2000 × 2 ms` — the ~4 s spin budget a CONTENDED acquire pays before giving up. Named because
/// two things read it: the callers whose retry logic is sized against it
/// (`crates/vike-data/src/live_rec.rs`'s `RecorderSink` documents "the store already spun ~4s"),
/// and the tests, which pass a SMALLER budget so a deliberately-contended case costs milliseconds
/// instead of four seconds.
const SPIN_ATTEMPTS: u32 = 2000;

impl SeriesLock {
    pub(super) fn acquire(series_dir: &Path) -> Result<Self, DataError> {
        Self::acquire_within(series_dir, SPIN_ATTEMPTS).map(|(lock, _attempts)| lock)
    }

    /// [`SeriesLock::acquire`], with the spin budget as a parameter and the number of `try_lock`
    /// ATTEMPTS spent reported back.
    ///
    /// The attempt count is the honest measure of "how contended was this lock", and it is what the
    /// leftover-lock-file test asserts on. That test used to assert a WALL CLOCK (`elapsed() < 1s`)
    /// — and a wall clock here measures the wrong thing entirely. Only the LAST of this function's
    /// syscalls is the lock: `create_dir_all` issues `mkdirat` (which takes the PARENT directory's
    /// inode lock exclusively before it can even discover the directory already exists) plus a
    /// `stat`, then `open` walks the same parent — and in a test that parent is the system temp
    /// dir every other process on the box is also churning. Measured on the CI box (btrfs `/tmp`): in
    /// ONE 1,024-run soak of this exact uncontended acquire, p50 was 42 µs and max was 301 ms — a
    /// 7,000x spread within a single shape, living entirely in `create_dir_all`, while `attempts`
    /// stayed 1 in all 1,536 runs measured. The lock behaviour was never what varied.
    fn acquire_within(series_dir: &Path, spins: u32) -> Result<(Self, u32), DataError> {
        std::fs::create_dir_all(series_dir).map_err(io)?;
        let path = series_dir.join(MANIFEST_LOCK);
        // `create(true).truncate(false)`: open-or-create, and never disturb a file another process
        // may already hold — the bytes are irrelevant, the lock lives in the kernel.
        let file =
            OpenOptions::new().write(true).create(true).truncate(false).open(&path).map_err(io)?;
        for attempt in 1..=spins {
            match file.try_lock() {
                Ok(()) => return Ok((SeriesLock(file), attempt)),
                // Another writer holds it — in this process or any other. Spin: the background
                // MaintenanceScheduler churns this lock far harder than one-off manual compaction
                // did, and every holder's critical section is a manifest read-modify-write.
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        }
        Err(DataError::Io(format!(
            "timeout acquiring series lock at {} after {spins} attempts",
            path.display()
        )))
    }
}

impl Drop for SeriesLock {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock on its own; unlocking first makes the
        // release explicit and ordered. The FILE stays on disk — see the type doc.
        let _ = self.0.unlock();
    }
}

fn minmax_ts(iter: impl Iterator<Item = i64>) -> (i64, i64) {
    let mut lo = i64::MAX;
    let mut hi = i64::MIN;
    for t in iter {
        lo = lo.min(t);
        hi = hi.max(t);
    }
    (lo, hi)
}

/// Seal one parquet part per UTC `date=` for `ts`/`build_cols`, recording each in `m` IN MEMORY (the
/// caller publishes `m`). Shared by the live [`super::DataFusionHist::commit_rows`] and WAL
/// [`super::DataFusionHist::recover_series`] so both go through the identical date-split + seal path.
/// Assumes the caller holds the series lock and has already run the idempotency + empty-batch
/// guards. Returns rows sealed.
/// The next `part-NNNNN.parquet` name for `date` — **one above the HIGHEST index still listed**,
/// never `count + 1`.
///
/// The invariant: a fresh name must not collide with a part that is still LIVE in the manifest.
/// A count-derived name breaks it, and silently corrupts the series when it does. Compaction
/// removes its inputs from the middle of the name space (its own output is `part-c…`, outside it)
/// while parts that landed during its unlocked merge stay live with HIGHER indices — e.g. the
/// manifest below, taken verbatim from the state that made
/// `concurrent_append_and_compact_no_lost_update` fail:
///
/// ```text
/// [part-00005, part-00006, part-00007, part-00008, part-c00000009]   count = 5  ->  part-00006
/// ```
///
/// `part-00006` is LIVE. Writing it does two things, both silent: [`write_parquet`] opens with
/// `File::create`, so that part's rows are TRUNCATED AWAY, and the caller pushes a SECOND
/// `FileEntry` with the same `(name, date)`, so the survivor is read TWICE by `collect_for_symbol`
/// (which builds one read URL per manifest entry). Rows lost and rows duplicated in equal number —
/// which is why the row-count assert passed while the ts-uniqueness assert failed.
///
/// Compaction outputs are skipped by construction: `part-c00000009` does not parse as an index, so
/// it never raises the max — and it cannot collide, being in a different name space.
///
/// Orphan recovery is PRESERVED. A part left by a crash is absent from `m.files`, so it does not
/// raise the max either and its name is reused and overwritten in place, exactly as before — the
/// property the old count-derived name was chosen for.
pub(super) fn next_part_name(files: &[FileEntry], date: &str) -> String {
    let max = files
        .iter()
        .filter(|f| f.date == date)
        .filter_map(|f| {
            f.name
                .strip_prefix("part-")
                .and_then(|s| s.strip_suffix(".parquet"))?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    format!("part-{:05}.parquet", max + 1)
}

pub(super) fn seal_into_manifest<F>(
    series_dir: &Path,
    m: &mut Manifest,
    commit_key: Option<&str>,
    ts: &[i64],
    schema: &Arc<Schema>,
    build_cols: F,
    opts: WriteOpts,
) -> Result<usize, DataError>
where
    F: Fn(&[usize]) -> Vec<ArrayRef>,
{
    // group row indices by UTC day; BTreeMap = deterministic (sorted) iteration, no dep
    let mut by_date: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, &t) in ts.iter().enumerate() {
        by_date.entry(epoch_ms_to_utc_date(t)).or_default().push(i);
    }
    let mut written = 0usize;
    for (date, idxs) in &by_date {
        let name = next_part_name(&m.files, date);
        let dir = part_dir(series_dir, date);
        std::fs::create_dir_all(&dir).map_err(io)?;
        let batch = RecordBatch::try_new(schema.clone(), build_cols(idxs)).map_err(q)?;
        // The SAME keys recorded in the `FileEntry` below also go into the part's own footer, so
        // the part is self-describing and the manifest is rebuildable from the data alone.
        let keys: Vec<String> = commit_key.map(|k| k.to_string()).into_iter().collect();
        write_parquet(&dir.join(&name), schema.clone(), vec![batch], opts.profile, &keys)?;
        // The part's BYTES are durable (fsynced inside `write_parquet`); this makes its NAME durable
        // too. Skipped under `Bulk` for the same reason the manifest fsync is: without a durable
        // manifest entry there is nothing that could outlive the missing directory entry.
        if opts.durability == Durability::Fsync {
            fsync_dir(&dir)?;
        }
        let (lo, hi) = minmax_ts(idxs.iter().map(|&i| ts[i]));
        m.files.push(FileEntry {
            name,
            date: date.clone(),
            ts_min: lo,
            ts_max: hi,
            rows: idxs.len(),
            commit_keys: commit_key.map(|k| k.to_string()).into_iter().collect(),
        });
        written += idxs.len();
    }
    if let Some(k) = commit_key {
        m.commits.push(k.to_string());
    }
    m.version += 1;
    Ok(written)
}

#[cfg(test)]
mod part_name_tests {
    use super::*;

    fn fe(name: &str, date: &str) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            date: date.to_string(),
            ts_min: 0,
            ts_max: 0,
            rows: 5,
            commit_keys: Vec::new(),
        }
    }

    /// THE regression: the manifest state a compaction leaves behind when appends landed during its
    /// unlocked merge — its `part-c…` output plus live parts whose indices are ABOVE the file count.
    /// A `count + 1` name lands on `part-00006`, which is LIVE: the append truncates that file (rows
    /// LOST) and adds a duplicate manifest entry (rows read TWICE). Equal loss and duplication is
    /// why `concurrent_append_and_compact_no_lost_update` kept its 40-row count while failing
    /// `ts unique + ascending`.
    #[test]
    fn next_part_name_never_collides_with_a_live_part() {
        let files = vec![
            fe("part-00005.parquet", "1970-01-01"),
            fe("part-00006.parquet", "1970-01-01"),
            fe("part-00007.parquet", "1970-01-01"),
            fe("part-00008.parquet", "1970-01-01"),
            fe("part-c00000009.parquet", "1970-01-01"),
        ];
        let name = next_part_name(&files, "1970-01-01");
        assert!(
            !files.iter().any(|f| f.name == name && f.date == "1970-01-01"),
            "chose a LIVE part name: {name}"
        );
        assert_eq!(name, "part-00009.parquet", "one above the highest live index");
    }

    /// Names are per-`date=` dir, so a busy neighbouring date must not push this date's index up.
    #[test]
    fn next_part_name_is_scoped_to_its_date() {
        let files = vec![
            fe("part-00001.parquet", "1970-01-01"),
            fe("part-00002.parquet", "1970-01-01"),
            fe("part-00003.parquet", "1970-01-02"),
        ];
        assert_eq!(next_part_name(&files, "1970-01-02"), "part-00004.parquet");
        assert_eq!(
            next_part_name(&files, "1970-01-03"),
            "part-00001.parquet",
            "unseen date starts at 1"
        );
    }

    /// A compaction output must never raise the index (it is a separate name space), and a series
    /// holding ONLY compacted parts restarts appends at 1 — no live `part-NNNNN` to collide with.
    #[test]
    fn compaction_outputs_do_not_raise_the_index() {
        let files = vec![fe("part-c00000042.parquet", "1970-01-01")];
        assert_eq!(next_part_name(&files, "1970-01-01"), "part-00001.parquet");
    }
}

#[cfg(test)]
mod series_lock_tests {
    use super::*;

    /// The property that makes this a LOCK: while one holder has it, nobody else gets it. `flock` is
    /// per open-file-description, so two `acquire`s inside one process contend exactly as two
    /// processes do — which is what lets the property be tested without spawning one.
    #[test]
    fn a_second_acquire_is_excluded_until_the_holder_drops() {
        let dir = tempfile::tempdir().unwrap();
        let held = SeriesLock::acquire(dir.path()).expect("first acquire");

        // `let Err(..) else` rather than `expect_err`: that would need `SeriesLock: Debug`, and a
        // lock guard has no business growing a public trait impl to satisfy a test.
        let Err(err) = SeriesLock::acquire(dir.path()) else {
            panic!("two holders had the same series lock at once");
        };
        assert!(
            format!("{err:?}").contains("timeout acquiring series lock"),
            "wrong failure while contended: {err:?}"
        );

        drop(held);
        SeriesLock::acquire(dir.path()).expect("lock must be free once its holder drops");
    }

    /// A lock file with no live holder — what a SIGKILL'd writer leaves — must be takeable at once.
    /// Under the old existence-is-the-lock scheme this spun 4 s and then failed, forever.
    ///
    /// "At once" is asserted as **one `try_lock` attempt**, not as a wall clock. The clock this
    /// test used to read (`elapsed() < 1 s`) spans `create_dir_all` + `open` as well, and those are
    /// filesystem metadata syscalls on the shared system temp dir — bounded by the box, not by the
    /// lock. It failed 2 of 248 runs in #1125's soak for exactly that reason. Reproduced on the CI box
    /// under CPU starvation, the old assertion failed with `spun for 36.9s` and `spun for 76.2s`
    /// — and the phase timing of those very runs puts 36.900997766 s of the 36.9012802 s inside
    /// `create_dir_all`, with ONE `try_lock` attempt. It never spun at all; its own failure message
    /// was wrong about what it had measured. Attempts are what the #1024 fix changed, so attempts
    /// are what this asserts — and it is the STRICTER bound: a 999 ms spin of ~500 attempts passed
    /// the old assertion and fails this one.
    #[test]
    fn a_leftover_lock_file_with_no_holder_is_taken_immediately() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_LOCK), b"").unwrap();

        let (_held, attempts) = SeriesLock::acquire_within(dir.path(), SPIN_ATTEMPTS)
            .expect("a dead writer's file must not wedge us");
        assert_eq!(attempts, 1, "a holderless lock file cost {attempts} attempts, not 1");
    }

    /// The negative control for the test above: `attempts == 1` only carries information because a
    /// loop that genuinely iterates can report more than 1. An `acquire_within` that returned on
    /// its first pass whatever the lock's state, or one whose spin never slept, would satisfy the
    /// leftover-file test and be worthless. Here the holder never lets go, so the acquire must
    /// spend its WHOLE budget, and both halves of that are asserted: the error names the budget,
    /// and the call takes at least the sleeping that many attempts implies.
    ///
    /// The elapsed assertion is a LOWER bound, deliberately — that is the difference between this
    /// and the bound it replaces. Load can only push a lower bound further into the passing side,
    /// where an upper bound is exactly what a loaded box breaks. Budget 8 rather than
    /// [`SPIN_ATTEMPTS`] keeps the control at ~16 ms instead of four seconds; a cheap control is
    /// one that keeps getting run.
    #[test]
    fn a_lock_with_a_live_holder_spends_every_attempt_in_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let _held = SeriesLock::acquire(dir.path()).expect("first acquire");

        let t0 = std::time::Instant::now();
        let Err(err) = SeriesLock::acquire_within(dir.path(), 8) else {
            panic!("a live holder must not yield the series lock");
        };
        let spent = t0.elapsed();
        assert!(
            format!("{err:?}").contains("after 8 attempts"),
            "a contended acquire must report the budget it spent: {err:?}"
        );
        // 8 attempts sleep 2 ms each; `thread::sleep` sleeps AT LEAST that long, so 7 completed
        // sleeps is a floor no scheduler can undercut.
        assert!(spent >= Duration::from_millis(14), "8 spin attempts cannot take only {spent:?}");
    }
}
