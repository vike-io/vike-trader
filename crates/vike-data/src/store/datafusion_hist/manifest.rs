//! The per-series manifest: file index + ingest commit-log, and the sealing of new parts into it.
//!
//! The manifest (JSON, NOT a database) is the file index a series' reads consult to pick which
//! Parquet parts overlap a query range — no directory LIST (the spec's must-fix #5). It also
//! carries the commit-log of ingested batch keys so appends are idempotent by COMMIT KEY, never by
//! row value (must-fix #1: trades carry no id). [`SeriesLock`] serializes the read-modify-write
//! across `commit_rows` / `compact_series` / `apply_retention` so a concurrent compaction + append
//! can't lost-update.
//!
//! # ⚠ A manifest is TWO files now: a BASE and a DELTA LOG
//!
//! `_manifest.json` is the base and [`write_manifest`] still publishes it whole, by atomic rename.
//! `_manifest.delta` is an append-only log of framed [`super::delta::DeltaFrame`]s, one per
//! publish. [`read_manifest`] is base + replay; [`publish`] is the write side and appends a frame
//! instead of rewriting the base; [`fold_base`] periodically collapses the log back into a new
//! base.
//!
//! **Why.** MEASURED on the live data box, 2026-09-16
//! (`docs/decisions/0060-the-manifest-rewrite-is-the-write-amplification.md`): the recorder wrote
//! **2.72 TB/day** of device writes to persist **~0.74 GB/day** of tape, and **96.5%** of that was
//! this file being rewritten whole. The largest series' manifest is 104 MB and every nine-second
//! flush rewrote all of it. Nothing about a JSON file index requires that — it was a consequence of
//! `Manifest` being one `serde` value with one `Serialize` call. What IS inherent, and what the
//! delta log reproduces from framing rather than from `rename`, is the ATOMICITY: a reader must
//! never see a manifest naming a part that is not there, nor miss one that is. `delta.rs`'s module
//! doc carries that argument and the read-ordering rule it depends on.
//!
//! # ⚠ The commit log is DERIVED, not stored
//!
//! v2 carried a top-level `commits` array AND the same keys again inside each
//! [`FileEntry::commit_keys`]. MEASURED on that same box: 659,697 entries against 659,684 distinct
//! keys on files — the log was held twice, and the duplication, not the index, was the size
//! (99.0% of a 104 MB file). So v3 stores the keys only where they belong, on the part that holds
//! their rows, and [`Manifest::has_commit`] answers from there. Two consequences worth knowing
//! before you edit anything here:
//!
//! - **A key's lifetime is now exactly its rows' lifetime**, which is what the idempotency guard
//!   always wanted. `apply_retention_at`'s commit-key GC — an `O(total keys)` scan per dropped key,
//!   `~10^11` string comparisons on the live box, inside [`SeriesLock`] — is deleted rather than
//!   optimised: dropping the file drops its keys.
//! - **[`Manifest::orphan_commits`] is the exception, and it exists because a real store had 13.**
//!   Keys in v2's `commits` that no surviving part carries. The v2 conversion (since deleted: this
//!   build reads v3 only) carried them verbatim, so the derivation could not lose an idempotency
//!   guarantee even where the store's history is not fully explained. On the live box those 13 are
//!   all `live-`-prefixed and date to 2026-08-02; none is a backfill key. Since 2026-10-02 the list
//!   has a second, deliberate tenant: the key-only EMPTY-DAY MARKER
//!   (`DataFusionHist::spend_keys_without_rows`), a key that records "this window held no rows"
//!   and so has no part to live on.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::time::Duration;

use datafusion::arrow::array::ArrayRef;
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use serde::{Deserialize, Serialize};

use crate::store::hist::{DataError, TsRange};
use crate::store::hist_maint::{Durability, WriteOpts};
use vike_model::time::epoch_ms_to_utc_date;

use super::delta::DeltaFrame;
use super::{fsync_dir, io, part_dir, q, write_parquet};

mod from_parts;
mod io;
mod lock;

pub use from_parts::RebuildReport;
pub(super) use from_parts::{merge_tmp_name, rebuild_manifest};
pub(super) use io::{
    fold_base, folded_orphan_commits, last_published_version, publish, read_base, read_manifest,
};
#[cfg(test)]
use lock::SPIN_ATTEMPTS;
pub(super) use lock::{SeriesLock, remove_series_contents};

