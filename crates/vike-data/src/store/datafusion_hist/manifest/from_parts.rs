//! Rebuilding a series manifest from its Parquet parts, and the merge-output name that rebuild skips.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use crate::store::hist::DataError;

use super::super::{COMMIT_KEYS_META, io};
use super::{FileEntry, Manifest};

/// Reconstruct a series' manifest from the PARTS THEMSELVES, ignoring any existing `_manifest.json`.
///
/// This is the property that demotes the manifest from ground truth to a rebuildable CACHE — the
/// posture every comparable system takes and this store did not. Parquet-catalog systems
/// re-derive their filename index from Parquet row-group statistics;
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
///    CONTENT distinguishes it. A part's `commit_keys` are exactly the batches whose rows it holds
///    — with ONE deliberate exception: a SUPERSEDING commit's freshly-sealed part is ALSO stamped
///    with the key it superseded, even though that key's rows are not inside this part (they were
///    just removed under it) — see `DataFusionHist::append_quotes_superseding`'s doc for why: it is
///    what keeps `has_commit` true for a spent key, and it is what lets this very rule recognize a
///    crash-orphaned copy of the superseded file as CONTAINED in the new part rather than as an
///    unexplained stranger. Outside that one case, a merge output carries the UNION of its inputs'
///    keys, so a part whose key set is a strict subset of another part's in the same `date=` is
///    contained in that part and is skipped ([`RebuildReport::parts_superseded`]). Exact, not
///    heuristic — and immune to part-name reuse, which is why the rule keys on content and never on
///    names (`next_part_name` deliberately recycles `part-NNNNN` indices after a compaction).
///
/// ⚠ Two residuals, both narrower than the bug they replace and neither silent-by-design:
/// a KEYLESS part (no `commit_key` at the append, or a part predating `COMMIT_KEYS_META`) carries no
/// content identity, so rule 2 cannot see it — [`RebuildReport::parts_without_keys`] is where that
/// shows up; and two compaction passes racing on ONE date can leave two outputs whose key sets
/// overlap without either containing the other, which rule 2 keeps both of.
///
/// Does NOT write anything — the caller publishes the result (or compares it, as the round-trip
/// test does). Reads footers only, never row data.
pub(crate) fn rebuild_manifest(dir: &Path) -> Result<(Manifest, RebuildReport), DataError> {
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
pub(crate) fn merge_tmp_name(out_name: &str) -> String {
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
/// `crate::store::datafusion_hist::repair::RepairPlan`, which carries one of these whether the rebuild was
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
