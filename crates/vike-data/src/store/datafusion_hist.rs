//! `DataFusionHist` — the DataFusion + Parquet backend for [`HistStore`] (feature `hist-datafusion`).
//!
//! Pure Rust (no C++/server/Python). Data lives as Parquet under a Hive-style tree; a per-series
//! **manifest** (JSON, NOT a database — the spec's one-engine rule) is the file index: it lists
//! each sealed part with its `date=` partition, `[ts_min, ts_max]` + row count, plus a commit-log
//! of ingested batch keys for idempotency. Reads consult the manifest to select overlapping files
//! and read exactly those (no directory LIST — the spec's must-fix #5); writes append a sealed part
//! per UTC day and publish a new manifest version by atomic rename. See [`mod@manifest`] for the
//! manifest itself and [`mod@wal`] for the crash-recovery log guarding the seal→publish window.
//!
//! The module is split by concern: [`mod@manifest`] (file index + commit-log), [`mod@wal`]
//! (crash-recovery log + replay), [`mod@query`] (the DataFusion read paths), [`mod@codec`]
//! (Arrow `RecordBatch` <-> domain-type schemas/encoders/decoders for bars/quotes/trades/book), and
//! the `impl DataFusionHist` blocks that each own one concern of the type itself: `ingest` (the
//! write path: commit, grouped and superseding appends), `compaction`, `maintenance` (retention and
//! the store-wide pass), `rebuild` (manifest re-derivation and its plan), `inventory` (what the
//! store holds, from the manifests alone), `lifecycle` (fold a series into a group, delete one) and
//! `bulk` (the opt-in bulk write profile). This file owns the `DataFusionHist` type itself — its
//! constructors, the on-disk path helpers and the scan helpers every concern shares — plus the
//! `HistStore` trait impl and the small set of path/write helpers all the submodules share.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use tokio::runtime::Runtime;

use vike_model::Bar;

use crate::store::hist::{DataError, HistStore, TsRange};
use crate::store::hist_maint::WriteProfile;
use crate::store::series::{SeriesCoverage, SeriesId};

mod bulk;
mod codec;
mod compaction;
// The base+delta manifest's append-only half: the framed log a publish writes INSTEAD of rewriting
// the whole manifest. Its module doc carries the framing, the crash story and the read-ordering
// rule `manifest::read_manifest` depends on.
mod delta;
mod gaps;
mod hist_store;
mod ingest;
mod inventory;
mod lifecycle;
mod maintenance;
// The STORE's persisted source-precedence rule (`_sources.json`) — what lets `run_maintenance`
// resolve two writers' overlapping rows on its own. Absent = today's behaviour, byte-identical.
mod manifest;
mod merge;
mod parquet_io;
mod query;
mod rebuild;
/// The manifest REPAIR plan — what a rebuild would recover and what it would lose. `pub` because an
/// operator reads it: `vike-cli data hist repair` (through the engine's `backtest data repair`) renders
/// one before it writes anything.
pub mod repair;
mod series_walk;
pub mod sources;
mod supersede;
mod wal;

pub use bulk::{BulkConfig, BulkFlushReport, BulkIngestSession, GroupResolver};
// `find_gaps` is re-exported from the crate ROOT out of `coverage` (ungated) rather than here —
// see `gaps.rs`. Leaving a second gated path to the same function would make `vike_data::find_gaps`
// mean different things depending on features.
pub use manifest::RebuildReport;
pub use repair::RepairPlan;
pub use sources::{StoreSourcePolicy, load_policy, save_policy};

use codec::{BarCodec, SeriesCodec};
use manifest::{FOLD_BYTES, Manifest};
use merge::{grouped_symbol_of, part_dir, plan_merge_groups, sort_for_merge, supersede_by_rank};
use parquet_io::{
    COMMIT_KEYS_META, encode_chunked, file_url, fsync_dir, interval_ms, write_parquet,
};
use query::Layout;
use series_walk::{find_manifest_series_dirs, parse_series_id};
use supersede::{SupersedeStep, unlink_superseded};

#[cfg(test)]
use crate::store::hist_maint::CompactionConfig;
#[cfg(test)]
use parquet_io::COMPACT_BATCH_ROWS;

fn q(e: impl std::fmt::Display) -> DataError {
    DataError::Query(e.to_string())
}
fn io(e: impl std::fmt::Display) -> DataError {
    DataError::Io(e.to_string())
}

