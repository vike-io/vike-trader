//! Store maintenance for [`DataFusionHist`]: retention (drop parts older than the policy cutoff) and
//! the one-shot pass over the WHOLE store that compacts and prunes every series.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. `run_maintenance` is the unit
//! [`crate::MaintenanceScheduler`] runs on a timer; each series goes through `compact_dir_inner`
//! (`super::compaction`) and `apply_retention_at` under that series' own lock, so a pass is safe
//! beside live appends. `ranks_for_series` is the strict guard on the store's source-precedence
//! policy (`super::sources`) and has no other caller.

use std::collections::BTreeSet;
use std::path::Path;

use crate::store::hist::DataError;
use crate::store::hist_maint::{
    Durability, MaintenanceConfig, MaintenanceReport, PruneReport, RetentionPolicy,
    SeriesMaintenance, SourceRankPolicy,
};
use crate::store::series::SeriesId;

use super::delta::DeltaFrame;
use super::manifest::{self, SeriesLock, publish, read_manifest};
use super::sources;
use super::{DataFusionHist, part_dir};

impl DataFusionHist {
    /// Drop parts older than the policy cutoff (`ts_max < cutoff`): rewrite the manifest (atomic),
    /// unlink the dropped parts, remove emptied `date=` dirs, and GC their commit keys IF no
    /// surviving part still references them (so a later re-backfill of a pruned window appends
    /// rather than being a false no-op).
    pub fn apply_retention(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        policy: &RetentionPolicy,
    ) -> Result<PruneReport, DataError> {
        self.apply_retention_at(&self.series_dir(kind, venue, symbol, interval, None), policy)
    }

    /// [`Self::apply_retention`] over an explicit series DIR, so a GROUPED series (`group=…`, which
    /// has no `symbol=` segment) is pruned too. Without this, maintenance rebuilt the path from
    /// `(kind, venue, symbol, interval)` and a grouped series simply never matched.
    pub fn apply_retention_at(
        &self,
        series_dir: &Path,
        policy: &RetentionPolicy,
    ) -> Result<PruneReport, DataError> {
        let series_dir = series_dir.to_path_buf();
        let mut report = PruneReport::default();
        if !series_dir.exists() {
            return Ok(report);
        }
        let now = vike_model::now_ms();
        let cutoff = match policy.cutoff(now) {
            Some(c) => c,
            None => return Ok(report), // no policy → no-op
        };
        let _guard = SeriesLock::acquire(&series_dir)?;
        let mut m = read_manifest(&series_dir)?;

        let (dropped, kept): (Vec<manifest::FileEntry>, Vec<manifest::FileEntry>) =
            m.files.into_iter().partition(|f| f.ts_max < cutoff);
        m.files = kept;
        if dropped.is_empty() {
            return Ok(report);
        }
        // ⚠ The commit-key GC that used to live here is DELETED, not moved. It re-read every
        // surviving part's key list for every dropped key and then rewrote `m.commits` — O(total
        // keys) per dropped key, inside this lock. `docs/decisions/0060-…` measured what that meant
        // on the live box: pruning half of 659,404 keys is on the order of 10^11 string
        // comparisons with the recorder spinning on the series lock, which is why "just set
        // retention_days" was the one operator action the record told you not to reach for.
        //
        // v3 needs none of it. A key lives on the part that holds its rows, so dropping the part
        // drops the key — and the subtlety the old loop existed for (a spanning-day batch or a
        // compaction output keeping a key alive on a file that SURVIVES the prune) is handled by
        // construction, because `Manifest::has_commit` asks the surviving files.
        m.version += 1;
        let version = m.version;
        publish(
            &series_dir,
            &mut m,
            DeltaFrame {
                version,
                files_rm: dropped.iter().map(|f| (f.name.clone(), f.date.clone())).collect(),
                ..Default::default()
            },
            Durability::Fsync,
            self.fold_bytes(),
        )?;
        // unlink dropped parts (manifest-first)
        for f in &dropped {
            let _ = std::fs::remove_file(part_dir(&series_dir, &f.date).join(&f.name));
            report.files_dropped += 1;
            report.rows_dropped += f.rows;
        }
        // remove date= dirs with no surviving parts
        let surviving: BTreeSet<&String> = m.files.iter().map(|f| &f.date).collect();
        let dropped_dates: BTreeSet<&String> = dropped.iter().map(|f| &f.date).collect();
        for date in dropped_dates {
            if !surviving.contains(date) {
                let _ = std::fs::remove_dir_all(part_dir(&series_dir, date));
                report.dates_dropped += 1;
            }
        }
        Ok(report)
    }

