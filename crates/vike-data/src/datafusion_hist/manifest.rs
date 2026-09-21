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
//!   Keys in v2's `commits` that no surviving part carries. The v2 migration enumerates them and
//!   carries them verbatim, so the derivation cannot lose an idempotency guarantee even where the
//!   store's history is not fully explained. On the live box those 13 are all `live-`-prefixed and
//!   date to 2026-08-02; none is a backfill key.

use std::collections::{BTreeMap, BTreeSet, HashSet};
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

use super::delta::{self, DeltaFrame};
use super::{COMMIT_KEYS_META, fsync_dir, io, part_dir, q, write_parquet};

/// The base manifest's filename. `pub(super)` so the repair plan can report on its PRESENCE:
/// "the base is missing while the log survives" is the one store state
/// [`read_manifest`] refuses outright, and a plan that named the file from a second literal would
/// be a second place to notice a rename.
pub(super) const MANIFEST: &str = "_manifest.json";
const MANIFEST_LOCK: &str = "_manifest.lock";
/// Where the v2 base is kept when a store is migrated in place — the rollback the migration buys.
/// Written ONCE, before the v3 base replaces the file it copies, and never touched again.
pub(super) const MANIFEST_V2_BACKUP: &str = "_manifest.v2.json.bak";

/// On-disk manifest format.
///
/// v2 added `FileEntry.date` (the `date=` partition key) + `commit_key` lineage + this `format`
/// tag. **v3 split the manifest into a BASE plus a framed delta log** and DERIVED the top-level
/// commit array from [`FileEntry::commit_keys`] — see this module's doc.
///
/// ⚠ **v3 is a bump this build READS ACROSS rather than refuses.** v2's doc justified a hard break
/// on the grounds that "the data tree is derived — regenerate", and
/// `docs/decisions/0060-the-manifest-rewrite-is-the-write-amplification.md` records that as FALSE
/// for the live box: 31 GiB of recorded market tape cannot be re-fetched. So [`read_base`] accepts
/// a v2 file and converts it in memory, reads keep working with nothing written at all, and the
/// first WRITE migrates in place — see [`migrate_or_seed_base`]. A v1 manifest is still refused;
/// nothing in this tree can convert one.
const MANIFEST_FORMAT: u32 = 3;
/// The v2 format this build still reads. See [`MANIFEST_FORMAT`].
const MANIFEST_FORMAT_V2: u32 = 2;

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
    /// On-disk format tag (see [`MANIFEST_FORMAT`]). A v2 file is converted on read; anything else
    /// is refused.
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
    /// actually has to refuse forever. The v2 migration enumerates them and puts them HERE rather
    /// than deriving them away, so a store whose history is not fully explained still cannot lose
    /// an idempotency guarantee it had.
    ///
    /// Empty for a series born at v3: every key committed since then rides on the part that holds
    /// its rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) orphan_commits: Vec<String>,
    /// v2's top-level commit array. Deserialized ONLY so [`read_base`] can convert a legacy file;
    /// never serialized, so a v3 base cannot carry it back.
    #[serde(default, rename = "commits", skip_serializing)]
    legacy_commits: Vec<String>,
    /// Set when the next [`publish`] must write a WHOLE base rather than append a frame: the base
    /// file is absent (a brand-new series — `list_series` finds leaves by `_manifest.json`, so one
    /// must exist from the first commit) or it is v2 (the in-place migration). Never on disk.
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
            legacy_commits: Vec::new(),
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
            // v2 also pushed each key onto a separate top-level `commits` array here. v3 does not:
            // the key's home is the `FileEntry` below and `Manifest::has_commit` reads it there, so
            // the second copy this loop used to maintain was the 99% the whole format bump removes.
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
///
/// ⚠ `Serialize` because an OPERATOR reads these counts, and a `--json` caller must get the same
/// five numbers the human rendering prints rather than a prose line to re-parse — see
/// `crate::datafusion_hist::repair::RepairPlan`, which carries one of these whether the rebuild was
/// rehearsed or performed. No `Deserialize`: nothing reads one back, and the wire carries none of
/// this (the repair verb is engine-local by decision — that module's doc argues it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
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

