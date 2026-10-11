//! Manifest REBUILD for [`DataFusionHist`]: re-derive one series' index from its parts and publish it,
//! and the lock-free plan that says what that rebuild would recover and what it would lose.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. The manifest is a cache, not
//! ground truth, and these are the three verbs that act on that: `rebuild_series_manifest` (spins
//! for the series lock), `rebuild_series_manifest_if_uncontended` (refuses rather than waits) and
//! `plan_series_manifest_rebuild` (takes no lock and writes nothing). The plan's data type and the
//! verdict an operator reads are `super::repair`'s; the pure re-derivation they all share is
//! `manifest::rebuild_manifest`.

use crate::store::hist::DataError;
use crate::store::hist_maint::Durability;
use crate::store::series::SeriesId;

use super::DataFusionHist;
use super::delta;
use super::manifest::{self, RebuildReport, SeriesLock, fold_base, read_manifest};
use super::repair::RepairPlan;

impl DataFusionHist {
    /// Rebuild one series' manifest FROM ITS PARTS and publish it, replacing whatever is there.
    ///
    /// The manifest is a CACHE, not ground truth — this is the operation that proves it. A lost or
    /// corrupt `_manifest.json` used to make every part beneath it unreachable, because the read
    /// path deliberately never LISTs directories (spec must-fix #5): data sitting on disk was simply
    /// invisible, with no way back. Now it is one call.
    ///
    /// Precedent: Parquet-catalog systems re-derive their filename index from
    /// Parquet row-group statistics; ArcticDB documents an explicit fallback to iterating storage
    /// "in case we have consistency issues in the ref keys". Both treat the index as regenerable.
    /// This store now does too.
    ///
    /// Takes the series lock, so it is safe against a concurrent append or compaction. Returns what
    /// was recovered AND what could not be — see [`manifest::RebuildReport`]; in particular, parts
    /// written before commit keys were stamped into footers come back readable but contribute
    /// nothing to the idempotency log, which is reported rather than hidden.
    /// ⚠ **A rebuild PUBLISHES A BASE and clears the delta log**, so it is the one write path that
    /// is still a whole-file manifest write by design — that is what it is for.
    ///
    /// Its version is set ABOVE the manifest it replaces rather than to `rebuild_manifest`'s own
    /// `1`. Two things need that. A crash between the base publish and the log clear would
    /// otherwise leave frames whose version is far higher than the rebuilt base's, and replay would
    /// re-add every file the rebuild had just decided to skip — undoing the repair silently. And
    /// `compact_dir_inner` names merge output from `version + 1`, so a counter that restarted at 1
    /// could name a part a previous pass already wrote. `rebuild_manifest` itself stays pure and
    /// deterministic (its round-trip test compares field for field); the version is applied here.
    ///
    /// ⚠ It also DROPS `Manifest::orphan_commits`, because it re-derives the commit log from part
    /// footers and an orphan by definition has no part. That is not new — v2's rebuild rebuilt
    /// `commits` from the same footers and lost them identically — but it is worth knowing before
    /// rebuilding a migrated series: `RebuildReport::parts_without_keys` is the related signal. The
    /// empty-day markers [`Self::spend_keys_without_rows`] writes are orphans too and are dropped
    /// with them; each costs one request the next time its day is asked for.
    pub fn rebuild_series_manifest(&self, id: &SeriesId) -> Result<RebuildReport, DataError> {
        let dir = self.series_dir_of(id);
        let _guard = SeriesLock::acquire(&dir)?;
        // NOT `read_manifest`: that refuses a base-less series with a surviving log, which is one
        // of the very states this verb repairs. `last_published_version` answers from whichever
        // half is present.
        let previous = manifest::last_published_version(&dir);
        let (mut m, report) = manifest::rebuild_manifest(&dir)?;
        m.version = previous.max(m.version) + 1;
        fold_base(&dir, &m, Durability::Fsync)?;
        Ok(report)
    }

