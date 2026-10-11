//! The supersede machinery one commit goes through, shared by the live commit and the WAL replay.

use std::path::{Path, PathBuf};

use crate::store::hist::DataError;
use crate::store::hist_maint::Durability;

use super::delta::DeltaFrame;
use super::manifest::{Manifest, publish};
use super::merge::part_dir;

/// What a supersede must do, decided by [`plan_supersede`] from the manifest AS IT STOOD BEFORE
/// this call sealed anything.
enum SupersedePlan {
    /// `supersede_key` was never spent — nothing to remove. The part(s) the commit seals are
    /// STILL stamped with it: the canonical commit pre-spends its provisional twin (see
    /// [`SupersedeStep`]'s `stamp`), so a provisional write that arrives after it writes nothing.
    NeverSpent,
    /// `supersede_key` names EXACTLY these files (each identified by its `(name, date)`, never by
    /// re-matching a key set — see [`plan_supersede`]'s doc for why identity, not a key-set match,
    /// is what [`apply_supersede`] removes by). **Can hold more than one entry**: a commit whose
    /// `ts` straddles a UTC day boundary seals ONE part PER DATE ([`manifest::seal_into_manifest`] groups by
    /// `epoch_ms_to_utc_date`), and every part from that one commit carries the SAME commit key —
    /// so a day-straddling provisional commit produces two or more `FileEntry`s all under
    /// `commit_keys: ["provisional"]`, and every one of them must be removed together or the
    /// left-behind ones permanently duplicate whatever they hold.
    ExactMatch(Vec<(String, String)>),
}

/// Decide what a commit must do about `supersede_key`, reading `m` as it stood BEFORE this call
/// added anything. Its one caller is [`SupersedeStep::decide`], which both the live commit
/// ([`DataFusionHist::commit_rows_inner`]) and the WAL replay (`wal::recover_series`) go through.
///
/// ⚠ **This MUST run before [`manifest::seal_into_manifest`], never after — trying it after produced a real
/// bug.** [`manifest::seal_into_manifest`]'s `extra_key` stamps `supersede_key` onto the part THIS call is
/// about to seal (so a crash-orphaned copy stays recognizable to the repair tool's containment
/// rule — see its doc). Checking "is `supersede_key` already present in some OTHER part" AFTER that
/// stamp lets the freshly-sealed part answer its own question: it always carries `supersede_key`
/// now, so a post-seal check reads that as "already folded into a multi-key part" and refuses every
/// legitimate supersede, including the ordinary never-spent no-op. Deciding first, against the
/// PRE-seal manifest, is what keeps the two independent (this is what
/// `superseding_a_key_that_was_never_spent_is_a_harmless_no_op` pins).
///
/// Distinguishes "the key was never spent" ([`SupersedePlan::NeverSpent`] — a legitimate no-op,
/// nothing wrong) from "the key WAS spent, but at least one occurrence is inside a part that ALSO
/// carries OTHER keys" (`Err`): this store's default background maintenance (`run_maintenance`,
/// whose `CompactionConfig::default()` sets `min_parts: 4`) can merge a provisional part into a
/// multi-key compacted part before its canonical commit ever arrives, and once that happens the
/// exact-match removal can never find that occurrence again — it is gone, folded into e.g.
/// `{k1, provisional}`. **This refuses even when other, genuinely exact-match occurrences of the
/// SAME key also exist** (a day-straddling commit where one date's part got compacted away and
/// another date's did not): partial removal would silently under-remove and still double whatever
/// the folded-away part holds, which is worse than refusing outright — there is no row count this
/// function could return that would make a partial removal look like what it is. Refusing is the
/// honest answer — the removal this call was asked to perform cannot be honored exactly, and the
/// caller must resolve that out of band rather than have this function guess.
fn plan_supersede(m: &Manifest, supersede_key: &str) -> Result<SupersedePlan, DataError> {
    let target = vec![supersede_key.to_string()];
    let exact: Vec<(String, String)> = m
        .files
        .iter()
        .filter(|f| f.commit_keys == target)
        .map(|f| (f.name.clone(), f.date.clone()))
        .collect();
    let folded = m
        .files
        .iter()
        .find(|f| f.commit_keys != target && f.commit_keys.iter().any(|k| k == supersede_key));
    if let Some(f) = folded {
        let msg = if exact.is_empty() {
            format!(
                "cannot supersede commit key {supersede_key:?}: no exact-match part exists for \
                 it — its only occurrence is inside part {:?} (date {:?}, key set {:?}), merged \
                 into a multi-key part (e.g. by compaction). This call cannot remove every \
                 occurrence of {supersede_key:?} without also dropping rows that part holds under \
                 its OTHER keys — refusing rather than partially removing it and silently \
                 doubling the rest",
                f.name, f.date, f.commit_keys,
            )
        } else {
            format!(
                "cannot supersede commit key {supersede_key:?}: {} exact-match part(s) exist for \
                 it, but the key ALSO occurs inside part {:?} (date {:?}, key set {:?}), merged \
                 into a multi-key part (e.g. by compaction). This call cannot remove every \
                 occurrence of {supersede_key:?} without also dropping rows that part holds under \
                 its OTHER keys — refusing rather than partially removing it and silently \
                 doubling the rest",
                exact.len(),
                f.name,
                f.date,
                f.commit_keys,
            )
        };
        return Err(DataError::Query(msg));
    }
    if exact.is_empty() {
        Ok(SupersedePlan::NeverSpent)
    } else {
        Ok(SupersedePlan::ExactMatch(exact))
    }
}

