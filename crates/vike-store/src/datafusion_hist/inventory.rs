//! Series INVENTORY for [`HistoryStoreDataFusion`]: what the store holds, answered from the manifests alone.
//!
//! Split out of `datafusion_hist.rs` by concern, bodies verbatim. Every method here reads a
//! manifest (or walks the tree for manifests) and NEVER a Parquet part: `list_series`, the
//! coverage folds (`series_coverage`, `series_facts`, `inventory`, `coverage_report`), the gap view
//! (`series_gaps`), the commit-key accessors (`series_commits`, `series_has_commit`,
//! `series_has_commits`) and a read's stamped days (`series_days`). None takes a series lock (they hold
//! only their memo's: `CoverageMemo`, and `ManifestMemo`, which every lock-free read of a manifest goes
//! through, the bar and tick reads too, and which the append writers keep current under their lock),
//! and all are cheap enough to call per series in a loop. The
//! shared fold itself (`coverage_of`) and the path parser (`parse_series_id`) stay in the parent,
//! where their test modules live.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use vike_data::store::hist::{BarDay, HistoryStoreError, TimestampRange};
use vike_data::store::series::{SeriesCoverage, SeriesId};
use vike_data::store::store_kind::{
    EMPTY_MARKER_SUFFIX, is_provisional_key, venue_window_key_prefix,
};
use vike_model::MS_PER_DAY;
use vike_model::StoreKindId;

use super::delta::{DELTA, delta_read_with_end};
use super::gaps::{day_gap_to_ms_range, parse_utc_date};
use super::manifest::{MANIFEST, Manifest, SeriesLock, read_manifest};
use super::{
    HistoryStoreDataFusion, coverage_of, find_manifest_series_dirs, parse_series_id, part_dir,
};

/// The length, modification time and inode of a series' two manifest files (the base and the delta
/// log), `None` for one that is absent. Every write publishes through one of the two, so a write
/// moves it: a frame only ever lengthens the log until a fold removes it, and a base is only ever
/// published by renaming a NEW file over the old one (`write_manifest`), so a republished base is a
/// new inode even when its length and its mtime tick are the old ones (the CI box's kernel stamps a write
/// with the coarse clock, a tick of milliseconds).
///
/// shortcut: off unix the inode is `0`, so a base rewritten to the SAME length within one tick (with
/// an unchanged log) keeps its stamp there; read the platform's file id if a store is served off unix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManifestStamp([Option<(u64, Option<SystemTime>, u64)>; 2]);

fn manifest_stamp(dir: &Path) -> ManifestStamp {
    let of = |name: &str| {
        let meta = std::fs::metadata(dir.join(name)).ok()?;
        Some((meta.len(), meta.modified().ok(), inode(&meta)))
    };
    ManifestStamp([of(MANIFEST), of(DELTA)])
}