/// Parse the BASE file alone — no delta replay. [`read_manifest`] is what callers want.
///
/// A **v2** file is converted here, in memory, writing nothing: its top-level `commits` array is
/// split into the keys some part still carries (dropped — they are already on the parts) and the
/// keys none does ([`Manifest::orphan_commits`]), and `base_pending` is raised so the next write
/// migrates the file. **An old store is therefore READABLE by this build with no migration step at
/// all**, which is the property `MANIFEST_FORMAT`'s doc explains and `0060` demanded: the live
/// box's 31 GiB of tape is not regenerable, so a format bump cannot mean "regenerate the store".
pub(super) fn read_base(dir: &Path) -> Result<Manifest, DataError> {
    let bytes = match std::fs::read(dir.join(MANIFEST)) {
        Ok(b) => b,
        // A series with no base yet. `base_pending` so the FIRST publish writes one: `list_series`
        // finds leaves by the presence of `_manifest.json`, so a series that only ever appended
        // frames would be invisible to maintenance, to the Data Manager and to `delete_series`.
        Err(ref e) if e.kind() == ErrorKind::NotFound => {
            return Ok(Manifest { base_pending: true, ..Manifest::empty() });
        }
        Err(e) => return Err(io(e)),
    };
    let mut m: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
        DataError::Query(format!(
            "manifest parse at {} (incompatible format? this build reads v{MANIFEST_FORMAT} and \
             converts v{MANIFEST_FORMAT_V2} in place): {e}",
            dir.display()
        ))
    })?;
    match m.format {
        MANIFEST_FORMAT => {}
        MANIFEST_FORMAT_V2 => {
            // The keys v2 held twice are already on the parts; the residue is what the derivation
            // cannot see, and it is carried verbatim. `has_commit` is used rather than a set build
            // because this runs once per open of a legacy series, on a `files` vector we have in
            // hand, and the orphan count is measured in single digits.
            let on_files: HashSet<&str> =
                m.files.iter().flat_map(|f| f.commit_keys.iter().map(String::as_str)).collect();
            let orphans: Vec<String> = std::mem::take(&mut m.legacy_commits)
                .into_iter()
                .filter(|k| !on_files.contains(k.as_str()))
                .collect();
            if !orphans.is_empty() {
                tracing::info!(
                    series = %dir.display(),
                    orphan_commit_keys = orphans.len(),
                    "manifest v2 -> v3: commit keys that no surviving part carries are being \
                     carried forward verbatim rather than derived away"
                );
            }
            m.orphan_commits = orphans;
            m.format = MANIFEST_FORMAT;
            m.base_pending = true;
        }
        other => {
            return Err(DataError::Query(format!(
                "manifest format v{other} at {} — this build reads v{MANIFEST_FORMAT} and converts \
                 v{MANIFEST_FORMAT_V2}; nothing here can convert v{other} (regenerate the store, \
                 or rebuild its manifest from the parts with `vike-cli data repair --kind K \
                 --venue V …`, which reaches this state: the rebuild never parses the base, it \
                 asks `last_published_version`, which swallows this very error to 0)",
                dir.display()
            )));
        }
    }
    m.legacy_commits = Vec::new();
    Ok(m)
}

