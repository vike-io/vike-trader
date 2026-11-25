//! The store-tree walk: find every series leaf, and parse a leaf path back into its `SeriesId`; and
//! [`SeriesWalkMemo`], the last walk's answer, kept while nothing the walk read has moved.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use vike_data::store::hist::HistoryStoreError;
use vike_data::store::series::SeriesId;
use vike_model::StoreKindId;

use super::io;

/// **`HistoryStoreDataFusion::list_series`' memo: the last walk's answer, kept while its key holds.**
///
/// The walk is 30.8 ms of the datahub's CPU per `ListSeries` on the CI box (2026-11-19: 2,230 folders opened and 2,020
/// manifests stat-ed, under the unit's seccomp filter), and every backtest of a USDT symbol on the dollar ledger asks
/// it. Its answer depends on two things only, so they are the key, read from the filesystem and never from a writer:
/// a writer in any process (this one, the store tool, a backfill collector, a hand edit) moves it.
///
/// - **Each folder the walk LISTED** (the root and every folder above a leaf), with its modification time, read
///   before its listing. A series created, deleted or renamed adds or removes an entry in one of them: a write's
///   `create_dir_all` in the nearest folder that existed, a first manifest published into a folder that had none,
///   `delete_series`' `remove_dir_all` in the leaf's parent, a rename both. An entry added or removed moves its
///   folder's time.
/// - **Each leaf's manifest**, which must still be a file. The walk never lists a leaf, so a leaf that loses its
///   `_manifest.json` and stays (a rename that empties a series around a nested folder, a delete stopped half way, a
///   hand removal) is caught by this stat alone.
///
/// A write INSIDE a leaf (a part, a new `date=` folder, a fold of the manifest) moves neither and changes no answer.
/// ⚠ A time keys the answer only when it is [`SETTLED`] old at the walk's start: a filesystem stamps a folder from a
/// coarse clock, so a second change inside the tick of the first keeps the time the walk read. A walk that read a
/// younger time is answered and NOT kept, so the next ask walks again.
///
/// shortcut: a permission change alone (no entry added or removed) keeps a folder's time, so the memo answers the
/// last listing where a walk would now fail; a time put back by hand to exactly the nanosecond it had is not seen
/// either. Key on the change time (`ctime`, unix only) if either is ever met.
///
/// Bounded: ONE walk's answer (every series id) and its key (one path per listed folder and per leaf), about 0.5 MB
/// for the CI box's 2,020 series.
#[derive(Default)]
pub(crate) struct SeriesWalkMemo {
    held: Mutex<Option<Arc<Walked>>>,
    /// Walks taken: what tells a hit from a walk (the tests read it).
    pub(crate) walks: AtomicU64,
}

/// How old a folder's time must be at a walk's start to key the answer: above the tick of a local filesystem's clock
/// (a jiffy, at most 10 ms) and of one that stamps whole seconds.
pub(crate) const SETTLED: Duration = Duration::from_secs(2);

/// One walk's answer and its key ([`SeriesWalkMemo`]).
struct Walked {
    key: WalkKey,
    series: Vec<SeriesId>,
}

/// What one walk read, for [`SeriesWalkMemo`]'s key.
#[derive(Default)]
pub(crate) struct WalkKey {
    listed: Vec<(PathBuf, SystemTime)>,
    manifests: Vec<PathBuf>,
    /// A listed folder's time could not be read: no key holds this walk.
    unkeyed: bool,
}

impl WalkKey {
    /// Every listed folder still has the time it was listed at, and every leaf's manifest is still a file.
    fn holds(&self) -> bool {
        self.listed.iter().all(|(dir, at)| {
            std::fs::metadata(dir).and_then(|m| m.modified()).is_ok_and(|t| t == *at)
        }) && self.manifests.iter().all(|m| is_manifest(m))
    }
}

