//! Series INVENTORY for [`DataFusionHist`]: what the store holds, answered from the manifests alone.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. Every method here reads a
//! manifest (or walks the tree for manifests) and NEVER a Parquet part: `list_series`, the
//! coverage folds (`series_coverage`, `series_facts`, `inventory`, `coverage_report`), the gap view
//! (`series_gaps`) and the commit-key accessors (`series_commits`, `series_has_commit`,
//! `series_has_commits`). All are lock-free and cheap enough to call per series in a loop. The
//! shared fold itself (`coverage_of`) and the path parser (`parse_series_id`) stay in the parent,
//! where their test modules live.

use std::collections::BTreeSet;

use crate::hist::DataError;
use crate::series::{SeriesCoverage, SeriesId};

use super::gaps::{day_gap_to_ms_range, parse_utc_date};
use super::manifest::read_manifest;
use super::{DataFusionHist, coverage_of, find_manifest_series_dirs, parse_series_id};

impl DataFusionHist {
    /// Enumerate every series in the store: walk the root tree for leaf dirs holding a
    /// `_manifest.json`, and parse each one's `(kind, venue, symbol, interval)` straight back out of
    /// its `kind=…/venue=…/symbol=…[/interval=…]` path segments. Returned sorted (deterministic order
    /// for reproducible maintenance + stable test assertions).
    ///
    /// This is a MAINTENANCE walk (feeds [`Self::run_maintenance`]), NOT the hot read path — a
    /// directory scan is fine here, exactly like WAL [`recovery`](super::wal). A leaf mid-creation
    /// (lock taken, manifest not yet written) is simply skipped this sweep and picked up the next.
    pub fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        let mut dirs = Vec::new();
        find_manifest_series_dirs(&self.root, &mut dirs)?;
        let mut out: Vec<SeriesId> =
            dirs.iter().filter_map(|d| parse_series_id(&self.root, d)).collect();
        out.sort();
        Ok(out)
    }

    /// Coverage for one series (manifest fold + per-part fs size). Empty series → all-zero.
    /// NO DataFusion scan — cheap enough to call per-series in a loop ([`Self::inventory`]).
    pub fn series_coverage(&self, id: &SeriesId) -> Result<SeriesCoverage, DataError> {
        let dir = self.series_dir_of(id);
        // ONE fold, shared with [`Self::series_facts`] — see [`coverage_of`] for why it is a free
        // function rather than two copies of the same arithmetic.
        Ok(coverage_of(&dir, &read_manifest(&dir)?)) // read_manifest is private, same module
    }

    /// One series' COVERAGE **and** its INGEST COMMIT KEYS, from **ONE** manifest parse.
    ///
    /// ⚠ **This exists because the two questions have one answer and asking them separately parsed
    /// it twice.** [`Self::series_coverage`] and [`Self::series_commits`] each call the private
    /// `read_manifest` — so a caller that wants both paid two full parses of the same file, which
    /// on a grouped series is a multi-megabyte JSON document. `crates/vike-backtest/src/
    /// backtest_cli.rs`'s `collect_data_fingerprint` is that caller, its own doc named this method
    /// as the missing piece ("merging the two answers needs a `DataFusionHist` method that returns
    /// coverage and commits from ONE parse, which is that crate's edit rather than this one's"),
    /// and it is what lets a parameter SEARCH address its inputs for the price of the data witness
    /// it was already paying for.
    ///
    /// The two halves are byte-for-byte what the single-question accessors answer — they share this
    /// method's fold and its `read_manifest` call — so nothing that reads one of them can disagree
    /// with a caller that reads both. It is NOT a cheaper coverage: the per-part `fs::metadata`
    /// stat that fills [`SeriesCoverage::bytes`] happens here exactly as it does there.
    pub fn series_facts(&self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), DataError> {
        let dir = self.series_dir_of(id);
        let m = read_manifest(&dir)?;
        Ok((coverage_of(&dir, &m), m.commit_keys()))
    }

    /// One series' INGEST COMMIT KEYS — the manifest's commit log, verbatim.
    ///
    /// ⚠ **The ORDER changed at manifest v3 and this doc used to promise "the order it records
    /// them".** v2 kept a separate top-level array appended in commit order; v3 stores each key on
    /// the part that holds its rows, so the answer is now `Manifest::orphan_commits` (a migrated
    /// store's residue) followed by `files` order. For an append-only series those are the same
    /// sequence; after a compaction they are not, because a merge output joins at the end of
    /// `files` carrying the union of its inputs' keys. Duplicates are collapsed — a batch
    /// straddling a UTC midnight stamps its key on both of its parts. Nothing in this tree depends
    /// on the order (the provenance assertion below is a membership test), which is why the
    /// weakening is stated rather than worked around.
    ///
    /// This is the store's only record of WHO WROTE a series, and until this accessor existed it
    /// was unreadable from outside this module: `Manifest`, `FileEntry` and `read_manifest` are all
    /// `pub(super)`. Deleting by a CHECKED PROPERTY rather than by name needs it —
    /// `crate::removal`'s plan reports it and `--produced-by` asserts over it — and so does anyone
    /// answering "which collector put these rows here", which the 2026-09-07 the CI box cleanup had to
    /// answer with a throwaway script reading the JSON by hand.
    ///
    /// ⚠ **An EMPTY vector is a real state, not an error.** A keyless append (`commit_key: None`)
    /// records nothing, and parts sealed before the commit-key metadata existed carry nothing to
    /// rebuild from either (`crate::datafusion_hist::manifest`'s `rebuild_manifest` reports the same
    /// fact as `parts_without_keys`). Such a series can satisfy NO provenance assertion, which is
    /// why the assertion refuses it rather than passing it — see `crate::removal::Provenance`.
    ///
    /// Resolved through [`Self::series_dir_of`] like every other id-taking method, so a grouped
    /// series answers about its `group=` leaf rather than about a phantom `symbol=` one.
    pub fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, DataError> {
        Ok(read_manifest(&self.series_dir_of(id))?.commit_keys())
    }

    /// Whether `commit_key` has already been spent for `id` — the membership-test twin of
    /// [`Self::series_commits`], for a caller that wants to skip work (a network fetch, say) whose
    /// result a repeat of the same key would make the store discard anyway. Same cost and the same
    /// safety as `series_commits`: one lock-free manifest read, and `Ok(false)` rather than an error
    /// for a series with no manifest yet (`read_manifest`'s missing-directory fallback), so a symbol
    /// with no prior write answers `false` with no special-casing at the call site.
    pub fn series_has_commit(&self, id: &SeriesId, commit_key: &str) -> Result<bool, DataError> {
        Ok(read_manifest(&self.series_dir_of(id))?.has_commit(commit_key))
    }

    /// [`Self::series_has_commit`] for SEVERAL keys from ONE manifest read: `out[i]` answers
    /// `keys[i]`.
    ///
    /// It exists for a day-chunked ingest that asks two questions about every day — is the day's own
    /// key spent (the day is stored), and is its EMPTY MARKER spent (the day is known to hold
    /// nothing; see [`crate::store_kind::EMPTY_MARKER_SUFFIX`]). Two calls would read the manifest
    /// twice per day, and a twelve-year request is about 4,400 days. Same safety as the one-key
    /// form: lock-free, and every answer `false` for a series with no manifest yet.
    pub fn series_has_commits(&self, id: &SeriesId, keys: &[&str]) -> Result<Vec<bool>, DataError> {
        let m = read_manifest(&self.series_dir_of(id))?;
        Ok(keys.iter().map(|k| m.has_commit(k)).collect())
    }

    /// Every stored series with its coverage: [`Self::list_series`] + [`Self::series_coverage`] each.
    pub fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        let mut out = Vec::new();
        for id in self.list_series()? {
            let cov = self.series_coverage(&id)?;
            out.push((id, cov));
        }
        Ok(out)
    }

    /// The CROSS-KIND coverage report: every instrument, with `kind=trade`/`kind=quote`/`kind=book`
    /// lined up so a day one kind has and another lacks becomes a single visible row
    /// ([`crate::coverage::InstrumentCoverage::partial_days`]).
    ///
    /// This is what makes a backfill source's limits legible. A Polymarket venue-backfill restores
    /// the trade tape and nothing else — no book history exists to fetch — so per-series BOTH
    /// manifests look unremarkable (contiguous trades; a book series that simply has no rows there),
    /// while the joined view shows a window a market-making backtest would run over with no book at
    /// all.
    ///
    /// Same cost class as [`Self::inventory`]: one manifest read per series, NO Parquet scan, so it
    /// is cheap enough for the Data Manager to call on open.
    pub fn coverage_report(&self) -> Result<Vec<crate::coverage::InstrumentCoverage>, DataError> {
        let mut pairs: Vec<(SeriesId, Vec<i64>)> = Vec::new();
        for id in self.list_series()? {
            if !crate::coverage::TICK_KINDS.contains(&id.kind.as_str()) {
                continue; // don't even read the manifest of a kind the report ignores
            }
            let dir = self.series_dir_of(&id);
            let m = read_manifest(&dir)?;
            let mut days = Vec::with_capacity(m.files.len());
            for f in &m.files {
                days.push(parse_utc_date(&f.date)?);
            }
            pairs.push((id, days));
        }
        Ok(crate::coverage::join_coverage(&pairs))
    }

    /// The GAP ranges (inclusive epoch-ms, same convention as [`SeriesCoverage`]'s
    /// `first_ts`/`last_ts`) missing within `id`'s recorded span — the Data Manager's "where's the
    /// hole" view. Derived purely from the manifest's `date=` file index ([`crate::coverage::find_gaps`] over
    /// the distinct `FileEntry::date`s): NO Parquet scan, so this is as cheap as
    /// [`Self::series_coverage`]. A series with fully contiguous coverage, fewer than two distinct
    /// days on record, or no data at all all return `Ok(vec![])` — this never errors on "no data".
    pub fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        let dir = self.series_dir_of(id);
        let m = read_manifest(&dir)?; // private, same module
        let mut days: BTreeSet<i64> = BTreeSet::new();
        for f in &m.files {
            days.insert(parse_utc_date(&f.date)?);
        }
        let days: Vec<i64> = days.into_iter().collect();
        Ok(crate::coverage::find_gaps(&days, 1).into_iter().map(day_gap_to_ms_range).collect())
    }
}