    /// [`Self::rebuild_series_manifest`], but **refusing rather than waiting** when another writer
    /// holds this series' lock: `Ok(None)` means nothing was attempted and nothing was written.
    ///
    /// # Why the operator-facing verb takes this one and not the spinning one
    ///
    /// The spin in [`manifest::SeriesLock::acquire`] is right for a writer that must eventually
    /// write — a loser waits and a holder pays nothing. It is the wrong shape here, and the reason
    /// is the critical SECTION rather than the wait: a rebuild reads EVERY part footer in the
    /// series with the lock held, so on a series with hundreds of parts the hold is long, and
    /// `crate::rec::live_rec`'s `RecorderSink` discards its buffer — up to 5,000 rows — when its own
    /// flush spins that budget out. WINNING a contended lock is therefore the outcome to avoid.
    /// `docs/decisions/0060-…`'s *What would reopen this* names this verb as a second writer on a
    /// hot series; this is the answer to that clause.
    ///
    /// ⚠ **`Ok(None)` is honest in one direction only, and the caller owes its operator the other
    /// half.** A held lock PROVES a writer is mid-commit here. An unheld one proves only that no
    /// commit is in flight at this instant — a recorder holds this lock for the length of a commit
    /// and not between them — so a clean acquire is not evidence that the series is idle. The
    /// refusal catches the collision it can see; the plan
    /// ([`Self::plan_series_manifest_rebuild`]) is what tells the operator how big the window is
    /// that it cannot see.
    ///
    /// Everything else is [`Self::rebuild_series_manifest`] unchanged, including the version bump
    /// and the delta-log clear — see that method for why both are load-bearing.
    ///
    /// ⚠ **An `Err` from here does not mean nothing was written.** The publish and the log clear
    /// are two steps (`manifest::fold_base`), so a failure in the second returns `Err` with a new
    /// base already durable. A caller reporting this must say so rather than printing a bare error.
    pub fn rebuild_series_manifest_if_uncontended(
        &self,
        id: &SeriesId,
    ) -> Result<Option<RebuildReport>, DataError> {
        let dir = self.series_dir_of(id);
        let Some(_guard) = SeriesLock::try_acquire(&dir)? else { return Ok(None) };
        // Identical to `rebuild_series_manifest` below the lock, deliberately: two rebuild bodies
        // would be two places for the version bump to be got wrong.
        let previous = manifest::last_published_version(&dir);
        let (mut m, report) = manifest::rebuild_manifest(&dir)?;
        m.version = previous.max(m.version) + 1;
        fold_base(&dir, &m, Durability::Fsync)?;
        Ok(Some(report))
    }

    /// What [`Self::rebuild_series_manifest_if_uncontended`] WOULD do — computed **lock-free** and
    /// writing nothing at all.
    ///
    /// It runs the same pure `manifest::rebuild_manifest` pass the write runs, so the counts it
    /// reports are the counts the write would report, and reads the current index beside them so an
    /// operator can compare. Nothing here takes the series lock: a rehearsal that stalled a live
    /// recorder in order to say what a rebuild would cost would be the wrong tool for its own
    /// question. `repair::RepairPlan`'s module doc carries the whole argument, including why the
    /// plan/write gap is bounded the same way `crate::store::removal`'s is.
    ///
    /// ⚠ **It does not create the series leaf and must not**, which is why `RepairPlan::leaf_present`
    /// exists: `rebuild_manifest` reads an absent directory as an EMPTY series, so a mistyped
    /// selector reaching the write would publish an empty manifest at a path nothing had ever
    /// written. The caller refuses on that flag.
    pub fn plan_series_manifest_rebuild(&self, id: &SeriesId) -> Result<RepairPlan, DataError> {
        let dir = self.series_dir_of(id);
        let (mut current_parts, mut current_rows, mut current_error) = (None, None, None);
        match read_manifest(&dir) {
            Ok(m) => {
                current_parts = Some(m.files.len());
                current_rows = Some(m.files.iter().map(|f| f.rows as u64).sum());
            }
            // The refusal VERBATIM — usually the exact sentence that sent the operator here, and
            // never re-worded: a repair tool that paraphrases the error it repairs makes the two
            // impossible to match up.
            Err(e) => current_error = Some(e.to_string()),
        }
        let base = manifest::read_base(&dir);
        // Read DIRECTLY rather than through `read_manifest`, which refuses the base-less state this
        // verb repairs — and which would therefore report "no frames" for the one series whose
        // frame count matters most. An `Err` here is the narrow version-guard hole
        // `RepairPlan::delta_error` documents.
        let (delta_frames, delta_error) = match delta::delta_read_with_end(&dir) {
            Ok((frames, _)) => (Some(frames.len()), None),
            Err(e) => (None, Some(e.to_string())),
        };
        // ONE pass: the same call the write makes, and its manifest is what the write would
        // publish. Running it twice would read every footer twice for two halves of one answer.
        let (rebuilt, report) = manifest::rebuild_manifest(&dir)?;
        // Base PLUS log: a key spent without rows lives only in the log until the next fold. The
        // empty markers among the orphans are a NOTE (a dropped one costs one request later); the
        // rest are the LOSS `RepairPlan::orphan_commits` has always counted.
        let (empty_markers, orphans): (Vec<String>, Vec<String>) =
            manifest::folded_orphan_commits(&dir)
                .into_iter()
                .partition(|k| k.ends_with(crate::store::store_kind::EMPTY_MARKER_SUFFIX));
        Ok(RepairPlan {
            id: id.clone(),
            series_dir: dir.display().to_string(),
            leaf_present: dir.is_dir(),
            base_present: dir.join(manifest::MANIFEST).is_file(),
            base_version: base.as_ref().map(|m| m.version).unwrap_or(0),
            current_parts,
            current_rows,
            current_error,
            delta_frames,
            delta_error,
            orphan_commits: orphans.len(),
            empty_markers: empty_markers.len(),
            report,
            rebuilt_rows: rebuilt.files.iter().map(|f| f.rows as u64).sum(),
        })
    }
}