/// The store ROOT could not be created — the one `create_dir_all` failure whose cause an operator
/// will not guess from the raw errno, so it is named here instead of surfacing as `os error 30`.
///
/// **The failure this exists for.** Every shipped unit sets `ProtectSystem=strict`, which mounts the
/// whole filesystem read-only except what `ReadWritePaths=` names. [`DataFusionHist::open`] CREATES
/// the root it is given, so a daemon whose store root is not listed there dies at open with
/// `EROFS` — and `Read-only file system` alone points at the disk, not at the unit file that made it
/// read-only. It also fires on `EACCES`, the sibling shape (a store root owned by another user, or a
/// `-m700` parent).
///
/// ⚠ **Diagnosis only, never a probe.** Nothing here tests writability ahead of time: a pre-flight
/// probe is a TOCTOU guess, and it would make an otherwise-pure store open depend on the
/// filesystem's permissions. This decorates the failure that actually happened.
fn unwritable_store_root(root: &std::path::Path, e: &std::io::Error) -> DataError {
    use std::io::ErrorKind::{PermissionDenied, ReadOnlyFilesystem};
    if !matches!(e.kind(), ReadOnlyFilesystem | PermissionDenied) {
        return io(e);
    }
    DataError::Io(format!(
        "cannot create the hist store root {}: {e}. If this is a systemd unit: \
         ProtectSystem=strict mounts everything read-only except what ReadWritePaths= names, so \
         add this exact path to ReadWritePaths= (and create the directory first — systemd refuses \
         to start a unit naming a path that does not exist). Otherwise point the store at a \
         writable location with the config.store_root row, --store, or the profile's `store =`.",
        root.display()
    ))
}

/// DataFusion-backed historical store rooted at a directory of Parquet + manifests.
pub struct DataFusionHist {
    root: PathBuf,
    rt: Runtime,
    /// TEST-ONLY crash-injection switch. When set, [`Self::commit_rows`] seals its parquet part(s)
    /// and fsyncs the WAL record but SKIPS publishing the manifest — reproducing a crash in the
    /// seal→publish window so the recovery tests can prove the WAL replays it. The bulk write
    /// profile's own commit path (`bulk::commit_rows_bulk`) checks the SAME switch to reproduce the
    /// analogous "crashed after seal, before publish" window for its crash-safety tests — it has no
    /// WAL to replay, so that module's tests instead prove the crashed window is simply absent (not
    /// corrupted) and safely redone by a re-run. Always `false` in production; only the
    /// `#[doc(hidden)]` setter (used from the test crate) flips it.
    skip_publish_for_test: AtomicBool,
    /// TEST-ONLY crash-injection switch for COMPACTION, the sibling of `skip_publish_for_test`.
    /// When set, [`Self::compact_dir_inner`] returns after its unlocked MERGE phase and before the
    /// publish — the on-disk state a `SIGKILL` mid-compaction leaves, and the one that used to make
    /// [`Self::rebuild_series_manifest`] count a merge's inputs AND its output. Deterministic, so
    /// the regression test does not have to race a kill against a merge. Always `false` in
    /// production; only the `#[doc(hidden)]` setter flips it.
    stop_after_merge_for_test: AtomicBool,
    /// TEST-ONLY: force [`Self::commit_rows_inner`] to return immediately after its manifest publish
    /// succeeds but BEFORE it unlinks any superseded file — the exact crash window "manifest-first,
    /// unlink-after" exists to make harmless. Always `false` in production; only the
    /// `#[doc(hidden)]` setter (used from the test crate) flips it.
    stop_after_supersede_publish_for_test: AtomicBool,
    /// TEST-ONLY pause seam for COMPACTION, the second sibling of `skip_publish_for_test`. When
    /// set, [`Self::compact_dir_inner`] PARKS where `stop_after_merge_for_test` returns — the
    /// merge outputs written, the series lock NOT held, the publish not yet begun — announcing its
    /// arrival on the `Sender` and blocking on the `Receiver` until the test releases it.
    ///
    /// It exists because that window is what the plan/merge/publish split creates, and a test
    /// cannot otherwise prove it is IN it: a merge of a handful of one-row parts finishes three
    /// orders of magnitude inside `manifest::SPIN_ATTEMPTS`' ~4 s lock spin, so a concurrent
    /// appender lands whether the merge holds the lock or not, and row counts alone cannot tell the
    /// two designs apart. Parking makes the overlap a fact of the test rather than a race it hopes
    /// to win.
    ///
    /// The `Sender` half is not decoration: without it the test can only SLEEP until the compactor
    /// is probably parked, which is the timing assumption this seam exists to remove. One-shot —
    /// taken when it fires, so a later pass runs unimpeded. Always `None` in production; only the
    /// `#[doc(hidden)]` setter fills it.
    pause_in_merge_for_test: Mutex<Option<(Sender<()>, Receiver<()>)>>,
    /// Delta-log size at which a publish folds the log back into a new base
    /// ([`manifest::FOLD_BYTES`]). A field rather than a constant read at the call site for ONE
    /// reason: a fold at the production threshold is ~16,000 commits away, so no test could reach
    /// one, and an untested fold is where a crashed fold's double-apply would live. Production
    /// never changes it — only the `#[doc(hidden)]` setter does.
    fold_bytes: AtomicU64,
}

