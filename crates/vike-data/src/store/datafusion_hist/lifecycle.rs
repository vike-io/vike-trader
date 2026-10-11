//! Whole-series operations on [`DataFusionHist`]: delete a series outright.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. `delete_series_checked` removes
//! a series under the series lock with its provenance assertion re-checked at that instant;
//! `delete_series` is its no-assertion form.

use std::io::ErrorKind;

use crate::store::hist::DataError;
use crate::store::series::SeriesId;

use super::DataFusionHist;
use super::manifest::{SeriesLock, read_manifest, remove_series_contents};

impl DataFusionHist {
    /// Delete an entire stored series — its `kind=/venue=/symbol=[/interval=]` leaf dir with the
    /// manifest, commit-log, and every Parquet part. **Irreversible.** Removes ONLY that one
    /// series' dir; siblings are untouched. Idempotent: an absent series is `Ok(())`. Powers the
    /// Data Manager's per-series Delete action (the GUI confirms first).
    ///
    /// Takes the series lock — see [`Self::delete_series_checked`], of which this is the
    /// no-assertion form.
    pub fn delete_series(&self, id: &SeriesId) -> Result<(), DataError> {
        self.delete_series_checked(id, None)
    }

    /// [`Self::delete_series`], optionally asserting that EVERY commit key the series records
    /// carries `require_produced_by` — and doing so **under the series lock, immediately before the
    /// removal**.
    ///
    /// # ⚠ Why the lock, and why it cannot simply wrap the removal
    ///
    /// Until 2026-09-07 this was the ONE mutating verb on this type that took no
    /// [`SeriesLock`]: every append (`commit_rows`, the bulk writer), the compaction publish,
    /// `apply_retention_at`, WAL `recover_series` and `rebuild_series_manifest` take it. That was
    /// survivable while the only caller was a GUI on a developer's box; it is not survivable for a
    /// cleanup run against the box that is RECORDING, which is the case this verb now serves.
    ///
    /// The naive fix does not work, and the reason is structural rather than a platform quirk:
    /// `_manifest.lock` lives INSIDE the leaf, so a guard held across `remove_dir_all` is deleting
    /// the file it holds — and on Windows `remove_dir_all` fails outright while any file in the
    /// tree is open, which is a configuration this crate genuinely runs in (the Data Manager).
    /// Dropping the guard first reopens the race it was taken for.
    ///
    /// So: **lock, verify, remove the CONTENTS, release, remove the leaf.** Under the guard this
    /// re-reads the manifest, re-checks the assertion, and deletes every `date=` partition and
    /// `_manifest.json` — everything but the lock file itself. The guard is then dropped (closing
    /// the descriptor) and the now-nearly-empty leaf is removed.
    ///
    /// A crash between the two halves leaves a leaf holding only `_manifest.lock`. That is
    /// RECOVERABLE and INVISIBLE: `read_manifest` answers `Manifest::empty()` for a missing file so
    /// every reader sees an empty series, `find_manifest_series_dirs` skips a leaf with no manifest
    /// so `list_series` does not even report it, and re-running finishes the job (this verb stays
    /// idempotent).
    ///
    /// # ⚠ The assertion is re-checked HERE, not only at plan time
    ///
    /// A live recorder can commit a new key between a plan and its execution, and a provenance
    /// check that only ran at plan time would be a TOCTOU on the one property the whole verb turns
    /// on. `require_produced_by` is therefore evaluated inside the critical section, against the
    /// manifest as it is at that instant — and a series whose commit log is EMPTY cannot satisfy an
    /// assertion, so it is refused rather than passed (see [`Self::series_commits`]).
    pub fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        let dir = self.series_dir_of(id);
        // Probed BEFORE the lock, deliberately: `SeriesLock::acquire` does a `create_dir_all`, so
        // locking an absent series would CREATE the leaf this call is supposed to find missing —
        // turning the idempotent no-op into a phantom-series writer.
        if !dir.exists() {
            return Ok(());
        }
        {
            let _guard = SeriesLock::acquire(&dir)?;
            if let Some(prefix) = require_produced_by {
                let commits = read_manifest(&dir)?.commit_keys();
                if commits.is_empty() {
                    return Err(DataError::Query(format!(
                        "refusing to delete {}: it records NO commit keys, so it cannot satisfy \
                         --produced-by {prefix:?} (a keyless append, or parts sealed before the \
                         commit-key metadata existed)",
                        dir.display()
                    )));
                }
                if let Some(foreign) = commits
                    .iter()
                    .find(|k| !crate::store::store_kind::key_matches_prefix(k, prefix))
                {
                    return Err(DataError::Query(format!(
                        "refusing to delete {}: its commit key {foreign:?} does not carry \
                         --produced-by {prefix:?}. Nothing was deleted.",
                        dir.display()
                    )));
                }
            }
            remove_series_contents(&dir)?;
        }
        // The guard is dropped, so the only descriptor this process held on the tree is closed and
        // `_manifest.lock` — the one file `remove_series_contents` leaves — can go with the leaf.
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(ref e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(DataError::Query(format!("delete series {}: {e}", dir.display()))),
        }
    }
}