/// The manifest as of this instant: the base file, with every delta frame it does not already carry
/// replayed over it.
///
/// ⚠ **The LOG is read before the BASE, and the order is load-bearing rather than stylistic.** A
/// fold publishes a new base and THEN clears the log; a reader that took the base first could pair
/// a pre-fold base with a post-fold (empty) log and silently lose every frame the fold had just
/// folded in. Taking the log first cannot lose anything — `delta.rs`'s "Ordering" section carries
/// the case analysis. Do not reorder these two statements.
///
/// Replay is version-guarded, so a fold that published its base and died before clearing the log
/// re-reads frames the base already holds and skips every one. Without that, such a crash would
/// double every `FileEntry` the fold had absorbed, and a doubled entry is a part READ TWICE.
pub(super) fn read_manifest(dir: &Path) -> Result<Manifest, DataError> {
    let (frames, intact_len) = delta::delta_read_with_end(dir)?;
    // ⚠ A base and its log are ONE object, and half of one is CORRUPTION rather than a degraded
    // read. Replaying frames onto an empty manifest would succeed and produce an index naming SOME
    // of the series' parts — every commit since the last fold and none before it — so a query over
    // the older window would return fewer rows with no error at all. That is the silent shape; an
    // error is the recoverable one, and the recovery already exists.
    //
    // No legitimate writer produces this state: a series' first commit publishes a base before any
    // frame exists, a fold RENAMES over the base so it is never absent, and the v2 migration keeps
    // the old file in place until the new one replaces it. Someone deleted the base.
    if !frames.is_empty() && !dir.join(MANIFEST).is_file() {
        let delta_name = delta::DELTA;
        return Err(DataError::Query(format!(
            "manifest at {} has a delta log but NO base ({MANIFEST} is missing): replaying the log \
             alone would index only the parts committed since the last fold and silently hide the \
             rest. The repair is `DataFusionHist::rebuild_series_manifest`, which re-derives the \
             index from the parts themselves, and the OPERATOR spelling is `vike-cli data repair \
             --kind K --venue V (--symbol S [--interval I] | --group G) [--store DIR]` — which \
             REHEARSES by default and writes only with --yes. Removing `{delta_name}` as well \
             would also make the series readable, at the cost of every commit since the last fold.",
            dir.display()
        )));
    }
    let mut m = read_base(dir)?;
    for f in &frames {
        if f.version <= m.version {
            continue; // already folded into the base
        }
        m.apply(f);
    }
    // Carry the tear (if any) up to the next writer — see `Manifest::log_torn_at`. Nothing is
    // repaired here: this function runs lock-free.
    let on_disk = delta::delta_len(dir);
    m.log_torn_at = (on_disk > intact_len).then_some(intact_len);
    Ok(m)
}

/// Publish a BASE manifest by atomic rename (write tmp → replace). `fs::rename` replaces on modern
/// Windows + unix; the remove-then-rename fallback covers any platform that won't.
///
/// ⚠ **This is no longer the append path's publish** — [`publish`] is, and it appends a frame. This
/// runs only at a FOLD, at a v2 MIGRATION, when a series gets its first base, and from
/// `rebuild_series_manifest`. Callers outside this module must go through [`publish`] or
/// [`fold_base`]: writing a base without clearing the log leaves frames that will replay over it.
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

/// The highest version this series has ever published, tolerating a manifest that is only half
/// present. ONLY [`super::DataFusionHist::rebuild_series_manifest`] uses it, because a rebuild is
/// the repair for exactly the state [`read_manifest`] refuses, and it still must not restart the
/// version counter underneath a delta log or a compaction's output names.
pub(super) fn last_published_version(dir: &Path) -> u64 {
    let base = read_base(dir).map(|m| m.version).unwrap_or(0);
    let log = delta::delta_read_with_end(dir)
        .ok()
        .and_then(|(frames, _)| frames.last().map(|f| f.version))
        .unwrap_or(0);
    base.max(log)
}

/// Collapse the delta log into a new base: publish the base, THEN clear the log.
///
/// **The order is the whole of the crash story and it may not be swapped.** Publishing first means
/// a crash between the two steps leaves a durable base plus a log whose frames that base already
/// carries — and replay skips them by version, so the state is exactly right and the next fold
/// clears the log. Clearing first would mean a crash loses every frame the base had not yet
/// absorbed, which is published commits gone.
pub(super) fn fold_base(dir: &Path, m: &Manifest, durability: Durability) -> Result<(), DataError> {
    write_manifest(dir, m, durability)?;
    delta::delta_clear(dir)
}