#[cfg(unix)]
fn inode(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> u64 {
    0
}

/// [`HistoryStoreDataFusion::inventory`]'s and [`HistoryStoreDataFusion::coverage_report`]'s memo:
/// each series' fold ([`Held`]) beside the [`ManifestStamp`] it was folded under.
///
/// ⚠ **It exists because the inventory was a full store walk per request**, measured on the CI box
/// (2026-11-16): 497 manifests holding 331 MB of JSON (one grouped book manifest alone 155 MB) and
/// 189,001 part files stat-ed for their sizes, 3.2 to 5.2 s before the first reply byte. A desktop
/// behind a slow tunnel gave up at 10 s. The directory walk alone is 0.06 s (`list_series`), so the
/// memo keeps the walk and skips the manifest parse and the part stats of every series whose stamp
/// has not moved: a write is seen at the next inventory, with no timer and no write-side hook (a
/// writer in another process, the CLI's engine, moves the stamp as well).
///
/// ⚠ **A series being recorded moves its stamp every few seconds**, so a stamp alone saved nothing on
/// exactly the manifests that cost the most: the CI box's five live series (the polymarket group's book,
/// trade and quote, binance's trade and depth) are 311 MB of base JSON and were re-read by every
/// inventory, 0.7 s of a 0.8 s answer, and by every coverage report, 2.4 s (measured 2026-11-17: the
/// book's base alone parses in 0.66 s, and depth's 9,846 logged frames replay in 0.32 s). Such a
/// series' log is append-only until a fold rewrites its base, so [`Held::replayed`] keeps the
/// manifest as of the last fold and the next one replays only the frames logged since.
#[derive(Default)]
pub(crate) struct CoverageMemo(Mutex<HashMap<PathBuf, Held>>);

/// One series' fold in [`CoverageMemo`].
struct Held {
    stamp: ManifestStamp,
    coverage: SeriesCoverage,
    /// Each part's UTC day in `files` order, what [`HistoryStoreDataFusion::coverage_report`] joins;
    /// `None` when a part's date does not parse (the report then reads the manifest and says why).
    days: Option<Vec<i64>>,
    /// The manifest this fold read, with its commit keys dropped (a fold needs none, and they are
    /// most of a big manifest's bytes), kept while the series has a delta log.
    replayed: Option<Manifest>,
}

impl Held {
    /// `dir`'s fold under `stamp`, taken from `was` (the memo's entry) whenever it can be.
    fn fold(
        dir: &Path,
        stamp: ManifestStamp,
        was: Option<Held>,
    ) -> Result<Held, HistoryStoreError> {
        let mut replayed = None;
        match was {
            Some(held) if held.stamp == stamp => return Ok(held),
            // Only the log moved. It is append-only until a fold rewrites the base, which moves the
            // base's stamp, so the frames past the held version are all that changed. The log is read
            // BEFORE the base's stamp is asked again, `read_manifest`'s order.
            Some(Held { stamp: was_stamp, replayed: Some(mut m), .. })
                if was_stamp.0[0] == stamp.0[0] =>
            {
                let (frames, _) = delta_read_with_end(dir)?;
                if manifest_stamp(dir).0[0] == stamp.0[0] {
                    for f in &frames {
                        if f.version > m.version {
                            m.apply(f);
                        }
                    }
                    m.orphan_commits.clear();
                    replayed = Some(m);
                }
            }
            _ => {}
        }
        let m = match replayed {
            Some(m) => m,
            None => {
                let mut m = read_manifest(dir)?;
                for f in &mut m.files {
                    f.commit_keys = Vec::new();
                }
                m.orphan_commits = Vec::new();
                m
            }
        };
        let days = m.files.iter().map(|f| parse_utc_date(&f.date).ok()).collect();
        let has_log = stamp.0[1].is_some_and(|(len, ..)| len > 0);
        Ok(Held { stamp, coverage: coverage_of(dir, &m), days, replayed: has_log.then_some(m) })
    }
}

/// Each series' parsed manifest ([`Listed`]) beside the [`ManifestStamp`] it holds for,
/// [`CoverageMemo`]'s rule: a series whose stamp has not moved answers without parsing its base or
/// replaying its log. Every lock-free read goes through it ([`HistoryStoreDataFusion::listed`]: every
/// bar and tick read, `series_days`, the commit-key questions, `series_facts`, `series_coverage` and
/// `series_gaps`), and so does the WRITER of an append, under its series lock
/// ([`HistoryStoreDataFusion::manifest_to_write`] and [`HistoryStoreDataFusion::keep_manifest`]).
///
/// ⚠ **It exists because a manifest was parsed on every request and on every commit**, measured on
/// the CI box. Readers (2026-11-19, one traced JOAT run): `series_days` over BTCUSDT `1d`'s whole history replayed
/// a 678 kB log of 3,363 frames and stat-ed 3,363 parts, 30 ms, and a USDT-USD daily rate's 448 kB log
/// 11 ms, on every run, and every page of a bar or tick read parsed its series' manifest again. Writers
/// (2026-11-19): every commit parsed its series' whole base and log under the lock, and the five live
/// recorded series hold 321 MB of base JSON (the Polymarket book's 162 MB committed every 2.65 s): about
/// 6.3 TB of JSON a day, 0.35 to 0.4 of a core. A writer now takes the manifest it kept at its own last
/// commit, edits it, publishes, and keeps it again under the stamp its publish left, so a commit parses
/// nothing and the readers of a series being written read the writer's copy.
///
/// ⚠ **It is exact because every change of a manifest is a publish, which moves the stamp**, and, for
/// `series_days`' part stats, because no writer changes a LISTED part's file: every writer seals a part
/// under a name no manifest lists (`next_part_name`, a merge's `part-c<version>`, a rename's
/// unpublished name), publishes, and unlinks only what it has already unlisted (manifest first). A
/// writer that rewrote a listed part in place would be missed by `series_days` until the series' next
/// publish, and the backtesthub would serve the old bars of that day. A kept WRITER's copy is exact for
/// one more reason: it is the manifest the writer published, and a writer's in-memory edit equals the
/// replay of the frame it published (`Manifest::apply`); a debug build re-reads the files at every
/// keep and asserts so. A writer that takes no copy (compaction, retention, rename, repair, delete)
/// moves the stamp, so the next commit re-reads, as a commit by another process does.
///
/// Bounded by the JSON bytes it was read from ([`MANIFEST_MEMO_BYTES`], the least recently read or
/// written evicted first); a series with no base is never held.
#[derive(Default)]
pub(crate) struct ManifestMemo(Mutex<MemoEntries>);

#[derive(Default)]
struct MemoEntries {
    held: HashMap<PathBuf, Kept>,
    /// Bumped by every read: what "least recently read" counts in.
    clock: u64,
}

struct Kept {
    stamp: ManifestStamp,
    listed: Arc<Listed>,
    read_at: u64,
}

/// The manifest JSON (base and log) [`ManifestMemo`] holds parsed at most. the CI box's 2,137 series held
/// 454 MB of it on 2026-11-19, 332 MB of that in its five live recorded series (the book's growing about
/// 2.3 MB a day), so all of it fits with room for months of growth; past it the coldest series goes
/// first, never a series being recorded, whose every commit makes it the newest. Parsed, a manifest
/// takes 1.16 bytes of heap per byte of JSON before allocator overhead (MEASURED on a 96 MB base of
/// 1.2M commit keys: `crates/vike-store/src/datafusion_hist/kept_manifest_tests.rs`'s
/// `the_commit_measured`), so the memo holds at most about 0.9 GB, under 1 GB with the allocator's
/// share, inside the datahub unit's `MemoryMax=8G`; the CI box's whole store today is about 0.55 GB of it.
/// The writer would allocate the same manifest at every commit without it.
const MANIFEST_MEMO_BYTES: u64 = 768 << 20;

/// One series' manifest as of one [`ManifestStamp`], and what `series_days` derives per part.
pub(super) struct Listed {
    pub(super) manifest: Manifest,
    /// Per part, in `manifest.files` order, found the first time `series_days` reaches it.
    days: Box<[OnceLock<PartDays>]>,
}

/// What `series_days` folds of one part besides its entry.
#[derive(Clone, Copy)]
struct PartDays {
    /// Written under provisional keys alone (`is_provisional_key`, a fixed table).
    provisional: bool,
    /// The part file's length and mtime.
    file: Option<(u64, Option<SystemTime>)>,
}

impl Listed {
    fn of(manifest: Manifest) -> Listed {
        let days = manifest.files.iter().map(|_| OnceLock::new()).collect();
        Listed { manifest, days }
    }
}

impl ManifestMemo {
    /// The manifest `dir` holds now, from the memo while its stamp holds.
    fn read(&self, dir: &Path) -> Result<Arc<Listed>, HistoryStoreError> {
        // Stamped BEFORE the read, as `inventory` stamps.
        let stamp = manifest_stamp(dir);
        {
            let mut memo = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            memo.clock += 1;
            let now = memo.clock;
            if let Some(kept) = memo.held.get_mut(dir)
                && kept.stamp == stamp
            {
                kept.read_at = now;
                return Ok(Arc::clone(&kept.listed));
            }
        }
        let listed = Arc::new(Listed::of(read_manifest(dir)?));
        let mut memo = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // Held only when no write landed during the read: a writer that published meanwhile has kept
        // its copy under the newer stamp, which this older one must not replace.
        if manifest_stamp(dir) == stamp {
            memo.admit(dir, stamp, Arc::clone(&listed), MANIFEST_MEMO_BYTES);
        }
        Ok(listed)
    }

    /// Under `dir`'s series lock: the manifest a writer edits. The memo's own while its stamp holds,
    /// TAKEN out (a reader meanwhile reads the files, which hold that same manifest until the writer
    /// publishes), and a fresh `read_manifest` otherwise.
    ///
    /// shortcut: a reader still holding the taken copy makes this clone it (tens of ms for the CI box's
    /// largest, against 0.7 s to parse it); hand readers a cheaper share if that shows in a profile.
    fn take(&self, dir: &Path) -> Result<Manifest, HistoryStoreError> {
        let stamp = manifest_stamp(dir);
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner).held.remove(dir);
        match kept {
            Some(kept) if kept.stamp == stamp => Ok(Arc::try_unwrap(kept.listed)
                .map_or_else(|shared| shared.manifest.clone(), |listed| listed.manifest)),
            _ => read_manifest(dir),
        }
    }

    /// Under the same lock, after the writer's publish returned (or it had nothing to publish): `m` IS
    /// what the files hold now, kept under the stamp they hold now.
    fn keep(&self, dir: &Path, m: Manifest) {
        let stamp = manifest_stamp(dir);
        #[cfg(debug_assertions)]
        assert_eq!(
            read_manifest(dir).as_ref().ok(),
            Some(&m),
            "the writer's kept manifest of {} disagrees with the files it published",
            dir.display()
        );
        let mut memo = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        memo.admit(dir, stamp, Arc::new(Listed::of(m)), MANIFEST_MEMO_BYTES);
    }
}