/// Apply a previously-computed [`SupersedePlan::ExactMatch`] against `m`/`frame`, returning the
/// physical paths to unlink AFTER `frame` durably publishes (manifest-first, exactly like
/// [`DataFusionHist::apply_retention_at`]). Removes EVERY listed `(name, date)` by IDENTITY, never
/// by re-matching a key set — a part [`manifest::seal_into_manifest`] just sealed alongside this removal may
/// itself now carry `supersede_key` (see [`plan_supersede`]'s doc), so a key-set match here could
/// otherwise mistake that brand-new part for one being superseded.
fn apply_supersede(
    series_dir: &Path,
    m: &mut Manifest,
    frame: &mut DeltaFrame,
    targets: &[(String, String)],
) -> Vec<PathBuf> {
    let before = m.files.len();
    m.files.retain(|f| !targets.iter().any(|(n, d)| f.name == *n && f.date == *d));
    debug_assert_eq!(
        m.files.len(),
        before - targets.len(),
        "plan_supersede's ExactMatch targets must all still be present: {targets:?}"
    );
    m.version += 1;
    frame.version = m.version;
    let mut to_unlink = Vec::with_capacity(targets.len());
    for (name, date) in targets {
        frame.files_rm.push((name.clone(), date.clone()));
        to_unlink.push(part_dir(series_dir, date).join(name));
    }
    to_unlink
}