impl SeriesWalkMemo {
    /// The held answer while its key holds, else `walk`'s (over `root`, filling the key it is handed), kept when its
    /// key can be.
    pub(crate) fn list(
        &self,
        root: &Path,
        walk: impl FnOnce(&mut WalkKey) -> Result<Vec<SeriesId>, HistoryStoreError>,
    ) -> Result<Vec<SeriesId>, HistoryStoreError> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(walked) = held.filter(|w| w.key.holds()) {
            return Ok(walked.series.clone());
        }
        let started = SystemTime::now();
        let mut key = WalkKey::default();
        let series = walk(&mut key)?;
        self.walks.fetch_add(1, Ordering::Relaxed);
        let settled = |at: &SystemTime| started.duration_since(*at).is_ok_and(|age| age >= SETTLED);
        // The root itself was listed: an absent root, or one that is a leaf, is never keyed.
        let keyed = !key.unkeyed
            && key.listed.first().is_some_and(|(dir, _)| dir == root)
            && key.listed.iter().all(|(_, at)| settled(at));
        let kept = keyed.then(|| Arc::new(Walked { key, series: series.clone() }));
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = kept;
        Ok(series)
    }
}

/// `path` is a manifest the walk counts: present and not a folder. `symlink_metadata` + `!is_dir()` is exactly the
/// listing's test in [`find_manifest_series_dirs`] (`DirEntry::file_type` follows no link).
fn is_manifest(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir())
}

/// Recursively collect series leaf dirs (those directly containing a `_manifest.json`) under `dir`.
/// A manifest marks a series leaf, whose only children are `date=` part dirs — so recursion stops
/// there (no nested series). Drives [`HistoryStoreDataFusion::list_series`], which every backtest of a USDT
/// symbol on the dollar ledger asks (`crates/vike-backtest/src/run/data_plan/conversion.rs`'s `load_conversions`).
///
/// ⚠ **A leaf is told by ONE stat of its manifest, never by listing it**: its `date=` part dirs are ~99% of the
/// tree, so listing them made the walk the cost of the answer (the CI box, 2026-11-19: 2,020 leaves, 223,079 entries
/// listed per walk, 2,230 with the stat). Every folder is still OPENED first, so a leaf that cannot be listed
/// fails the walk as it always did (an unreadable store is an error); only the listing of a leaf is skipped.
/// A failed stat (absent, or unreadable) falls through to the listing.
///
/// `key` gets what [`SeriesWalkMemo`] keys the answer on: each listed folder's time, read BEFORE its listing (an entry
/// added or removed after it moves the time), and each leaf's manifest.
pub(crate) fn find_manifest_series_dirs(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    key: &mut WalkKey,
) -> Result<(), HistoryStoreError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
    let manifest = dir.join(super::manifest::MANIFEST);
    if is_manifest(&manifest) {
        out.push(dir.to_path_buf());
        key.manifests.push(manifest);
        return Ok(());
    }
    match std::fs::metadata(dir).and_then(|m| m.modified()) {
        Ok(at) => key.listed.push((dir.to_path_buf(), at)),
        Err(_) => key.unkeyed = true,
    }
    let mut has_manifest = false;
    let mut subdirs = Vec::new();
    for entry in rd {
        let entry = entry.map_err(io)?;
        let ft = entry.file_type().map_err(io)?;
        if ft.is_dir() {
            subdirs.push(entry.path());
        } else if entry.path().file_name().and_then(|n| n.to_str()) == Some("_manifest.json") {
            has_manifest = true;
        }
    }
    if has_manifest {
        out.push(dir.to_path_buf());
        key.manifests.push(manifest);
        return Ok(()); // series leaf: children are date= part dirs, never nested series
    }
    for sub in subdirs {
        find_manifest_series_dirs(&sub, out, key)?;
    }
    Ok(())
}