impl MemoEntries {
    /// Hold `listed` as `dir`'s manifest under `stamp`, the newest entry, evicting the coldest until
    /// every entry fits in `budget` JSON bytes. A series with no base (a probe of one that does not
    /// exist) or larger than the whole budget is not held.
    fn admit(&mut self, dir: &Path, stamp: ManifestStamp, listed: Arc<Listed>, budget: u64) {
        self.held.remove(dir);
        let bytes = json_bytes(stamp);
        if stamp.0[0].is_none() || bytes > budget {
            return;
        }
        while self.held.values().map(|kept| json_bytes(kept.stamp)).sum::<u64>() + bytes > budget {
            let Some(coldest) =
                self.held.iter().min_by_key(|(_, kept)| kept.read_at).map(|(d, _)| d.clone())
            else {
                break;
            };
            self.held.remove(&coldest);
        }
        self.clock += 1;
        self.held.insert(dir.to_path_buf(), Kept { stamp, listed, read_at: self.clock });
    }
}

/// The bytes of a series' base and log, as stamped.
fn json_bytes(stamp: ManifestStamp) -> u64 {
    stamp.0.iter().flatten().map(|(len, ..)| len).sum()
}

/// The inclusive UTC days an empty marker's window spans, or `None` for any other key.
fn empty_marker_days(key: &str) -> Option<(i64, i64)> {
    let window =
        key.strip_suffix(EMPTY_MARKER_SUFFIX)?.strip_prefix(venue_window_key_prefix(key)?)?;
    let (start, end) = window.split_once('-')?;
    let day = |ms: &str| Some(ms.parse::<i64>().ok()?.div_euclid(MS_PER_DAY));
    Some((day(start)?, day(end)?))
}