/// The base manifest's filename. `pub(super)` so the repair plan can report on its PRESENCE:
/// "the base is missing while the log survives" is the one store state
/// [`read_manifest`] refuses outright, and a plan that named the file from a second literal would
/// be a second place to notice a rename.
pub(super) const MANIFEST: &str = "_manifest.json";
const MANIFEST_LOCK: &str = "_manifest.lock";
/// On-disk manifest format.
///
/// v2 added `FileEntry.date` (the `date=` partition key) + `commit_key` lineage + this `format`
/// tag. **v3 split the manifest into a BASE plus a framed delta log** and DERIVED the top-level
/// commit array from [`FileEntry::commit_keys`] — see this module's doc.
///
/// ⚠ **This build reads v3 and nothing else.** A v2 reader/migration existed while the live box
/// still held v2 series (`docs/decisions/0060-the-manifest-rewrite-is-the-write-amplification.md`:
/// 31 GiB of recorded tape cannot be re-fetched); every series there was converted first, and
/// decision 0117 keeps no migration code, so any other format is refused by [`read_base`] with the
/// repair that rebuilds the manifest from the parts.
const MANIFEST_FORMAT: u32 = 3;

/// How large `_manifest.delta` may grow before a publish folds it back into a new base.
///
/// The fold is the ONLY whole-file manifest write left on the append path, so this constant is the
/// amplification dial: a series pays `base_bytes / frames_per_fold` per commit instead of
/// `base_bytes`. MEASURED against the live box's largest series (104 MB base, ~250-byte frames,
/// one commit every ~9 s): 4 MiB is roughly 16,000 commits, i.e. one base rewrite every ~40 hours,
/// for an amortised ~6.5 KB of manifest per commit against today's 104 MB.
///
/// It is bounded at the other end by the REPLAY a reader pays: 4 MiB of compact JSON is a few tens
/// of milliseconds, against the ~0.9 s that the 104 MB base parse measured. Raising it trades read
/// latency for write volume; there is no operator setting for it deliberately — a settings key that
/// changes a durability-adjacent cadence is a way to break a store from a config file.
pub(super) const FOLD_BYTES: u64 = 4 * 1024 * 1024;

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

/// A per-series manifest: the file index + the ingest commit-log. The reader's source of truth for
/// which files to open, and the writer's idempotency guard.
///
/// **This is the in-memory FOLD of the base file and its delta log**, not the shape of either one:
/// [`read_manifest`] builds it by parsing `_manifest.json` and replaying `_manifest.delta` over it.
/// The serde impl is the BASE's on-disk shape, which is why two fields are excluded from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Manifest {
    /// On-disk format tag (see [`MANIFEST_FORMAT`]). Anything but the current format is refused.
    #[serde(default)]
    pub(super) format: u32,
    pub(super) version: u64,
    pub(super) files: Vec<FileEntry>,
    /// Ingested batch keys that NO file carries — the residue the derivation cannot see.
    ///
    /// ⚠ **This is not a design flourish; a real store had 13 of them.** MEASURED on the live box's
    /// largest series, 2026-09-16: v2's `commits` held 659,697 keys and `files[].commit_keys` held
    /// 659,684, all distinct. The 13 that sit in one and not the other are all `live-`-prefixed and
    /// all from 2026-08-02, the store's first day and the day a documented compaction incident hit
    /// this kind; none is a backfill key, which is the family whose re-offer the idempotency guard
    /// actually has to refuse forever. The (since deleted) v2 conversion put them HERE rather than
    /// deriving them away, so a store whose history is not fully explained still cannot lose an
    /// idempotency guarantee it had.
    ///
    /// ⚠ **A series born at v3 can hold orphans too, since 2026-10-02**, and this said it could not:
    /// an EMPTY-DAY MARKER (`DataFusionHist::spend_keys_without_rows`) records that a window held
    /// no rows, so it has no part to ride on and lands here through a delta frame's `keys_add`.
    /// Every other key still rides on the part that holds its rows. The repair plan tells the two
    /// apart by the marker's suffix (`crate::store::store_kind::EMPTY_MARKER_SUFFIX`), because dropping a
    /// marker costs a request and dropping a residue key costs an idempotency guarantee.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) orphan_commits: Vec<String>,
    /// Set when the next [`publish`] must write a WHOLE base rather than append a frame: the base
    /// file is absent (a brand-new series — `list_series` finds leaves by `_manifest.json`, so one
    /// must exist from the first commit). Never on disk.
    #[serde(skip)]
    pub(super) base_pending: bool,
    /// `Some(n)` when the delta log on disk is LONGER than its last intact frame, i.e. a crash left
    /// a torn tail; `n` is where the intact prefix ends.
    ///
    /// ⚠ **This is not diagnostics — without it the store would accept commits and silently lose
    /// them.** [`super::delta::delta_append`] opens the log in APPEND mode, so a frame written after
    /// a torn tail lands where the reader (which stops at the tear) will never reach it, and so does
    /// every frame after that, until the next fold. [`publish`] cuts the tail off first, under the
    /// series lock — a reader is lock-free and has no business rewriting a file. Never on disk.
    #[serde(skip)]
    pub(super) log_torn_at: Option<u64>,
}