/// Write the first base a series has, or migrate a v2 one in place. Raised by
/// [`Manifest::base_pending`]; performed by [`publish`] on the next write.
///
/// # The migration, and what makes it safe on a store that cannot be regenerated
///
/// The live box holds 31 GiB of recorded market tape that cannot be re-fetched, so
/// `docs/decisions/0060-…` records `MANIFEST_FORMAT`'s "the data tree is derived — regenerate" as
/// false there. This is the whole migration, and it has four properties:
///
/// 1. **Reads never need it.** [`read_base`] converts a v2 file in memory, so an un-migrated store
///    is fully readable by this build. The migration happens on the first WRITE.
/// 2. **It costs exactly one whole-file manifest write** — the write the store was about to do
///    anyway under v2. There is no separate pass and no downtime.
/// 3. **It is REVERSIBLE**, because the v2 file is copied to [`MANIFEST_V2_BACKUP`] BEFORE the v3
///    base replaces it. Rollback to a pre-v3 build is: stop the writer, `mv _manifest.v2.json.bak
///    _manifest.json`, `rm _manifest.delta`. That restores the manifest as of the migration
///    instant; parts committed since are on disk and `rebuild_manifest` re-indexes them, which is
///    the same recovery a v2 store already had.
/// 4. **A SIGKILL anywhere in it loses nothing.** Before the backup: nothing has changed. Mid-copy:
///    the bytes are under a `.tmp` name, so no half-written backup exists to be trusted later.
///    Between the backup and the base rename: `_manifest.json` is still the v2 file, so the next
///    open reads v2 and redoes this. After the rename: the v3 base is durable and the backup is
///    beside it. The one ordering that would be unsafe — publishing the v3 base before the backup —
///    is the reason the backup is step one.
///
/// ⚠ **It does not touch a single Parquet part**, which is the property that makes it affordable on
/// a store whose tape cannot be re-fetched: the only bytes at risk are an index that
/// [`rebuild_manifest`] can re-derive from the parts.
///
/// A backup that already exists is LEFT ALONE: it is the oldest v2 state, which is the one worth
/// keeping, and overwriting it with a newer one would quietly narrow what a rollback can reach.
fn migrate_or_seed_base(dir: &Path, m: &Manifest, durability: Durability) -> Result<(), DataError> {
    let legacy = dir.join(MANIFEST);
    let backup = dir.join(MANIFEST_V2_BACKUP);
    if legacy.is_file() && !backup.exists() {
        // COPY, not rename: the v2 file must stay in place until the v3 base has replaced it, so a
        // crash in between reads as "still v2" rather than as "no manifest".
        //
        // ⚠ Via a tmp file, because the backup is only ever consulted by a human performing a
        // rollback and the `!backup.exists()` guard above would happily KEEP a half-written one. A
        // crash mid-copy must leave no `MANIFEST_V2_BACKUP` at all rather than a truncated file
        // that looks like a rollback and is not one. `fs::copy` straight to the final name cannot
        // give that; nor can it give a durable copy, since fsync belongs on a descriptor opened for
        // WRITING (POSIX leaves fsync on a read-only one to the implementation).
        let tmp = dir.join(format!("{MANIFEST_V2_BACKUP}.tmp"));
        {
            let mut src = File::open(&legacy).map_err(io)?;
            let mut dst = std::fs::File::create(&tmp).map_err(io)?;
            std::io::copy(&mut src, &mut dst).map_err(io)?;
            if durability == Durability::Fsync {
                dst.sync_all().map_err(io)?;
            }
        }
        std::fs::rename(&tmp, &backup).map_err(io)?;
        if durability == Durability::Fsync {
            fsync_dir(dir)?;
        }
        tracing::info!(
            series = %dir.display(),
            backup = %backup.display(),
            "manifest v2 -> v3: the previous base was copied aside before migrating (restore it \
             and remove _manifest.delta to roll back to a pre-v3 build)"
        );
    }
    fold_base(dir, m, durability)
}