/// Each UTC day's running stamp and whether a provisional part meets it.
type DayStamps = BTreeMap<i64, (DefaultHasher, bool)>;

/// Fold, in manifest order (the order a read keeps the newest write of a bar time by), every part of `listed` (the
/// series at `dir`, from [`ManifestMemo`]) that overlaps `range` into the stamp of each UTC day it meets: the WHOLE
/// entry (name, date, edges, rows, commit keys) and the file's length and mtime, because a part NAME alone is reused
/// (`next_part_name` is `max + 1` of a date, so a removal hands its name to the next append), led by `layout` when
/// the read spans several directories. A day is marked provisional when a part of it was written under provisional
/// keys alone; a settled commit stamps its own key beside the provisional one it spends, so its day is not. Each
/// part's two facts are found once per manifest stamp ([`Listed`]'s `days`).
fn fold_days(
    dir: &Path,
    listed: &Listed,
    layout: Option<&Path>,
    range: TimestampRange,
    days: &mut DayStamps,
) {
    for (part, once) in listed.manifest.files.iter().zip(&listed.days) {
        if !part.overlaps(range) {
            continue;
        }
        let PartDays { provisional, file } = *once.get_or_init(|| PartDays {
            provisional: !part.commit_keys.is_empty()
                && part.commit_keys.iter().all(|key| is_provisional_key(key)),
            file: std::fs::metadata(part_dir(dir, &part.date).join(&part.name))
                .ok()
                .map(|meta| (meta.len(), meta.modified().ok())),
        });
        let first = range.start.map_or(part.ts_min, |s| s.max(part.ts_min));
        let last = range.end.map_or(part.ts_max, |e| e.min(part.ts_max));
        for day in first.div_euclid(MS_PER_DAY)..=last.div_euclid(MS_PER_DAY) {
            let (stamp, held) = days.entry(day).or_insert_with(|| (DefaultHasher::new(), false));
            if let Some(layout) = layout {
                layout.hash(stamp);
            }
            (&part.name, &part.date, part.ts_min, part.ts_max, part.rows, &part.commit_keys, file)
                .hash(stamp);
            *held |= provisional;
        }
    }
}