impl DataFusionHist {
    /// Open (creating the root dir if absent). Owns a tokio runtime so the sync [`HistStore`]
    /// methods drive DataFusion's async engine via `block_on` — callers stay sync. Runs WAL crash
    /// [`recovery`](Self::recover) once before returning, so any append that crashed after its WAL
    /// fsync but before its manifest publish is re-applied and visible — with ONE exception, which
    /// does not fail the open: a superseding commit whose provisional part was folded into a
    /// multi-key part meanwhile cannot be applied exactly, so it stays in its series' WAL,
    /// unapplied, with the records after it, and is logged at error level at every open.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DataError> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(|e| unwritable_store_root(&root, &e))?;
        let rt = Runtime::new().map_err(io)?;
        let store = Self {
            root,
            rt,
            skip_publish_for_test: AtomicBool::new(false),
            stop_after_merge_for_test: AtomicBool::new(false),
            stop_after_supersede_publish_for_test: AtomicBool::new(false),
            pause_in_merge_for_test: Mutex::new(None),
            fold_bytes: AtomicU64::new(FOLD_BYTES),
        };
        store.recover()?;
        Ok(store)
    }

    /// Open an EXISTING store WITHOUT performing either write [`open`](Self::open) performs — for a
    /// reader pointed at a root ANOTHER PROCESS is actively writing.
    ///
    /// [`open`](Self::open) is not a read-only operation and its two writes are both load-bearing
    /// where they belong: it `create_dir_all`s the root (so a tool pointed at a typo gets a fresh,
    /// valid, EMPTY store rather than an error — the trap `crates/vike-data/CLAUDE.md` names), and
    /// it runs the WAL crash-[`recovery`](Self::recover) sweep, which takes each affected series'
    /// [`manifest::SeriesLock`] and re-seals + re-publishes any un-published append. Against a LIVE store
    /// those are two different hazards: the first invents a store, and the second contends for a
    /// lock a running recorder holds — `SeriesLock::acquire` spins for ~4 s and then FAILS, so a
    /// probe can both stall a live writer and refuse to open at all.
    ///
    /// So this constructor creates nothing (an absent root is an ERROR, which is the answer a
    /// reader wants) and recovers nothing. Every read path below — the `scan_*` verbs,
    /// [`series_coverage`](Self::series_coverage), [`list_series`](Self::list_series) — is already
    /// lock-free and creates nothing, so a handle opened this way touches the store's bytes not at
    /// all.
    ///
    /// ⚠ **This bounds what OPENING does, not what the returned handle can do.** There is no
    /// read-only handle TYPE in this crate: the value is an ordinary [`DataFusionHist`] and its
    /// `append_*` verbs still work. A caller that wants the guarantee enforced rather than
    /// documented must not hold this value where a writer can reach it.
    ///
    /// ⚠ **Skipping recovery is not free, and the cost is the reason this is not the default.** An
    /// append that crashed in its seal→publish window stays invisible until somebody opens the
    /// store with [`open`](Self::open). A reader here therefore sees the PUBLISHED store, which is
    /// exactly what a live writer's own readers see, and never repairs one.
    pub fn open_read_only(root: impl AsRef<Path>) -> Result<Self, DataError> {
        let root = root.as_ref().to_path_buf();
        if !root.is_dir() {
            return Err(DataError::Query(format!(
                "open_read_only: no store directory at {} — a READER never creates one (that is \
                 `open`'s job, and inventing an empty store here would report zero rows instead of \
                 failing)",
                root.display()
            )));
        }
        Ok(Self {
            root,
            rt: Runtime::new().map_err(io)?,
            skip_publish_for_test: AtomicBool::new(false),
            stop_after_merge_for_test: AtomicBool::new(false),
            stop_after_supersede_publish_for_test: AtomicBool::new(false),
            pause_in_merge_for_test: Mutex::new(None),
            fold_bytes: AtomicU64::new(FOLD_BYTES),
        })
    }

    /// The root directory this store is rooted at (the path passed to [`open`](Self::open)).
    /// Consumers that must derive a filesystem location from the open store — e.g. the Studio
    /// persisting its saved-strategies/workspace JSON next to the store — read it here.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// TEST-ONLY: flip the crash-injection switch used by the WAL recovery tests (see
    /// [`skip_publish_for_test`](Self::skip_publish_for_test)). Not part of the public contract.
    #[doc(hidden)]
    pub fn set_skip_publish_for_test(&self, skip: bool) {
        self.skip_publish_for_test.store(skip, Ordering::SeqCst);
    }

    /// TEST-ONLY: stop a compaction between its unlocked merge and its publish (see
    /// [`stop_after_merge_for_test`](Self::stop_after_merge_for_test)). Not part of the public
    /// contract.
    #[doc(hidden)]
    pub fn set_stop_after_merge_for_test(&self, stop: bool) {
        self.stop_after_merge_for_test.store(stop, Ordering::SeqCst);
    }

    /// TEST-ONLY: force `commit_rows_inner` to return immediately after its manifest publish
    /// succeeds but BEFORE it unlinks any superseded file — the exact crash window
    /// "manifest-first, unlink-after" exists to make harmless. Not part of the public contract.
    #[doc(hidden)]
    pub fn set_stop_after_supersede_publish_for_test(&self, stop: bool) {
        self.stop_after_supersede_publish_for_test.store(stop, Ordering::SeqCst);
    }

    /// TEST-ONLY: park a compaction between its unlocked merge and its publish, instead of
    /// abandoning it there (see [`pause_in_merge_for_test`](Self::pause_in_merge_for_test)). The
    /// compaction sends on `parked` when it arrives and resumes when `resume` yields a value OR its
    /// sender is dropped — so a test that panics while the compaction is parked releases it by
    /// unwinding, rather than leaving a thread holding nothing forever. Not part of the public
    /// contract.
    #[doc(hidden)]
    pub fn set_pause_in_merge_for_test(&self, parked: Sender<()>, resume: Receiver<()>) {
        *self.pause_in_merge_for_test.lock().expect("pause seam mutex") = Some((parked, resume));
    }

    /// TEST-ONLY: the delta-log size at which a publish folds (see
    /// [`fold_bytes`](Self::fold_bytes)). Production leaves it at `manifest::FOLD_BYTES`, which is
    /// ~16,000 commits on the live box's largest series — far past what a test can drive, which is
    /// why the seam exists at all. Not part of the public contract.
    #[doc(hidden)]
    pub fn set_fold_bytes_for_test(&self, bytes: u64) {
        self.fold_bytes.store(bytes, Ordering::SeqCst);
    }

    /// The fold threshold this handle publishes against.
    fn fold_bytes(&self) -> u64 {
        self.fold_bytes.load(Ordering::SeqCst)
    }

    /// The leaf directory for a series: `kind=…/venue=…[/source=…]/symbol=…[/interval=…]`. Parts
    /// live one level deeper under `date=…` (see [`part_dir`]).
    ///
    /// ⚠ **`source` sits directly under `venue=` and the position is FORCED, not chosen.**
    /// [`find_manifest_series_dirs`] returns the moment it finds a `_manifest.json` and never
    /// descends further ("series leaf: children are date= part dirs, never nested series"). Put
    /// `source=` BELOW the manifest-bearing leaf and a legacy sourceless series would HIDE every
    /// sourced sibling written under it — `list_series` would never emit them, `run_maintenance`
    /// would never compact or prune them, and the Data Manager would not show them. Above the leaf,
    /// a `source=` directory is a SIBLING of the legacy `symbol=` directory under one `venue=`,
    /// neither carries a manifest, and the walk descends into both. That is the whole argument for
    /// the placement, and `crates/vike-data/tests/store/source_dimension.rs`'s
    /// `a_sourced_leaf_and_its_legacy_sibling_are_both_walked` is the regression test for the
    /// invisibility it avoids.
    ///
    /// ⚠ `source` is `None` at every caller but [`Self::series_dir_of`], and that is the STAGE-3
    /// scope rather than an oversight: the verbs take `(venue, symbol, …)` and carry no lane, so no
    /// write in this tree can create a `source=` leaf. Scoping a producer is the accepted design's
    /// stage 8, which is not accepted — see [`crate::store::series::SeriesId::source`].
    fn series_dir(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        source: Option<&str>,
    ) -> PathBuf {
        let mut p = self.venue_dir(kind, venue, source).join(format!("symbol={symbol}"));
        if let Some(iv) = interval {
            p = p.join(format!("interval={iv}"));
        }
        p
    }

    /// `kind=…/venue=…[/source=…]` — the parent EVERY leaf of a series hangs under, per-symbol and
    /// grouped alike, and therefore the ONE place the `source=` segment is joined.
    ///
    /// One function rather than the segment spelled in both leaf builders: the two layouts must not
    /// be able to disagree about where the dimension sits, because a disagreement puts one of them
    /// below a manifest-bearing directory and makes it invisible (see [`Self::series_dir`]).
    fn venue_dir(&self, kind: &str, venue: &str, source: Option<&str>) -> PathBuf {
        let p = self.root.join(format!("kind={kind}")).join(format!("venue={venue}"));
        match source {
            Some(s) => p.join(format!("source={s}")),
            None => p,
        }
    }

    fn bars_dir(&self, venue: &str, symbol: &str, interval: &str) -> PathBuf {
        self.series_dir("bar", venue, symbol, Some(interval), None)
    }
    fn ticks_dir(&self, kind: &str, venue: &str, symbol: &str) -> PathBuf {
        self.series_dir(kind, venue, symbol, None, None)
    }

    /// Refuse a symbol that cannot be a directory name, BEFORE it becomes one.
    ///
    /// [`Self::series_dir`] above interpolates the symbol straight into `symbol={symbol}` and
    /// joins it, so a separator inside it silently adds a path level and a Win32-reserved
    /// character makes the directory uncreatable on Windows.
    /// `vike_model::paths::store_path::refuse_a_path_hostile_symbol` carries both measurements and is the
    /// ONE spelling of the rule — it lives in `vike-model` because `vike-catalog`, which owns the
    /// canonical spellings this points callers at, is layer 20 like this crate and cannot be
    /// depended on from here.
    ///
    /// ⚠ **Called from the WRITE verbs only, and that asymmetry is deliberate.** The read path
    /// (`scan_*`, `load_bars`) reaches `bars_dir`/`ticks_dir` too, and putting the check there
    /// would mean threading a `Result` through twenty-eight call sites to refuse a lookup that
    /// already answers "nothing" — the harm is a series WRITTEN under a name nobody can spell, not
    /// a read that finds none. A read of a hostile symbol stays a miss, exactly as before.
    ///
    /// # Errors
    ///
    /// [`DataError::Query`] — the closest of the two variants: nothing was attempted on disk, the
    /// series KEY is malformed. The message is the shared one, which names the character, says how
    /// it breaks and on which platform, and points at the crate that owns canonical spellings.
    fn refuse_a_hostile_symbol(symbol: &str) -> Result<(), DataError> {
        vike_model::paths::store_path::refuse_a_path_hostile_symbol(symbol)
            .map_err(DataError::Query)
    }

    /// The same rule for a GROUP, which is the other caller-supplied `…=` component.
    ///
    /// ⚠ **The message is REWORDED rather than reused verbatim**, and that is the whole reason this
    /// is a second function instead of one more call. The shared refusal opens `symbol "…" cannot
    /// be a store key`, and an operator who typed `--group` and is told about a SYMBOL looks for a
    /// symbol they did not supply. The rule, the character list and the per-character explanation
    /// all still come from `vike_model::paths::store_path` — only the noun is this crate's.
    fn refuse_a_hostile_group(group: &str) -> Result<(), DataError> {
        vike_model::paths::store_path::refuse_a_path_hostile_symbol(group)
            .map_err(|why| DataError::Query(why.replacen("symbol", "group", 1)))
    }

    /// The leaf directory for a GROUPED series: `kind=…/venue=…/group=…`.
    ///
    /// A grouped series holds MANY symbols in one part, told apart by the row-level `symbol_col`
    /// column, instead of one series per symbol. This is the layout storage-study item #5 exists to
    /// enable: writing 112,400 rows as 562 per-symbol series measured **17,239 ms** against **86 ms**
    /// as one series (PR #938), because each series pays a ~30.6 ms fixed commit floor.
    ///
    /// `group=` rather than `symbol=` is deliberate — the two are siblings under one `kind=/venue=`
    /// parent and never collide, so a store can hold BOTH layouts at once and migrate one venue at
    /// a time.
    /// The on-disk dir for a series id, honoring BOTH layouts — `group=…` when grouped, the
    /// `symbol=…[/interval=…]` path otherwise. The inverse of `parse_series_id`.
    ///
    /// **Every method taking a [`SeriesId`] must resolve its directory through here.** Five did
    /// not: they predate grouping and built the path from `id.symbol` directly, which is EMPTY for
    /// a grouped series (see that field's doc). That yields a path ending `symbol=`, whose manifest
    /// does not exist — and `read_manifest` returns an EMPTY manifest for a missing dir rather than
    /// failing, so all five silently succeeded while doing nothing: `series_coverage` reported
    /// 0 rows / 0 bytes / "no data", `coverage_report` saw no days, `series_gaps` found no gaps,
    /// `rebuild_series_manifest` rebuilt a phantom, and `delete_series` deleted nothing and
    /// returned `Ok(())`.
    ///
    /// Found by pointing the Data Manager at a real 148 MB recorded tape: its entire Polymarket
    /// group — 37.5 M book rows — rendered as `0 rows · 0 B`. Nothing failed loudly anywhere, which
    /// is exactly why it survived; an empty manifest is indistinguishable from an empty series
    /// unless you already know what the store holds.
    /// ⚠ **This is the ONE caller that can produce a sourced path**, because it is the one that
    /// builds from an identity rather than from verb arguments — and an id's source comes off DISK,
    /// through [`parse_series_id`]. That is what keeps `series_coverage`, `series_gaps`,
    /// `delete_series`, `rebuild_series_manifest`, compaction and retention exact for a sourced
    /// leaf the moment one exists, with no signature change anywhere: they all take a `SeriesId`.
    fn series_dir_of(&self, id: &SeriesId) -> PathBuf {
        match &id.group {
            Some(g) => self.group_dir(&id.kind, &id.venue, g, id.source.as_deref()),
            None => self.series_dir(
                &id.kind,
                &id.venue,
                &id.symbol,
                id.interval.as_deref(),
                id.source.as_deref(),
            ),
        }
    }

    fn group_dir(&self, kind: &str, venue: &str, group: &str, source: Option<&str>) -> PathBuf {
        self.venue_dir(kind, venue, source).join(format!("group={group}"))
    }

    /// Every grouped-series dir under `kind=…/venue=…`, sorted (deterministic scan order).
    ///
    /// This is a directory LIST, which the read path otherwise refuses (spec must-fix #5 — the
    /// manifest is the index precisely so reads never list). The exemption is bounded and
    /// deliberate: it lists GROUPS, of which a venue has a handful (`btc-5m`, `eth-5m`, …), not
    /// SYMBOLS, of which one Polymarket day has 562. That scale difference is the entire reason
    /// the rule exists.
    ///
    /// The alternative — a store-level `symbol → group` index — was rejected. For Polymarket the
    /// mapping is not computable (a token id is a 78-digit number saying nothing about its
    /// asset/tenor family), so it would have to be persisted, and a shared index updated on every
    /// append reintroduces exactly the locked read-modify-write this work exists to remove. One
    /// contended index in place of 562 uncontended ones is not obviously a win.
    ///
    /// Empty (not an error) when the venue has no grouped series — which is every store today.
    ///
    /// ⚠ **It lists the SOURCELESS grouped leaves and deliberately does NOT descend through a
    /// `source=` sibling.** The design's §4.2 names that descent as a cost of the placement, and it
    /// is real — but on its own it would be the WRONG half to ship. This function's one caller is
    /// [`Self::scan_symbol_across_layouts`], a READ that takes `(venue, symbol)` and names no lane,
    /// so descending would make a sourceless scan return every lane's rows UNIONED: the
    /// double-count that `crate::store::datafusion_hist::sources`'s module doc calls the hazard, made
    /// mandatory. The design answers it with `read_leaves`' one-leaf-or-REFUSE rule (§5.1), which
    /// arrives with `for_source` — neither is in the accepted stages, and neither is here. Until
    /// then no leaf carries a `source=` segment at all, so this lists everything there is.
    fn group_dirs(&self, kind: &str, venue: &str) -> Result<Vec<PathBuf>, DataError> {
        let parent = self.venue_dir(kind, venue, None);
        let entries = match std::fs::read_dir(&parent) {
            Ok(e) => e,
            Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(e)),
        };
        let mut out: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("group="))
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// Scan one symbol across BOTH layouts — its own per-symbol series plus any grouped series for
    /// the same `kind`/`venue` — under ONE row budget. The read behind all four capped tick verbs
    /// (`quote`, `trade`, and the `book`/`depth` pair, which regroup its rows into events) and their
    /// uncapped twins.
    ///
    /// Grouped parts hold many symbols, so the symbol predicate is pushed INTO their read (DataFusion
    /// prunes row groups by the `symbol_col` statistics) and their decoded rows are then judged by
    /// `keep`. The codec's decode already resolves each row's own `symbol_col`, so that filter is a
    /// plain equality check on decoded rows rather than anything schema-aware.
    ///
    /// ⚠ **ONE budget for every layout together, and a capped answer holds AT LEAST `budget` rows
    /// unless it is the whole range** — the contract `HistStore::scan_quotes_capped` states, and
    /// `crates/vike-data/src/store/datafusion_hist/query.rs`'s `collect_head` is where both halves are
    /// kept. This read applied the budget PER SERIES until 2026-10-01, on the argument that
    /// splitting one budget "would need to know each one's row count before reading any" — but the
    /// manifests know every part's row count, and the per-series clamps disagreed: the merged page
    /// was complete only up to the smallest, and a paged reader skipped the other layouts' rows
    /// past it. A grouped part's row count also counts every OTHER symbol in it, so one narrowed
    /// block could hold none of this symbol's rows while the range went on, and the pager stops on
    /// an empty page.
    /// `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md` carries the
    /// reproduction; `crates/vike-datahub/tests/grouped_tick_paging.rs` is it, over the wire.
    ///
    /// `None` — or a zero budget — reads the whole range, byte-identical to before budgets existed.
    /// A store with no grouped series lists an empty set of group directories and reads its one
    /// per-symbol series, exactly as it always did.
    fn scan_symbol_across_layouts<C: SeriesCodec>(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        range: TsRange,
        keep: impl Fn(&C::Row) -> bool,
        budget: Option<usize>,
    ) -> Result<Vec<C::Row>, DataError> {
        let mut layouts = vec![Layout { dir: self.ticks_dir(kind, venue, symbol), symbol: None }];
        layouts.extend(
            self.group_dirs(kind, venue)?
                .into_iter()
                .map(|dir| Layout { dir, symbol: Some(symbol) }),
        );
        self.collect_head::<C>(&layouts, range, symbol, budget_to_count(budget), keep)
    }

    /// A capped read of ONE per-symbol series — the three research verbs (`cohort`,
    /// `perp_metrics`, `equity`), none of which has a grouped layout (`STORE_KINDS`), so the walk is
    /// [`Self::scan_symbol_across_layouts`]'s with a single layout and no row to judge. `ctx` is the
    /// codec's decode argument, exactly as the unbudgeted `scan_series` passes it.
    ///
    /// `None` — or a zero budget — is the whole range, the `scan_*_capped` reading of a budget;
    /// [`Self::load_bars_head`] and [`Self::scan_exec_fills_head`] take a COUNT instead and call
    /// `collect_head` directly.
    fn scan_one_series_capped<C: SeriesCodec>(
        &self,
        dir: PathBuf,
        range: TsRange,
        ctx: &str,
        budget: Option<usize>,
    ) -> Result<Vec<C::Row>, DataError> {
        let one = [Layout { dir, symbol: None }];
        self.collect_head::<C>(&one, range, ctx, budget_to_count(budget), |_| true)
    }

    // ---- scan_series: the SeriesCodec-generic read behind every HistStore scan_*/load_bars -----
    // method (dedup site: these were 4 nearly-identical scan bodies, differing only in which
    // `codec::` decode ran). Its append twin, `append_series`, lives in `ingest` — see
    // [`codec::SeriesCodec`] for the byte-identity argument.

    /// Read+decode+sort every row of `series_dir` in `range`, `C`-decoded. `ctx` is the codec's
    /// decode-time re-injection argument (symbol for quotes/trades, symbol-standing-in-for-venue for
    /// equity, unused otherwise — see [`codec::SeriesCodec::decode`]).
    fn scan_series<C: SeriesCodec>(
        &self,
        series_dir: &Path,
        range: TsRange,
        ctx: &str,
    ) -> Result<Vec<C::Row>, DataError> {
        self.scan_series_for::<C>(series_dir, range, ctx, None)
    }

    /// [`Self::scan_series`] with an optional symbol predicate pushed into the Parquet read.
    ///
    /// `None` is the per-symbol path: that series already holds one symbol, so there is nothing to
    /// filter and the read is byte-identical to before. `Some(sym)` is the GROUPED path, where the
    /// predicate is what lets DataFusion skip row groups by the `symbol_col` statistics instead of
    /// decoding the whole group — see `collect_for_symbol` for the measured cost of not doing it.
    ///
    /// It reads ONE directory and takes no budget: a capped tick read spans every layout a symbol
    /// lives in, under one budget, through [`Self::scan_symbol_across_layouts`].
    fn scan_series_for<C: SeriesCodec>(
        &self,
        series_dir: &Path,
        range: TsRange,
        ctx: &str,
        symbol: Option<&str>,
    ) -> Result<Vec<C::Row>, DataError> {
        let batches = self.collect_for_symbol(series_dir, range, symbol)?;
        let mut out = Vec::new();
        for b in &batches {
            out.extend(C::decode(b, ctx)?);
        }
        out.sort_by_key(C::sort_key);
        Ok(out)
    }
}