impl Manifest {
    /// A fresh (empty) manifest stamped with the current format — for a series with no parts yet.
    pub(super) fn empty() -> Self {
        Manifest {
            format: MANIFEST_FORMAT,
            version: 0,
            files: Vec::new(),
            orphan_commits: Vec::new(),
            base_pending: false,
            log_torn_at: None,
        }
    }

    /// Has this batch key already been ingested? **The store's only idempotency** (must-fix #1:
    /// trades carry no id, so a dedup can only ever be batch-level).
    ///
    /// Answered from the parts themselves plus [`Self::orphan_commits`], because v3 stores a key
    /// exactly where its rows are. Same cost class as v2's `commits.iter().any(…)` — one pass of
    /// string comparisons over the same key set, no allocation and no index build. That matters:
    /// this runs inside [`SeriesLock`], on the live recorder's append path, and building a hash set
    /// of 659,697 keys to answer one membership question would have made the critical section
    /// WORSE while removing the bytes.
    pub(super) fn has_commit(&self, key: &str) -> bool {
        self.orphan_commits.iter().any(|c| c == key)
            || self.files.iter().any(|f| f.commit_keys.iter().any(|c| c == key))
    }

    /// Every ingested batch key, deduplicated, orphans first and then in file order.
    ///
    /// ⚠ **The ORDER is weaker than v2's and deliberately so.** v2 appended to one array, so its
    /// order was commit order; here it is `files` order, which IS commit order for an
    /// append-only series and is not after a compaction (a merge output joins at the end carrying
    /// the union of its inputs' keys — which was already true of `files` itself). Callers that care
    /// about membership (`delete_series_checked`'s `--produced-by`) are unaffected; the one that
    /// reports them verbatim (`series_commits`) documents the change.
    ///
    /// Deduplication is not cosmetic: a batch whose rows straddle a UTC midnight seals one part per
    /// date and stamps its key on BOTH, so the raw fold would report it twice.
    pub(super) fn commit_keys(&self) -> Vec<String> {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out = Vec::new();
        for k in self.orphan_commits.iter().chain(self.files.iter().flat_map(|f| &f.commit_keys)) {
            if seen.insert(k.as_str()) {
                out.push(k.clone());
            }
        }
        out
    }