    /// One-shot maintenance pass over the WHOLE store: [`list_series`](Self::list_series), then per
    /// series `compact_series` and — if `cfg.retention` is set — `apply_retention`, aggregating every
    /// per-series report into a [`MaintenanceReport`]. This is the unit the
    /// [`crate::MaintenanceScheduler`] runs on a timer.
    ///
    /// LAYERING: each series is maintained through `compact_series` / `apply_retention`, which take
    /// that series' own lock (the manifest read-modify-write barrier). So a pass is safe to run
    /// alongside live appends — a racing append just serializes on the same per-series lock and loses
    /// no rows (the `concurrent_append_and_compact` invariant). Series are visited sequentially, so a
    /// pass never overlaps itself.
    pub fn run_maintenance(&self, cfg: &MaintenanceConfig) -> Result<MaintenanceReport, DataError> {
        let mut report = MaintenanceReport::default();
        // The STORE's own source-precedence rule, if the operator wrote one. Absent (the default) ⇒
        // `None` ⇒ every series compacts exactly as before, byte-identical. An INERT policy (empty
        // prefix list) would supersede nothing, so it is dropped here rather than paying a rewrite
        // for a guaranteed no-op. See `sources`' module doc for why automating a row-DROPPING pass
        // is only defensible under these guards.
        let policy = sources::load_policy(&self.root)?.filter(|p| !p.is_inert());
        for series in self.list_series()? {
            // Per-series work is ISOLATED (the closure + `match` below, NOT `?`): one broken series
            // must not stop the store's other series from being compacted and pruned. Before this,
            // three `?`s here aborted the whole pass, so a single persistently-failing series
            // silently disabled maintenance STORE-WIDE — and `MaintenanceScheduler` swallowed the
            // error without logging, so the only visible symptom was parts piling up forever.
            let one = || -> Result<SeriesMaintenance, DataError> {
                // Resolve the DIR from the id rather than rebuilding it from (kind, venue, symbol,
                // interval): a GROUPED series has no `symbol=` segment, so the rebuilt path would
                // never match and it would be silently skipped — never compacted, never pruned.
                let dir = self.series_dir_of(&series);
                let ranks = self.ranks_for_series(&dir, &series, policy.as_ref())?;
                // `group.is_some()` is how every consumer tells the two layouts apart, and the same
                // fact `series_dir_of` just resolved the directory from.
                let compaction = self.compact_dir_inner(
                    &dir,
                    &series.kind,
                    series.group.is_some(),
                    &cfg.compaction,
                    ranks.as_ref(),
                )?;
                let retention = match &cfg.retention {
                    Some(policy) => self.apply_retention_at(&dir, policy)?,
                    None => PruneReport::default(),
                };
                Ok(SeriesMaintenance { series: series.clone(), compaction, retention })
            };
            // `catch_unwind`, not just `?`-isolation: a PANIC in the merge is not an `Err` and used
            // to unwind out of the whole maintenance thread, ending compaction and retention for the
            // rest of the PROCESS — the store looked idle, not broken, and only a restart revived
            // it. That is exactly how the arrow `offset overflow` panic (see `COMPACT_BATCH_ROWS`)
            // presented on the CI box. A panicking series is now one skipped series, like any other
            // failure. `AssertUnwindSafe` is sound here: every mutation is behind the series' file
            // lock, whose guard releases on unwind, and `report` is only touched on the Ok path.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(one))
                .unwrap_or_else(|p| {
                    let what = p
                        .downcast_ref::<&str>()
                        .map(|s| (*s).to_string())
                        .or_else(|| p.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    Err(DataError::Query(format!("maintenance PANICKED: {what}")))
                });
            match outcome {
                Ok(entry) => report.absorb(entry),
                Err(e) => {
                    tracing::warn!(
                        kind = %series.kind,
                        venue = %series.venue,
                        symbol = %series.symbol,
                        error = %e,
                        "maintenance: series FAILED — skipped, pass continues"
                    );
                    report.failed.push((series, e.to_string()));
                }
            }
        }
        if !report.failed.is_empty() {
            tracing::warn!(
                failed = report.failed.len(),
                visited = report.series_visited(),
                "maintenance: pass completed with per-series failures"
            );
        }
        Ok(report)
    }

    /// The supersession ranks to compact ONE series under, honoring the policy's strict guard.
    ///
    /// `None` (no policy, or the guard tripped) ⇒ the duplicate-PRESERVING default compaction, which
    /// is byte-identical to a store with no policy at all.
    ///
    /// **The guard**: `SourceRankPolicy::rank_of` gives a commit key matching no listed prefix the
    /// LOWEST precedence, so a series holding a writer the policy never mentions would have that
    /// writer's rows superseded away. Dropping rows from an unranked source, silently and
    /// irreversibly, on a background timer, is not a thing to do — so a strict policy SKIPS such a
    /// series and names the keys it could not rank.
    fn ranks_for_series(
        &self,
        dir: &Path,
        series: &SeriesId,
        policy: Option<&sources::StoreSourcePolicy>,
    ) -> Result<Option<SourceRankPolicy>, DataError> {
        let Some(p) = policy else { return Ok(None) };
        if !p.strict {
            return Ok(Some(p.rank_policy()));
        }
        let m = read_manifest(dir)?;
        let unranked =
            p.unranked(m.files.iter().flat_map(|f| f.commit_keys.iter().map(String::as_str)));
        if unranked.is_empty() {
            return Ok(Some(p.rank_policy()));
        }
        tracing::warn!(
            kind = %series.kind,
            venue = %series.venue,
            series = %series.label(),
            unranked = ?unranked,
            "maintenance: the store's source policy does not rank every writer in this series — \
             superseding SKIPPED for it (running would drop their rows as lowest-precedence). Add \
             the prefix to _sources.json, or set strict=false to accept that."
        );
        Ok(None)
    }
}