/// Write `bars` to a standalone Parquet file — the ENCODE half of an export, and the symmetric
/// twin of [`DataFusionHist::append_bars_from_parquet`].
///
/// # Why there is a WRITER at all
///
/// The reader has existed since bulk loading did, and its asymmetry was invisible until something
/// needed to hand a slice to somebody who does not have this store: the starter dataset published
/// as a release asset, a slice shared between boxes, a reproducer attached to a bug report. Each of
/// those was previously "copy the store directory", which ships the manifest, the commit keys and
/// every other series in the same part — the store's INTERNALS, to somebody who wanted six months
/// of one instrument.
///
/// The output carries exactly the columns [`DataFusionHist::append_bars_from_parquet`] selects, so
/// the pair round-trips: export a slice, load it into a fresh store, get the same bars back. That
/// is asserted rather than assumed (`crates/vike-data/tests/store/parquet_export.rs`).
///
/// ⚠ It writes a PLAIN file, not a part: no manifest entry, no commit key, and no store is even
/// in scope. A file this produces is data, and the store that later ingests it decides for itself
/// what commit key that ingest gets.
///
/// `WriteProfile::Sealed` deliberately — an exported file is read start to finish by whoever
/// receives it and is never appended to, which is exactly the shape compaction output has, and the
/// heavier compression is worth it for something that will be downloaded many times.
///
/// # Why it is a free function
///
/// Because the bars no longer come from a store this process opened. Decision 0084's 2026-09-25
/// amendment closed the local READ door on every history reader, and on 2026-09-26 the last one on
/// the data surface followed: `backtest data export` asks a datahub for the bars
/// (`vike_datahub_client::RemoteHistStore`'s paged `load_bars`) and only the encoding stays on this
/// side — the datahub's server is backend-agnostic by design and holds no Parquet encoder of its
/// own. An inherent method would have made the one sanctioned export path open a `DataFusionHist`
/// to reach the encoder, which is the second reader the ruling removed.
///
/// ⚠ **It WAS an inherent method until then, and that method is DELETED rather than kept beside
/// this.** `DataFusionHist::export_bars_parquet` was exactly `load_bars` followed by this body;
/// once the export stopped calling it, it had no production caller, and keeping it would have been
/// a second public spelling of one operation — the shape this workspace refuses for a moved symbol.
/// A caller that holds a store writes `write_bars_parquet(path, &store.load_bars(..)?)`, which is
/// the whole of what the method did. The wire carries a `Bar` as JSON with correctly-rounded float
/// parsing (the workspace pins `serde_json`'s `float_roundtrip`), so bars read over it encode
/// bit-for-bit what a local read would have —
/// `crates/vike-backtest/tests/optimizer_cli/data_export.rs`'s
/// `data_export_reads_its_bars_through_a_datahub_and_writes_what_the_store_holds` holds that
/// through a real datahub.
///
/// # Errors
///
/// Any Arrow/Parquet failure while encoding, or the file create. Returns the number of rows
/// written, which is ZERO for an empty slice — a valid empty file, because the alternative (an
/// error) would make "this slice is empty" indistinguishable from "the export broke".
pub fn write_bars_parquet(path: &Path, bars: &[Bar]) -> Result<usize, DataError> {
    let schema = BarCodec::schema();
    let batches = encode_chunked::<BarCodec>(bars, &schema)?;
    write_parquet(path, schema, batches, WriteProfile::Sealed, &[])?;
    Ok(bars.len())
}