    /// Apply one replayed [`DeltaFrame`] in place. The caller has already decided this frame is
    /// ABOVE the base's version — see [`read_manifest`].
    ///
    /// A `files_rm` naming a part that is not listed is IGNORED rather than an error: replay must
    /// be total over any prefix of the log a crash can leave, and a publish that removed a part is
    /// the same publish that stopped naming it.
    fn apply(&mut self, f: &DeltaFrame) {
        for (name, date) in &f.files_rm {
            if let Some(i) = self.files.iter().position(|e| &e.name == name && &e.date == date) {
                self.files.remove(i);
            }
        }
        self.files.extend(f.files_add.iter().cloned());
        for k in &f.keys_add {
            if !self.has_commit(k) {
                self.orphan_commits.push(k.clone());
            }
        }
        if !f.keys_rm.is_empty() {
            // The Q2 hatch's read half. Nothing in this tree writes a non-empty `keys_rm`, but the
            // whole point of the field is that a future retention answer lands as a FRAME and not
            // as a second format bump — so the replay that would honour it exists and is tested.
            let rm: HashSet<&str> = f.keys_rm.iter().map(String::as_str).collect();
            self.orphan_commits.retain(|c| !rm.contains(c.as_str()));
            for file in &mut self.files {
                file.commit_keys.retain(|c| !rm.contains(c.as_str()));
            }
        }
        self.version = f.version;
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

/// Returns `(rows sealed, the frame describing this publish)`. The caller hands that frame to
/// [`publish`] — it has already been applied to `m`, so the two cannot disagree about what changed.
pub(super) fn seal_into_manifest<F>(
    series_dir: &Path,
    m: &mut Manifest,
    commit_key: Option<&str>,
    extra_key: Option<&str>,
    ts: &[i64],
    schema: &Arc<Schema>,
    build_cols: F,
    opts: WriteOpts,
) -> Result<(usize, DeltaFrame), DataError>
where
    F: Fn(&[usize]) -> Vec<ArrayRef>,
{
    // group row indices by UTC day; BTreeMap = deterministic (sorted) iteration, no dep
    let mut by_date: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, &t) in ts.iter().enumerate() {
        by_date.entry(epoch_ms_to_utc_date(t)).or_default().push(i);
    }
    // The keys every sealed part of this call carries: `commit_key` (the idempotency key, if any)
    // PLUS `extra_key` — a caller-supplied key to stamp on TOP of `commit_key`, so the sealed part
    // is (and stays) recognizable as carrying both. A superseding commit (live or replayed, through
    // `SupersedeStep`) passes its `supersede_key` here whenever the supersede is not refused —
    // whether or not a provisional part existed to remove — so the freshly-sealed canonical part
    // carries `[canonical, provisional]` and the provisional key is SPENT with it: a provisional
    // write arriving later writes nothing. The same union is what lets `rebuild_manifest`'s
    // existing containment rule (a part whose commit-key set is a SUBSET of another's in the same
    // `date=` is a resurrected orphan) recognize a leftover `{provisional}` part as superseded by
    // this one — `{provisional}` and `{canonical}` alone are DISJOINT sets and the rule would never
    // fire without it. Every other caller passes `None` and gets exactly the old one-or-zero-key
    // shape.
    let keys: Vec<String> = commit_key.into_iter().chain(extra_key).map(str::to_string).collect();
    let mut written = 0usize;
    let mut added: Vec<FileEntry> = Vec::new();
    for (date, idxs) in &by_date {
        let name = next_part_name(&m.files, date);
        let dir = part_dir(series_dir, date);
        std::fs::create_dir_all(&dir).map_err(io)?;
        let batch = RecordBatch::try_new(schema.clone(), build_cols(idxs)).map_err(q)?;
        // The SAME keys recorded in the `FileEntry` below also go into the part's own footer, so
        // the part is self-describing and the manifest is rebuildable from the data alone.
        write_parquet(&dir.join(&name), schema.clone(), vec![batch], opts.profile, &keys)?;
        // The part's BYTES are durable (fsynced inside `write_parquet`); this makes its NAME durable
        // too. Skipped under `Bulk` for the same reason the manifest fsync is: without a durable
        // manifest entry there is nothing that could outlive the missing directory entry.
        if opts.durability == Durability::Fsync {
            fsync_dir(&dir)?;
        }
        let (lo, hi) = minmax_ts(idxs.iter().map(|&i| ts[i]));
        let entry = FileEntry {
            name,
            date: date.clone(),
            ts_min: lo,
            ts_max: hi,
            rows: idxs.len(),
            commit_keys: keys.clone(),
        };
        m.files.push(entry.clone());
        added.push(entry);
        written += idxs.len();
    }
    // v2 also pushed `commit_key` onto a separate `m.commits` array here. It is not dropped — it
    // rides on every `FileEntry` above, which is where `Manifest::has_commit` now reads it from,
    // and where it inherits exactly the lifetime the idempotency guard wants: as long as the rows.
    // A batch straddling a UTC midnight stamps the key on BOTH of its parts, which is why
    // `Manifest::commit_keys` deduplicates.
    m.version += 1;
    Ok((written, DeltaFrame { version: m.version, files_add: added, ..Default::default() }))
}

#[path = "replay_tests.rs"]
#[cfg(test)]
mod replay_tests;

#[path = "part_name_tests.rs"]
#[cfg(test)]
mod part_name_tests;

#[path = "series_lock_tests.rs"]
#[cfg(test)]
mod series_lock_tests;