/// The folded days, ascending.
fn stamped(days: DayStamps) -> Vec<BarDay> {
    days.into_iter()
        .map(|(day, (stamp, provisional))| BarDay { day, stamp: stamp.finish(), provisional })
        .collect()
}

impl HistoryStoreDataFusion {
    /// `dir`'s manifest for a LOCK-FREE read, from [`ManifestMemo`] while the series' stamp holds.
    /// A writer takes [`Self::manifest_to_write`] under its series lock instead.
    pub(super) fn listed(&self, dir: &Path) -> Result<Arc<Listed>, HistoryStoreError> {
        self.manifest_memo.read(dir)
    }

    /// The manifest a writer holding `dir`'s series lock edits: the copy [`Self::keep_manifest`] kept
    /// while the series' stamp holds, else `read_manifest`. Hand it back to [`Self::keep_manifest`] once
    /// the publish returned, or once the writer decided to publish nothing; on any error, or after a
    /// change it did not publish, DROP it, and the next writer reads the files.
    pub(super) fn manifest_to_write(
        &self,
        _held: &SeriesLock,
        dir: &Path,
    ) -> Result<Manifest, HistoryStoreError> {
        self.manifest_memo.take(dir)
    }

    /// Keep `m`, what `dir`'s manifest files hold now, for the next writer and every reader, still under
    /// the series lock the publish was made under.
    pub(super) fn keep_manifest(&self, _held: &SeriesLock, dir: &Path, m: Manifest) {
        self.manifest_memo.keep(dir, m);
    }