/// Publish one change to a series manifest.
///
/// **The caller has already applied the change to `m`** (sealed its parts into `m.files`, dropped
/// the ones it removed, bumped `m.version`) and describes it in `frame`. This is the write side's
/// single entry point and it replaces the `m.version += 1; write_manifest(…)` pair every publish
/// site used to spell for itself.
///
/// Ordering, and why it is the same durability boundary as before:
///
/// - The frame is appended and fsynced only AFTER every part it names has had its bytes and its
///   directory entry fsynced, so a reader can never be handed a manifest naming a part that is not
///   there. That was `write_manifest`'s guarantee and it is unchanged; only the fsync got smaller.
/// - A caller may clear an applied WAL record only once this returns. That is the ordering
///   `wal.rs`'s `wal_rewrite_keeping_unapplied` depends on, and the boundary it waits on moves from
///   "the whole manifest is durable" to "this frame is durable".
/// - A crash before the frame's fsync leaves the sealed part on disk under no manifest entry —
///   exactly the window the WAL exists to close, and it closes it identically.
///
/// `fold_bytes` is the log size at which this folds (see [`FOLD_BYTES`]); it is a parameter only so
/// tests can drive a fold in milliseconds instead of at the production cadence.
pub(super) fn publish(
    dir: &Path,
    m: &mut Manifest,
    frame: DeltaFrame,
    durability: Durability,
    fold_bytes: u64,
) -> Result<(), DataError> {
    if m.base_pending {
        // A first base, or a v2 migration. `m` already carries the change, so writing it whole
        // publishes the change too — there is no frame to append, and any stale log is cleared.
        migrate_or_seed_base(dir, m, durability)?;
        m.base_pending = false;
        m.log_torn_at = None; // the log is gone; there is no tail left to cut
        return Ok(());
    }
    // A crash left a torn tail. Cut it off BEFORE appending, or this frame lands beyond where any
    // reader stops and is silently lost — as is every frame after it, until the next fold. This is
    // the writer's job and this is the only place it is safe: we hold the series lock.
    if let Some(intact_len) = m.log_torn_at.take() {
        delta::delta_truncate(dir, intact_len)?;
    }
    delta::delta_append(dir, &frame, durability)?;
    if delta::delta_len(dir) >= fold_bytes {
        fold_base(dir, m, durability)?;
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

    /// ONE attempt, no spin, and **no `create_dir_all`** — `Ok(None)` means another writer holds
    /// this series' lock RIGHT NOW, in this process or any other.
    ///
    /// # Why a caller would want the refusal instead of the spin
    ///
    /// [`acquire`](Self::acquire)'s ~4 s spin is right for a WRITER that must eventually write:
    /// spinning costs the holder nothing, and the loser simply waits. It is wrong for the manifest
    /// REPAIR ([`super::super::DataFusionHist::rebuild_series_manifest_if_uncontended`]), and the
    /// asymmetry is about the critical SECTION rather than about the wait: a rebuild reads every
    /// part footer in the series INSIDE the lock, so on a series with hundreds of parts the hold is
    /// long — and `crates/vike-data/src/live_rec.rs`'s `RecorderSink` discards its buffer (up to
    /// 5,000 rows) when its own flush spins that budget out. Winning a contended lock is therefore
    /// the outcome to avoid, not the one to wait for.
    ///
    /// ⚠ **The attempt IS the probe, deliberately.** A "check then acquire" pair is a TOCTOU gap a
    /// writer arrives in, so this returns the GUARD it took rather than an answer about whether one
    /// could be taken. What it cannot do is prove the series is idle: a recorder holds this lock
    /// only while committing, so an absent lock is an absent COMMIT, not an absent writer. The
    /// caller owes that sentence to its operator; this function owes only the honest snapshot.
    ///
    /// No `create_dir_all` (unlike [`acquire_within`](Self::acquire_within)) because the one caller
    /// repairs a series that EXISTS: creating the leaf here would mint a phantom series at a
    /// mistyped selector and then publish an empty manifest into it, which `list_series` would
    /// enumerate forever. An absent directory is `NotFound` — an error the caller reports rather
    /// than a leaf it invents.
    pub(super) fn try_acquire(series_dir: &Path) -> Result<Option<Self>, DataError> {
        let path = series_dir.join(MANIFEST_LOCK);
        let file =
            OpenOptions::new().write(true).create(true).truncate(false).open(&path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SeriesLock(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
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

/// Remove EVERYTHING inside a series leaf except the lock file — the first half of a locked delete
/// (`DataFusionHist::delete_series_checked`, whose doc carries the whole argument).
///
/// The exclusion is the point, and it is stated as "everything but the lock" rather than as a list
/// of what a series holds: the caller is running INSIDE the guard on `_manifest.lock`, so removing
/// that file would delete the inode the kernel is holding its lock on — and on Windows the open
/// descriptor makes the removal fail outright. Every other entry goes: the `date=` partitions and
/// their parts, `_manifest.json`, a half-written `_manifest.json.tmp`, the `_wal.arrow` if a crash
/// left one, and any `_tmp-merge-…` an interrupted compaction abandoned. Spelled as a NEGATIVE
/// filter so a sibling file added later is removed by construction rather than left behind by an
/// enumeration nobody remembered to extend — which is exactly what happened when v3 added
/// `_manifest.delta` and [`MANIFEST_V2_BACKUP`]: both are swept with no edit here, and a positive
/// list would have left a delta log behind to replay over the next series created at this path.
///
/// A leaf that vanishes under us is `Ok(())`: this is a delete, and something else having already
/// done the work is the outcome asked for.
pub(super) fn remove_series_contents(dir: &Path) -> Result<(), DataError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    for entry in entries {
        let entry = entry.map_err(io)?;
        if entry.file_name() == MANIFEST_LOCK {
            continue;
        }
        let path = entry.path();
        let removed = if entry.file_type().map_err(io)?.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => {}
            Err(ref e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                return Err(DataError::Query(format!(
                    "delete series {}: removing {}: {e}",
                    dir.display(),
                    path.display()
                )));
            }
        }
    }
    Ok(())
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

/// Returns `(rows sealed, the frame describing this publish)`. The caller hands that frame to
/// [`publish`] — it has already been applied to `m`, so the two cannot disagree about what changed.
pub(super) fn seal_into_manifest<F>(
    series_dir: &Path,
    m: &mut Manifest,
    commit_key: Option<&str>,
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
    let mut written = 0usize;
    let mut added: Vec<FileEntry> = Vec::new();
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
        let entry = FileEntry {
            name,
            date: date.clone(),
            ts_min: lo,
            ts_max: hi,
            rows: idxs.len(),
            commit_keys: commit_key.map(|k| k.to_string()).into_iter().collect(),
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

#[cfg(test)]
mod replay_tests {
    use super::*;

    fn fe(name: &str, date: &str, keys: &[&str]) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            date: date.to_string(),
            ts_min: 0,
            ts_max: 1,
            rows: 1,
            commit_keys: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    /// Replay applies a frame's adds and removes, and takes the frame's version rather than
    /// incrementing — which is what keeps the counter monotonic across a log whose frames a fold
    /// may already have absorbed.
    #[test]
    fn a_frame_adds_removes_and_carries_its_own_version() {
        let mut m = Manifest {
            version: 7,
            files: vec![fe("part-00001.parquet", "2026-01-01", &["a"])],
            ..Manifest::empty()
        };
        m.apply(&DeltaFrame {
            version: 8,
            files_add: vec![fe("part-c00000008.parquet", "2026-01-01", &["a", "b"])],
            files_rm: vec![("part-00001.parquet".into(), "2026-01-01".into())],
            ..Default::default()
        });
        assert_eq!(m.version, 8);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].name, "part-c00000008.parquet");
        assert!(m.has_commit("a") && m.has_commit("b"), "the merge output carries both keys");
    }

    /// A `files_rm` naming a part that is not listed is ignored, not an error. Replay has to be
    /// total over any prefix of the log a crash can leave.
    #[test]
    fn removing_an_absent_part_is_not_an_error() {
        let mut m = Manifest { version: 1, ..Manifest::empty() };
        m.apply(&DeltaFrame {
            version: 2,
            files_rm: vec![("gone.parquet".into(), "2026-01-01".into())],
            ..Default::default()
        });
        assert_eq!(m.version, 2);
        assert!(m.files.is_empty());
    }

    /// Removal matches on `(name, DATE)` and not on name alone — part names are unique within a
    /// `date=` directory and NOT across a series, so `part-00001.parquet` exists under every date
    /// and a name-only match would drop the wrong one.
    #[test]
    fn removal_matches_the_date_too() {
        let mut m = Manifest {
            version: 1,
            files: vec![
                fe("part-00001.parquet", "2026-01-01", &["a"]),
                fe("part-00001.parquet", "2026-01-02", &["b"]),
            ],
            ..Manifest::empty()
        };
        m.apply(&DeltaFrame {
            version: 2,
            files_rm: vec![("part-00001.parquet".into(), "2026-01-02".into())],
            ..Default::default()
        });
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].date, "2026-01-01", "the wrong date's part was dropped");
    }

    /// **The Q2 hatch, exercised.** `docs/decisions/0060-…`'s Q2 — what the commit log retains, and
    /// for how long — is the owner's to answer, and the whole point of these two fields is that an
    /// answer lands as a FRAME rather than as a second format bump. Nothing in this tree writes a
    /// non-empty `keys_add`/`keys_rm`, so without this test the replay honouring them would be
    /// dead code that nobody had ever run — and "the format can express it" would be a claim rather
    /// than a fact.
    ///
    /// `keys_rm` removes a key from BOTH homes: the orphan list and every `FileEntry` carrying it.
    /// That is what an EXPIRY policy needs — a key leaving the log while its part stays — and it is
    /// precisely the shape the current answer (a key lives exactly as long as its rows) does not
    /// use.
    #[test]
    fn the_q2_hatch_can_add_and_remove_keys_without_touching_files() {
        let mut m = Manifest {
            version: 1,
            files: vec![fe("part-00001.parquet", "2026-01-01", &["live-x", "pmxt:keep"])],
            ..Manifest::empty()
        };
        // keys_add: a key joins the log carrying no file at all.
        m.apply(&DeltaFrame {
            version: 2,
            keys_add: vec!["orphaned-by-policy".into()],
            ..Default::default()
        });
        assert!(m.has_commit("orphaned-by-policy"));
        assert_eq!(m.files.len(), 1, "keys_add must not touch the file index");

        // keys_rm: an expiry policy drops a key whose PART is still live.
        m.apply(&DeltaFrame {
            version: 3,
            keys_rm: vec!["live-x".into(), "orphaned-by-policy".into()],
            ..Default::default()
        });
        assert!(!m.has_commit("live-x"), "an expired key must leave the file that carries it");
        assert!(!m.has_commit("orphaned-by-policy"), "...and the orphan list");
        assert!(m.has_commit("pmxt:keep"), "a key the policy did not name is untouched");
        assert_eq!(m.files.len(), 1, "the PART stays — only its key entry went");
        assert_eq!(m.version, 3);
    }

    /// A key on two parts (a batch straddling a UTC midnight seals one per date) is reported ONCE.
    #[test]
    fn commit_keys_deduplicates_a_day_straddling_batch() {
        let m = Manifest {
            version: 2,
            files: vec![
                fe("part-00001.parquet", "2026-01-01", &["spanning"]),
                fe("part-00001.parquet", "2026-01-02", &["spanning"]),
            ],
            ..Manifest::empty()
        };
        assert_eq!(m.commit_keys(), vec!["spanning".to_string()]);
    }
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