/// What ONE commit does about its `supersede_key` — the part of the supersede sequence the live
/// commit ([`DataFusionHist::commit_rows_inner`]) and the WAL replay (`wal::recover_series`) share.
///
/// ⚠ **It exists so the two cannot drift, and before it they could.** Each used to spell out the
/// sequence around [`plan_supersede`] and [`apply_supersede`] for itself — turning the plan into the
/// parts to remove and the key to stamp, folding the removal into the frame, publishing — and the
/// replay's copy carried only a comment promising to decide "the SAME way the live path does". Only
/// the live copy's stamp was tested: a replay that stamped nothing passed the whole suite, because
/// the stamp changes no row. Now both paths call this, so one edit here changes both and one
/// mutation reddens both — `crates/vike-data/tests/store/superseding_commits.rs`'s
/// `a_resurrected_orphan_provisional_part_is_recognized_and_not_double_counted` pins the live stamp
/// and `a_replayed_superseding_commit_stamps_the_key_it_superseded` the replayed one.
///
/// What stays with each caller, because the two paths genuinely differ there: only the live path
/// appends to the WAL (a replayed record is already in it); the live path seals with its caller's
/// `WriteProfile` and the replay with `WriteOpts::live`; the live path rewrites the WAL after each
/// publish and the replay once, after its loop; the two test seams exist on the live path only; and
/// only the live path can arrive with an empty batch, which skips the seal and still supersedes.
pub(crate) struct SupersedeStep<'k> {
    /// Parts removed in the same publish, by `(name, date)` identity ([`apply_supersede`]). Empty =
    /// remove nothing.
    remove: Vec<(String, String)>,
    /// The extra key every part this commit seals carries beside its commit key — the `extra_key`
    /// [`manifest::seal_into_manifest`] is handed. Set whenever there IS a supersede key and the plan does not
    /// refuse, exact match or never spent alike: a canonical commit always spends its provisional
    /// twin, and PRE-SPENDS one that was never written.
    ///
    /// ⚠ **It is set for a never-spent twin too, and that is the whole guard against a LATE twin.**
    /// It used to be set for an exact match only. A provisional commit landing AFTER its canonical
    /// twin — a recent request that decided "recent" from its one clock read and then spent minutes
    /// fetching, beaten by a request that started after the window crossed the margin; or the
    /// twin's WAL record, whose publish had failed, replayed once the canonical commit published —
    /// then found its key unspent and sealed its early rows beside the settled ones, doubling them
    /// for good, since no later settled pass could ever reach the supersede again (its own key was
    /// spent). Pre-spent, the late twin meets `Manifest::has_commit` under the series lock and is
    /// the idempotent no-op, and the canonical commit's own WAL rewrite finds a pending twin record
    /// applied and drops it. The stamp's other reader is the repair tool's containment rule (see
    /// `seal_into_manifest`'s doc), for which it keeps a crash-orphaned copy of a removed part
    /// recognizable. An EMPTY batch seals nothing and therefore stamps nothing — its commit key
    /// stays unspent too, so the next settled pass retries the window.
    pub(crate) stamp: Option<&'k str>,
}

/// What [`SupersedeStep::publish`] did.
pub(crate) struct Published {
    /// Whether a frame was published at all. Only a publish makes the commit durable, so only a
    /// publish lets the live path drop its WAL record.
    pub(crate) published: bool,
    /// The superseded parts' paths, which the caller hands to [`unlink_superseded`] only AFTER its
    /// own post-publish steps — manifest-first, unlink-after.
    pub(crate) to_unlink: Vec<PathBuf>,
}

impl<'k> SupersedeStep<'k> {
    /// [`plan_supersede`] plus the stamp rule, against `m` as it stood BEFORE this commit sealed
    /// anything (`plan_supersede`'s doc says why the order is load-bearing). `None` decides "remove
    /// nothing, stamp nothing". `Err` is the refusal: the caller writes nothing.
    pub(crate) fn decide(m: &Manifest, supersede_key: Option<&'k str>) -> Result<Self, DataError> {
        let Some(sk) = supersede_key else {
            return Ok(SupersedeStep { remove: Vec::new(), stamp: None });
        };
        let remove = match plan_supersede(m, sk)? {
            SupersedePlan::NeverSpent => Vec::new(),
            SupersedePlan::ExactMatch(parts) => parts,
        };
        // Stamped for BOTH plans — the pre-spend; see `stamp`'s doc.
        Ok(SupersedeStep { remove, stamp: Some(sk) })
    }

    /// Fold the removal into `frame` — the frame this commit's seal returned, or an empty one when
    /// nothing was sealed — so ONE publish carries both the add and the remove, then publish it,
    /// only if it carries anything (an empty settled batch with nothing to supersede either writes
    /// nothing). A replayed record always seals a part, so for the replay this always publishes.
    pub(crate) fn publish(
        self,
        series_dir: &Path,
        m: &mut Manifest,
        mut frame: DeltaFrame,
        fold_bytes: u64,
    ) -> Result<Published, DataError> {
        let to_unlink = if self.remove.is_empty() {
            Vec::new()
        } else {
            apply_supersede(series_dir, m, &mut frame, &self.remove)
        };
        let published = !frame.files_add.is_empty() || !frame.files_rm.is_empty();
        if published {
            publish(series_dir, m, frame, Durability::Fsync, fold_bytes)?;
        }
        Ok(Published { published, to_unlink })
    }
}

/// Unlink parts whose removal has ALREADY published, best effort: a failure leaves an inert orphan
/// the read path never opens (the manifest no longer names it), which the repair tool's containment
/// rule recognizes because the part that superseded it carries the stamp.
pub(crate) fn unlink_superseded(paths: Vec<PathBuf>) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}