    /// Enumerate every series in the store: walk the root tree for leaf dirs holding a
    /// `_manifest.json`, and parse each one's `(kind, venue, symbol, interval)` straight back out of
    /// its `kind=…/venue=…/symbol=…[/interval=…]` path segments. Returned sorted (deterministic order
    /// for reproducible maintenance + stable test assertions).
    ///
    /// This is a MAINTENANCE walk (feeds [`Self::run_maintenance`]), NOT the hot read path — a
    /// directory scan is fine here, exactly like WAL [`recovery`](super::wal). A leaf mid-creation
    /// (lock taken, manifest not yet written) is simply skipped this sweep and picked up the next.
    pub fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        // The walk's answer is held while nothing it read has moved (`series_walk.rs`'s `SeriesWalkMemo`).
        self.series_walk_memo.list(&self.root, |key| {
            let mut dirs = Vec::new();
            find_manifest_series_dirs(&self.root, &mut dirs, key)?;
            let mut out: Vec<SeriesId> =
                dirs.iter().filter_map(|d| parse_series_id(&self.root, d)).collect();
            out.sort();
            Ok(out)
        })
    }

    /// Coverage for one series (manifest fold + per-part fs size). Empty series → all-zero.
    /// NO DataFusion scan — cheap enough to call per-series in a loop ([`Self::inventory`]).
    pub fn series_coverage(&self, id: &SeriesId) -> Result<SeriesCoverage, HistoryStoreError> {
        let dir = self.series_dir_of(id);
        // ONE fold, shared with [`Self::series_facts`] — see [`coverage_of`] for why it is a free
        // function rather than two copies of the same arithmetic.
        Ok(coverage_of(&dir, &self.listed(&dir)?.manifest))
    }

    /// One series' COVERAGE **and** its INGEST COMMIT KEYS, from **ONE** manifest parse.
    ///
    /// ⚠ **This exists because the two questions have one answer and asking them separately parsed
    /// it twice.** [`Self::series_coverage`] and [`Self::series_commits`] each call the private
    /// `read_manifest` — so a caller that wants both paid two full parses of the same file, which
    /// on a grouped series is a multi-megabyte JSON document. `crates/vike-backtesthub/src/
    /// backtest_cli.rs`'s `collect_data_fingerprint` is that caller, its own doc named this method
    /// as the missing piece ("merging the two answers needs a `HistoryStoreDataFusion` method that returns
    /// coverage and commits from ONE parse, which is that crate's edit rather than this one's"),
    /// and it is what lets a parameter SEARCH address its inputs for the price of the data witness
    /// it was already paying for.
    ///
    /// The two halves are byte-for-byte what the single-question accessors answer — they share this
    /// method's fold and its `read_manifest` call — so nothing that reads one of them can disagree
    /// with a caller that reads both. It is NOT a cheaper coverage: the per-part `fs::metadata`
    /// stat that fills [`SeriesCoverage::bytes`] happens here exactly as it does there.
    pub fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        let dir = self.series_dir_of(id);
        let m = &self.listed(&dir)?.manifest;
        Ok((coverage_of(&dir, m), m.commit_keys()))
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
    /// `vike_data::store::removal`'s plan reports it and `--produced-by` asserts over it — and so does anyone
    /// answering "which collector put these rows here", which the 2026-09-07 the CI box cleanup had to
    /// answer with a throwaway script reading the JSON by hand.
    ///
    /// ⚠ **An EMPTY vector is a real state, not an error.** A keyless append (`commit_key: None`)
    /// records nothing, and parts sealed before the commit-key metadata existed carry nothing to
    /// rebuild from either (`crate::datafusion_hist::manifest`'s `rebuild_manifest` reports the same
    /// fact as `parts_without_keys`). Such a series can satisfy NO provenance assertion, which is
    /// why the assertion refuses it rather than passing it — see `vike_data::store::removal::Provenance`.
    ///
    /// Resolved through [`Self::series_dir_of`] like every other id-taking method, so a grouped
    /// series answers about its `group=` leaf rather than about a phantom `symbol=` one.
    pub fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, HistoryStoreError> {
        Ok(self.listed(&self.series_dir_of(id))?.manifest.commit_keys())
    }

    /// Whether `commit_key` has already been spent for `id` — the membership-test twin of
    /// [`Self::series_commits`], for a caller that wants to skip work (a network fetch, say) whose
    /// result a repeat of the same key would make the store discard anyway. Same cost and the same
    /// safety as `series_commits`: one lock-free manifest read, and `Ok(false)` rather than an error
    /// for a series with no manifest yet (`read_manifest`'s missing-directory fallback), so a symbol
    /// with no prior write answers `false` with no special-casing at the call site.
    pub fn series_has_commit(
        &self,
        id: &SeriesId,
        commit_key: &str,
    ) -> Result<bool, HistoryStoreError> {
        Ok(self.listed(&self.series_dir_of(id))?.manifest.has_commit(commit_key))
    }

    /// [`Self::series_has_commit`] for SEVERAL keys from ONE manifest read: `out[i]` answers
    /// `keys[i]`.
    ///
    /// It exists for a day-chunked ingest that asks two questions about every day — is the day's own
    /// key spent (the day is stored), and is its EMPTY MARKER spent (the day is known to hold
    /// nothing; see [`vike_data::store::store_kind::EMPTY_MARKER_SUFFIX`]). Two calls would read the manifest
    /// twice per day, and a twelve-year request is about 4,400 days. Same safety as the one-key
    /// form: lock-free, and every answer `false` for a series with no manifest yet.
    pub fn series_has_commits(
        &self,
        id: &SeriesId,
        keys: &[&str],
    ) -> Result<Vec<bool>, HistoryStoreError> {
        let m = &self.listed(&self.series_dir_of(id))?.manifest;
        Ok(keys.iter().map(|k| m.has_commit(k)).collect())
    }

    /// The UTC days a read of `range` reads, each stamped: `HistoryStore::series_days`' answer, from one manifest
    /// read per layout and one `fs::metadata` per part, no Parquet ([`fold_days`] says what a stamp folds).
    ///
    /// - A BAR read (`kind` [`StoreKindId::Bar`], its `interval` named, a `source=` lane or none) folds the ONE
    ///   series `load_bars` reads; a repeat while the series' manifest stamp holds reads neither its manifest
    ///   nor its parts' metadata ([`ManifestMemo`]) and answers the same days, bit for bit.
    /// - A TICK read (`Quote`, `Trade`, `Book`; no interval, no source) folds EVERY layout the read reads, in its
    ///   order (`scan_symbol_across_layouts`' `tick_layouts`: the symbol's own series, then each `group=` series of
    ///   the kind and venue), each part with its layout's directory, so any part a read of a day could take rows from
    ///   is in that day's stamp. A layout that does not exist holds nothing, as the read treats it. As for a bar
    ///   read, a repeat while a layout's manifest stamp holds reads neither its manifest nor its parts' metadata.
    ///
    /// Any other kind, a bar read without its interval, or a tick read with an interval or a source is refused in
    /// words.
    pub fn series_days(
        &self,
        kind: StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: TimestampRange,
    ) -> Result<Vec<BarDay>, HistoryStoreError> {
        let refused = |why: String| Err(HistoryStoreError::Query(format!("series_days: {why}")));
        let mut days = DayStamps::new();
        match kind {
            StoreKindId::Bar => {
                let Some(interval) = interval else {
                    return refused("a bar read names its interval".to_string());
                };
                if let Some(lane) = source {
                    // As the lane's bar read refuses, before either becomes a path.
                    Self::refuse_a_hostile_symbol(symbol)?;
                    Self::refuse_an_undeclared_source(lane)?;
                }
                let dir = self.series_dir(StoreKindId::Bar, venue, symbol, Some(interval), source);
                let listed = self.listed(&dir)?;
                fold_days(&dir, &listed, None, range, &mut days);
            }
            StoreKindId::Quote | StoreKindId::Trade | StoreKindId::Book => {
                if interval.is_some() || source.is_some() {
                    return refused(format!("a `{kind}` read names no interval and no source"));
                }
                for layout in self.tick_layouts(kind, venue, symbol)? {
                    if layout.dir.try_exists().map_err(super::io)? {
                        let listed = self.listed(&layout.dir)?;
                        fold_days(&layout.dir, &listed, Some(&layout.dir), range, &mut days);
                    }
                }
            }
            other => {
                return refused(format!(
                    "`{other}` has no stamped read (bar, quote, trade or book)"
                ));
            }
        }
        Ok(stamped(days))
    }

    /// [`Self::list_series`] without the `source=` lanes: the series a read that names no lane reads
    /// (`Self::append_quotes_sourced`). What [`Self::inventory`] and
    /// [`Self::coverage_report`] answer from, and through them every planner, verdict, removal and
    /// picker, so a lane changes none of their answers. A kind's lane and its sourceless series share
    /// one `(venue, label)`, so a fold that took both would report a tape neither of them is.
    fn served_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        let mut ids = self.list_series()?;
        ids.retain(|id| id.source.is_none());
        Ok(ids)
    }

    /// Every series a read can read, with its coverage: `served_series` + [`Self::series_coverage`]
    /// each. A `source=` lane is in [`Self::list_series`] alone.
    ///
    /// A series whose manifest stamp has not moved since the last inventory answers from
    /// [`CoverageMemo`] without reading its manifest or its parts, and one whose log alone moved
    /// replays only the frames logged since; the memo then keeps exactly the series this walk
    /// found, so a deleted one leaves it.
    pub fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        let ids = self.served_series()?;
        let mut memo = self.coverage_memo.0.lock().unwrap_or_else(PoisonError::into_inner);
        let mut kept = HashMap::with_capacity(ids.len());
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let dir = self.series_dir_of(&id);
            // Stamped BEFORE the read: a write between the two leaves a stamp older than the fold,
            // which the next inventory folds again. The other order could keep an old fold under a
            // new stamp for good.
            let stamp = manifest_stamp(&dir);
            let held = match Held::fold(&dir, stamp, memo.remove(&dir)) {
                Ok(held) => held,
                Err(e) => {
                    memo.extend(kept);
                    return Err(e);
                }
            };
            out.push((id, held.coverage.clone()));
            kept.insert(dir, held);
        }
        *memo = kept;
        Ok(out)
    }

    /// The CROSS-KIND coverage report: every instrument, with `kind=trade`/`kind=quote`/`kind=book`
    /// lined up so a day one kind has and another lacks becomes a single visible row
    /// ([`vike_data::store::coverage::InstrumentCoverage::partial_days`]).
    ///
    /// This is what makes a backfill source's limits legible. A Polymarket venue-backfill restores
    /// the trade tape and nothing else — no book history exists to fetch — so per-series BOTH
    /// manifests look unremarkable (contiguous trades; a book series that simply has no rows there),
    /// while the joined view shows a window a market-making backtest would run over with no book at
    /// all.
    ///
    /// Same cost class as [`Self::inventory`], and answered from the same [`CoverageMemo`]: one
    /// manifest read per series whose stamp moved, NO Parquet scan, so it is cheap enough for the
    /// Data Manager to call on open.
    ///
    /// ⚠ Over `served_series`: `join_coverage` keeps ONE day set per `(venue, label, kind)`, so
    /// a lane would REPLACE the sourceless series' days and a day only the lane holds would read as
    /// held to a gap planner (`crates/vike-backfill/src/gapfill.rs`'s `plan_fill`), which reads this.
    pub fn coverage_report(
        &self,
    ) -> Result<Vec<vike_data::store::coverage::InstrumentCoverage>, HistoryStoreError> {
        let mut pairs: Vec<(SeriesId, Vec<i64>)> = Vec::new();
        let mut memo = self.coverage_memo.0.lock().unwrap_or_else(PoisonError::into_inner);
        for id in self.served_series()? {
            if !vike_data::store::coverage::TICK_KINDS.contains(&id.kind) {
                continue; // don't even read the manifest of a kind the report ignores
            }
            let dir = self.series_dir_of(&id);
            let stamp = manifest_stamp(&dir);
            let held = Held::fold(&dir, stamp, memo.remove(&dir))?;
            let days = match &held.days {
                Some(days) => days.clone(),
                // A part's date did not parse: the manifest's own read says which.
                None => {
                    let m = read_manifest(&dir)?;
                    let mut days = Vec::with_capacity(m.files.len());
                    for f in &m.files {
                        days.push(parse_utc_date(&f.date)?);
                    }
                    days
                }
            };
            pairs.push((id, days));
            memo.insert(dir, held);
        }
        Ok(vike_data::store::coverage::join_coverage(&pairs))
    }

    /// The GAP ranges (inclusive epoch-ms, same convention as [`SeriesCoverage`]'s
    /// `first_timestamp`/`last_timestamp`) missing within `id`'s recorded span — the Data Manager's "where's the
    /// hole" view. Derived purely from the manifest's `date=` file index ([`vike_data::store::coverage::find_gaps`] over
    /// the distinct `FileEntry::date`s): NO Parquet scan, so this is as cheap as
    /// [`Self::series_coverage`]. A series with fully contiguous coverage, fewer than two distinct
    /// days on record, or no data at all all return `Ok(vec![])` — this never errors on "no data".
    ///
    /// A day whose EMPTY MARKER is spent ([`vike_data::store::store_kind::EMPTY_MARKER_SUFFIX`]) is
    /// no gap: a chunked ingest asked for it and proved it holds nothing. No calendar: a closed
    /// market nobody marked is still a gap here, and a series with no marker answers as if markers
    /// did not exist. Clipped to the span the parts hold, so a marker clears a hole and never opens
    /// one.
    pub fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        let m = &self.listed(&self.series_dir_of(id))?.manifest;
        let mut days: BTreeSet<i64> = BTreeSet::new();
        for f in &m.files {
            days.insert(parse_utc_date(&f.date)?);
        }
        if let (Some(&first), Some(&last)) = (days.first(), days.last()) {
            for (from, to) in m.orphan_commits.iter().filter_map(|k| empty_marker_days(k)) {
                days.extend(from.max(first)..=to.min(last));
            }
        }
        let days: Vec<i64> = days.into_iter().collect();
        Ok(vike_data::store::coverage::find_gaps(&days, 1)
            .into_iter()
            .map(day_gap_to_ms_range)
            .collect())
    }
}

#[path = "kept_manifest_tests.rs"]
#[cfg(test)]
mod kept_manifest_tests;