/// Fold one already-parsed manifest into a [`SeriesCoverage`], stat-ing each part for its size.
///
/// A free function taking the PARSED manifest rather than a method that reads one, because
/// [`DataFusionHist::series_coverage`] and [`DataFusionHist::series_facts`] both need this
/// arithmetic and only one of them wants a parse of its own — see `series_facts` for what asking
/// the two questions separately used to cost. Two copies of a fold that computes a persisted
/// ADDRESS is exactly the drift this file's other shared helpers exist to prevent.
///
/// An EMPTY manifest folds to `SeriesCoverage::default()` rather than to `first_ts: i64::MAX` —
/// the early return the single-question accessor has always had, kept here so it cannot be lost by
/// one caller.
fn coverage_of(dir: &Path, m: &Manifest) -> SeriesCoverage {
    if m.files.is_empty() {
        return SeriesCoverage::default();
    }
    let mut first = i64::MAX;
    let mut last = i64::MIN;
    let mut rows = 0u64;
    let mut bytes = 0u64;
    let mut dates = BTreeSet::new();
    for f in &m.files {
        first = first.min(f.ts_min);
        last = last.max(f.ts_max);
        rows += f.rows as u64;
        dates.insert(f.date.clone());
        // part path: <series_dir>/date=<date>/<name> (matches how the reader builds part paths)
        let part = part_dir(dir, &f.date).join(&f.name);
        if let Ok(md) = std::fs::metadata(&part) {
            bytes += md.len();
        }
    }
    SeriesCoverage {
        first_ts: first,
        last_ts: last,
        rows,
        bytes,
        parts: m.files.len(),
        dates: dates.len(),
    }
}

/// A `scan_*_capped` BUDGET as the row COUNT `collect_head` reads to: `None` — or zero — is the
/// whole range, which `collect_head` spells `usize::MAX`. A count of zero there means "nothing",
/// so a zero budget must never reach it as one: the capped reads have always read `Some(0)` as no
/// budget at all (`narrow_to_budget` in the `query` module does too).
fn budget_to_count(budget: Option<usize>) -> usize {
    budget.filter(|b| *b > 0).unwrap_or(usize::MAX)
}

#[path = "datafusion_hist/compaction_encode_tests.rs"]
#[cfg(test)]
mod compaction_encode_tests;

#[path = "datafusion_hist/unwritable_root_tests.rs"]
#[cfg(test)]
mod unwritable_root_tests;

#[path = "datafusion_hist/plan_merge_groups_tests.rs"]
#[cfg(test)]
mod plan_merge_groups_tests;

#[path = "datafusion_hist/parse_series_id_tests.rs"]
#[cfg(test)]
mod parse_series_id_tests;