/// Parse a series leaf dir back into its [`SeriesId`] by reading the `key=value` path segments below
/// `root`: `kind=…/venue=…[/source=…]/symbol=…[/interval=…]`. Returns `None` if `dir` isn't under `root`, if any
/// segment isn't one of the expected keys (so a stray dir can't masquerade as a series), or if the
/// `symbol=` segment is EMPTY (see the arm below — that value is the grouped-series sentinel, not a
/// symbol). The inverse of [`HistoryStoreDataFusion::series_dir`].
pub(crate) fn parse_series_id(root: &Path, dir: &Path) -> Option<SeriesId> {
    let rel = dir.strip_prefix(root).ok()?;
    let (mut kind, mut venue, mut symbol, mut interval, mut group, mut source) =
        (None, None, None, None, None, None);
    for comp in rel.components() {
        let seg = comp.as_os_str().to_str()?;
        if let Some(v) = seg.strip_prefix("kind=") {
            // The kind set is CLOSED (`StoreKindId`), so a `kind=` segment no row declares — a leaf
            // written by a NEWER build, or by hand — is SKIPPED, loudly, for the empty-`symbol=` arm's
            // reason below: one unreadable directory must not take `list_series` down for the whole
            // store. This build has no codec for such a leaf, so it could neither compact nor read it;
            // it stays on disk untouched.
            match v.parse::<StoreKindId>() {
                Ok(k) => kind = Some(k),
                Err(e) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        "list_series: SKIPPING a leaf whose `kind=` segment this build does not \
                         know ({e}). It is left on disk untouched; a build that declares the kind \
                         lists it."
                    );
                    return None;
                }
            }
        } else if let Some(v) = seg.strip_prefix("venue=") {
            venue = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("symbol=") {
            // An EMPTY `symbol=` value is REFUSED — the READ-side twin of `append_exec_orders`' and
            // `append_book_updates_grouped`'s writer guards, and general where those are per-producer:
            // a leaf can acquire this shape from any producer, a partial write or a hand-edit.
            //
            // `crates/vike-data/src/store/series.rs`'s `SeriesId::group` RESERVES an empty `symbol` as the
            // GROUPED-series sentinel ("exactly one of `symbol`/`group` is meaningful for any given
            // series"), so accepting it here minted a `SeriesId { symbol: "", group: None }` that is
            // NEITHER: no `scan_*(venue, symbol)` names it, [`SeriesId::label`] renders it as a BLANK
            // node in the Data Manager, `vike_studio_core::run`'s `bar_series`/`tick_series` offer it
            // as a runnable `(venue, "")` slice that scans nothing, and it rides the datahub wire to
            // remote clients in that state. The sentinel and the parser disagreed about the same
            // value and nothing noticed.
            //
            // `None` — the answer this function already gives an unexpected segment — rather than a
            // new error type: it has ONE caller (`list_series`'s `filter_map`), and a hard `Err`
            // there would let one malformed directory disable `list_series` for the WHOLE store,
            // taking `run_maintenance` and the Data Manager down with it. That is the opposite of
            // `run_maintenance`'s own rule that one broken series is one SKIPPED series.
            //
            // NOT re-read as GROUPED, which would be affirmatively wrong rather than merely
            // generous: the inverse `HistoryStoreDataFusion::series_dir_of` would then rebuild `group=` — a
            // DIFFERENT, non-existent directory — so coverage/gaps/rebuild/compaction/retention/
            // delete would each silently operate on a phantom (the exact five-bug class
            // `series_dir_of`'s doc records), and the grouped read path tells rows apart by a
            // `symbol_col` column that a per-symbol part does not carry.
            //
            // Only `symbol`, mirroring #1307: an empty `venue=` collides with no sentinel (an empty
            // `kind=` names no declared kind and is refused by the `kind=` arm), and an empty
            // `group=` still parses as GROUPED — degenerate-looking but UNAMBIGUOUS, since
            // `group.is_some()` is what every consumer tells the two layouts apart by.
            //
            // The `warn!` is load-bearing, not decoration. Skipping is a real behaviour change for
            // a leaf that already holds rows: `run_maintenance` stops compacting and
            // retention-pruning it, and the reconcile pre-seed — which iterates `list_series()` and
            // scans each `kind=exec_fill` series by `id.symbol` — stops folding its trade ids into
            // the seen-fill dedup set. That consumer is the sentinel doc's own "silently scanning
            // `\"\"`" case, so refusing is right; doing it QUIETLY would swap one silence for
            // another. (⚠ The pre-seed named here was `vike-app`'s and went with the GUI's local
            // core; the same walk is `crates/vike-tradehub-core/src/journal_view.rs`'s
            // `journal_view_from_store` today, which no production root reaches yet.)
            if v.is_empty() {
                tracing::warn!(
                    dir = %dir.display(),
                    "list_series: SKIPPING a leaf whose `symbol=` segment is EMPTY — that value is \
                     the grouped-series sentinel, so the leaf names neither a symbol nor a group and \
                     no scan can address it. Nothing writes this shape today; move the rows under a \
                     real symbol, or delete the directory."
                );
                return None;
            }
            symbol = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("interval=") {
            interval = Some(v.to_string());
        } else if let Some(v) = seg.strip_prefix("source=") {
            // ⚠ **THE ONE-WAY DOOR, and this arm is the door.** Before it existed this segment fell
            // into the `else` below, failed the `group=` strip and returned `None` for the WHOLE
            // path — so a build that predates this line, run against a store holding `source=`
            // leaves, silently omits them from `list_series`, and therefore from `inventory`, from
            // the Data Manager and from `run_maintenance`. Never compacted, never retention-pruned,
            // no error anywhere. That is not a bug to fix later: it is a property of every binary
            // already released, and no marker file helps because an old build does not read one.
            //
            // The owner ACCEPTED it on 2026-09-22 (the design's §6.3 and the Status block above
            // it), and the mitigation is SEQUENCING rather than code: this arm ships and is
            // RELEASED to every box BEFORE any producer is scoped, so the window between "a build
            // can read a sourced leaf" and "a store contains one" is as wide as the owner wants.
            // Its blast radius is bounded to series a NEW build created for a lane an OLD build
            // never knew about — legacy sourceless data stays fully visible to both, which is the
            // second argument for absence-as-a-value.
            //
            // ⚠ This parser is ORDER-AGNOSTIC (it matches prefixes in whatever order the path
            // yields them), so it does NOT enforce that `source=` sits under `venue=` and above
            // `symbol=`/`group=`. The BUILDERS decide that, and
            // `crates/vike-store/src/datafusion_hist.rs`'s `series_dir` carries why the position is
            // forced; `crates/vike-store/tests/store/source_dimension.rs`'s
            // `the_source_segment_sits_directly_under_venue` is the pin.
            source = Some(v.to_string());
        } else {
            // Not `group=` either, so this segment is unexpected and the leaf is not a
            // well-formed series — the `?` returns None for the whole path, exactly as the
            // explicit `return None` here did before 1.97's `question_mark` lint asked for it.
            let v = seg.strip_prefix("group=")?;
            group = Some(v.to_string());
        }
    }
    // A GROUPED leaf has no `symbol=` segment — it holds many symbols, told apart by the row-level
    // symbol column. Before this arm existed, `parse_series_id` returned None for such a dir, so
    // `list_series` silently SKIPPED grouped series and `run_maintenance` never compacted or
    // retention-pruned them. Latent rather than live (nothing wrote grouped series yet), and
    // exactly the kind of silent omission that only shows up as unbounded disk growth much later.
    if let Some(g) = group {
        let id = SeriesId::grouped(kind?, venue?, g);
        return Some(match source {
            Some(s) => id.with_source(s),
            None => id,
        });
    }
    Some(SeriesId { kind: kind?, venue: venue?, symbol: symbol?, interval, group: None, source })
}

#[path = "series_walk_tests.rs"]
#[cfg(test)]
mod series_walk_tests;
