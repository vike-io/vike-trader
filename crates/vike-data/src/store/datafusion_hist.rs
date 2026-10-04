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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::basic::{Compression, ZstdLevel};
use datafusion::parquet::file::metadata::KeyValue;
use datafusion::parquet::file::properties::WriterProperties;
use tokio::runtime::Runtime;

use vike_model::{
    Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick, consolidate_quotes,
    consolidate_trades,
};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::perp_metrics_log::PerpMetricRow;
use crate::store::hist::{BarEdges, DataError, HistStore, TsRange};
use crate::store::hist_maint::{
    CompactionConfig, Durability, GROUPED_ROW_GROUP_ROWS, WriteProfile,
};
use crate::store::series::{SeriesCoverage, SeriesId};

mod bulk;
mod codec;
mod compaction;
// The base+delta manifest's append-only half: the framed log a publish writes INSTEAD of rewriting
// the whole manifest. Its module doc carries the framing, the crash story and the read-ordering
// rule `manifest::read_manifest` depends on.
mod delta;
mod gaps;
mod ingest;
mod inventory;
mod lifecycle;
mod maintenance;
// The STORE's persisted source-precedence rule (`_sources.json`) — what lets `run_maintenance`
// resolve two writers' overlapping rows on its own. Absent = today's behaviour, byte-identical.
mod manifest;
mod query;
mod rebuild;
/// The manifest REPAIR plan — what a rebuild would recover and what it would lose. `pub` because an
/// operator reads it: `vike-cli data hist repair` (through the engine's `backtest data repair`) renders
/// one before it writes anything.
pub mod repair;
pub mod sources;
mod wal;

pub use bulk::{BulkConfig, BulkFlushReport, BulkIngestSession, GroupResolver};
// `find_gaps` is re-exported from the crate ROOT out of `coverage` (ungated) rather than here —
// see `gaps.rs`. Leaving a second gated path to the same function would make `vike_data::find_gaps`
// mean different things depending on features.
pub use manifest::RebuildReport;
pub use repair::RepairPlan;
pub use sources::{StoreSourcePolicy, load_policy, save_policy};

use codec::{
    BarCodec, BookCodec, ChainCodec, CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec,
    FundingCodec, PerpMetricsCodec, PropertiesCodec, QuoteCodec, SeriesCodec, TradeCodec,
    book_rows, book_updates_from_rows,
};
use delta::DeltaFrame;
use manifest::{FOLD_BYTES, Manifest, publish};
use query::Layout;

fn q(e: impl std::fmt::Display) -> DataError {
    DataError::Query(e.to_string())
}
fn io(e: impl std::fmt::Display) -> DataError {
    DataError::Io(e.to_string())
}

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
/// mutation reddens both — `crates/vike-data/tests/hist_datafusion.rs`'s
/// `a_resurrected_orphan_provisional_part_is_recognized_and_not_double_counted` pins the live stamp
/// and `a_replayed_superseding_commit_stamps_the_key_it_superseded` the replayed one.
///
/// What stays with each caller, because the two paths genuinely differ there: only the live path
/// appends to the WAL (a replayed record is already in it); the live path seals with its caller's
/// `WriteProfile` and the replay with `WriteOpts::live`; the live path rewrites the WAL after each
/// publish and the replay once, after its loop; the two test seams exist on the live path only; and
/// only the live path can arrive with an empty batch, which skips the seal and still supersedes.
struct SupersedeStep<'k> {
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
    stamp: Option<&'k str>,
}

/// What [`SupersedeStep::publish`] did.
struct Published {
    /// Whether a frame was published at all. Only a publish makes the commit durable, so only a
    /// publish lets the live path drop its WAL record.
    published: bool,
    /// The superseded parts' paths, which the caller hands to [`unlink_superseded`] only AFTER its
    /// own post-publish steps — manifest-first, unlink-after.
    to_unlink: Vec<PathBuf>,
}

impl<'k> SupersedeStep<'k> {
    /// [`plan_supersede`] plus the stamp rule, against `m` as it stood BEFORE this commit sealed
    /// anything (`plan_supersede`'s doc says why the order is load-bearing). `None` decides "remove
    /// nothing, stamp nothing". `Err` is the refusal: the caller writes nothing.
    fn decide(m: &Manifest, supersede_key: Option<&'k str>) -> Result<Self, DataError> {
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
    fn publish(
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
fn unlink_superseded(paths: Vec<PathBuf>) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
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
         writable location with VIKE_HIST_STORE, --store, or the profile's `store =`.",
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
    /// the placement, and `crates/vike-data/tests/hist_datafusion.rs`'s
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
/// is asserted rather than assumed (`crates/vike-data/tests/parquet_export.rs`).
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
/// bit-for-bit what a local read would have — `crates/vike-backtest/tests/optimizer_cli.rs`'s
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

/// Source-ranked supersession over rows ALREADY stable-sorted by `C::sort_key`, each tagged with its
/// part's source rank (LOWER = higher precedence). Within every run of rows sharing the same natural
/// key, keep only the rows whose rank equals the MINIMUM (highest-precedence source) present in that
/// run, dropping the rest as superseded duplicates; returns the kept rows (still key-ascending) and
/// the count dropped.
///
/// Rows that share their run's minimum rank are ALL kept — every level-row of one book event (they
/// share `(ts, seq)`), or several trades in one millisecond, come from ONE source and so ONE rank, so
/// a run touched by a single source drops nothing. That is exactly why a collision-free (single-
/// source) series supersedes byte-identically to the plain [`DataFusionHist::compact_roundtrip`]
/// rewrite. A run holding two sources keeps the whole higher-precedence source and drops the lower —
/// window supersession trusts one authoritative source per key rather than interleaving.
fn supersede_by_rank<C: SeriesCodec>(ranked: Vec<(usize, C::Row)>) -> (Vec<C::Row>, usize) {
    let n = ranked.len();
    let mut keep = vec![true; n];
    let mut i = 0;
    while i < n {
        // the run [i, j) of rows sharing this natural key (input is already sort_key-sorted)
        let key = C::sort_key(&ranked[i].1);
        let mut j = i + 1;
        while j < n && C::sort_key(&ranked[j].1) == key {
            j += 1;
        }
        let min_rank = ranked[i..j].iter().map(|(r, _)| *r).min().expect("run is non-empty");
        // drop every row in the run whose source lost to a higher-precedence one at this key
        for (slot, (rank, _)) in keep[i..j].iter_mut().zip(&ranked[i..j]) {
            if *rank != min_rank {
                *slot = false;
            }
        }
        i = j;
    }
    let mut out = Vec::with_capacity(n);
    let mut superseded = 0usize;
    for ((_, row), keep_row) in ranked.into_iter().zip(keep) {
        if keep_row {
            out.push(row);
        } else {
            superseded += 1;
        }
    }
    (out, superseded)
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

/// The `date=YYYY-MM-DD` sub-directory of a series leaf where a part file lives.
fn part_dir(series_dir: &Path, date: &str) -> PathBuf {
    series_dir.join(format!("date={date}"))
}

/// Split one `date=`'s parts (`idxs`, indices into `files`) into consecutive merge groups, each
/// small enough to decode at once.
///
/// This is what makes a compaction pass BOUNDED. Phase 2 decodes a group's parts into Arrow all at
/// once — it must, because the output is ts-sorted and a sort cannot stream — so the group, not the
/// date, is what sets peak memory. Planning one merge per `date=` made peak memory "however much
/// data that day happened to hold", which on the CI box (2026-08-04) was 3,634 parts / 658 MB of zstd
/// Parquet in a 4 GB cgroup: the OOM killer took the recorder down, and the kill left a lock that
/// wedged every restart for 11 h.
///
/// ⚠ **The bound is `max_merge_rows`, NOT `target_bytes`.** Bounding by compressed input was the
/// first fix and it was not enough: measured on that same the CI box series, a 64 MB budget still peaked
/// at **8.95 GB**, because the parts are only 2.5x compressed and the real cost is dictionary/RLE
/// decoding, the sort's copy, and per-file buffering across the ~950 files a byte budget admits.
/// `target_bytes` survives as what it always meant — the sealed-file size to converge toward, and
/// so which parts are already finished.
///
/// Four rules decide the grouping, and each earns its place:
///
///   * **A part at or over `target_bytes` is skipped.** It is finished; folding it in would rewrite
///     the whole thing to absorb a few KB of neighbours (write amplification).
///   * **A part whose OWN row count reaches `max_merge_rows` is skipped too.** It cannot be merged
///     within budget by definition, and without this the next rule would happily pair two of them.
///   * **A group takes at least 2 parts even if that overshoots.** Otherwise a series whose parts
///     each exceed half the budget would compact NOTHING, ever, and the fragment count compaction
///     exists to control would grow without bound. Two parts is the smallest unit of progress, and
///     the rule above caps the overshoot at just under 2x rather than leaving it open.
///   * **A trailing group of ONE part is dropped** — merging one part into one part is a rewrite
///     that changes nothing but the name. UNLESS `min_parts <= 1`, which is the caller saying a
///     lone fragment is worth a pass on its own: a one-part "merge" still re-encodes under the
///     CURRENT schema, which is how an older-schema part gets upgraded.
///
/// Groups otherwise follow manifest order, so each output covers a contiguous run and the date
/// converges toward one part over successive passes. Row counts come from the manifest, so the row
/// bound costs no I/O at all; only the `target_bytes` check needs a stat, and a part that cannot be
/// stat'd is treated as not-yet-at-target rather than failing the pass — the merge surfaces the real
/// error, and a stat is not the place to decide a part is broken.
fn plan_merge_groups(
    dir: &Path,
    files: &[manifest::FileEntry],
    idxs: &[usize],
    cfg: &CompactionConfig,
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_rows: usize = 0;
    for &i in idxs {
        let rows = files[i].rows;
        if rows >= cfg.max_merge_rows {
            continue; // too big to merge inside the memory budget, by itself
        }
        let bytes = std::fs::metadata(dir.join(&files[i].name)).map(|md| md.len()).unwrap_or(0);
        if bytes >= cfg.target_bytes {
            continue; // already at target size — nothing to gain by rewriting it
        }
        if cur.len() >= 2 && cur_rows.saturating_add(rows) > cfg.max_merge_rows {
            groups.push(std::mem::take(&mut cur));
            cur_rows = 0;
        }
        cur.push(i);
        cur_rows = cur_rows.saturating_add(rows);
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    let min_group = cfg.min_parts.clamp(1, 2);
    groups.retain(|g| g.len() >= min_group);
    groups
}

/// Recursively collect series leaf dirs (those directly containing a `_manifest.json`) under `dir`.
/// A manifest marks a series leaf, whose only children are `date=` part dirs — so recursion stops
/// there (no nested series). Maintenance-only tree walk (drives [`DataFusionHist::list_series`]); not
/// the read path.
fn find_manifest_series_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), DataError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(ref e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(e)),
    };
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
        return Ok(()); // series leaf: children are date= part dirs, never nested series
    }
    for sub in subdirs {
        find_manifest_series_dirs(&sub, out)?;
    }
    Ok(())
}

/// Parse a series leaf dir back into its [`SeriesId`] by reading the `key=value` path segments below
/// `root`: `kind=…/venue=…[/source=…]/symbol=…[/interval=…]`. Returns `None` if `dir` isn't under `root`, if any
/// segment isn't one of the expected keys (so a stray dir can't masquerade as a series), or if the
/// `symbol=` segment is EMPTY (see the arm below — that value is the grouped-series sentinel, not a
/// symbol). The inverse of [`DataFusionHist::series_dir`].
fn parse_series_id(root: &Path, dir: &Path) -> Option<SeriesId> {
    let rel = dir.strip_prefix(root).ok()?;
    let (mut kind, mut venue, mut symbol, mut interval, mut group, mut source) =
        (None, None, None, None, None, None);
    for comp in rel.components() {
        let seg = comp.as_os_str().to_str()?;
        if let Some(v) = seg.strip_prefix("kind=") {
            kind = Some(v.to_string());
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
            // generous: the inverse `DataFusionHist::series_dir_of` would then rebuild `group=` — a
            // DIFFERENT, non-existent directory — so coverage/gaps/rebuild/compaction/retention/
            // delete would each silently operate on a phantom (the exact five-bug class
            // `series_dir_of`'s doc records), and the grouped read path tells rows apart by a
            // `symbol_col` column that a per-symbol part does not carry.
            //
            // Only `symbol`, mirroring #1307: an empty `venue=`/`kind=` collides with no sentinel,
            // and an empty `group=` still parses as GROUPED — degenerate-looking but UNAMBIGUOUS,
            // since `group.is_some()` is what every consumer tells the two layouts apart by.
            //
            // The `warn!` is load-bearing, not decoration. Skipping is a real behaviour change for
            // a leaf that already holds rows: `run_maintenance` stops compacting and
            // retention-pruning it, and the reconcile pre-seed — which iterates `list_series()` and
            // scans each `kind=exec_fill` series by `id.symbol` — stops folding its trade ids into
            // the seen-fill dedup set. That consumer is the sentinel doc's own "silently scanning
            // `\"\"`" case, so refusing is right; doing it QUIETLY would swap one silence for
            // another. (⚠ The pre-seed named here was `vike-app`'s and went with the GUI's local
            // core; the same walk is `crates/vike-core/src/journal_view.rs`'s
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
            // `crates/vike-data/src/store/datafusion_hist.rs`'s `series_dir` carries why the position is
            // forced; `crates/vike-data/tests/hist_datafusion.rs`'s
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

/// Write `batches` to one Parquet file. `Hot` = light zstd (live append); `Sealed` = heavier zstd +
/// Parquet footer key under which a part records the commit keys whose rows it holds.
///
/// This is what makes a part SELF-DESCRIBING, and it is the difference between a manifest that can
/// be rebuilt and one that merely looks rebuildable. Every other field of a `FileEntry` is already
/// recoverable from the file itself — `name` from the path, `date` from its `date=` directory,
/// `rows` from the footer's row count, `ts_min`/`ts_max` from the `ts` column's row-group
/// statistics. `commit_keys` was the sole exception, and it is the load-bearing one: it IS the
/// idempotency log, so a rebuild without it would silently re-admit already-applied appends and
/// DUPLICATE rows. Storing it in the part closes that gap, so [`DataFusionHist::rebuild_manifest`]
/// is lossless rather than best-effort.
///
/// Multiple keys are newline-separated — a compacted part carries the union of its inputs' keys.
const COMMIT_KEYS_META: &str = "vike.commit_keys";

/// bounded row groups so a large compacted file keeps page-level `ts` pruning. Encodings are
/// byte-level only — every f64 is bit-identical across profiles (the store's `to_bits()` gate holds).
/// Rows per re-encoded compaction batch.
///
/// **This exists to keep one Arrow array's i32 offsets from overflowing.** A `StringArray` /
/// `BinaryArray` addresses its values with 32-bit offsets, so a SINGLE array cannot hold more than
/// `i32::MAX` (~2 GiB) of bytes — and compaction used to re-encode an entire `date=` partition into
/// ONE `RecordBatch`, i.e. one array per column, no matter how large the day was. Past that limit
/// arrow does not return an error, it PANICS:
///
/// ```text
/// thread 'vike-data-maint' panicked at arrow-array/src/array/byte_array.rs:236:45: offset overflow
/// ```
///
/// which killed the maintenance thread outright (a panic is not an `Err`, so the per-series
/// isolation could not contain it) and left the store with no compaction and no retention until the
/// process restarted. Observed on the CI box 2026-08-02 on a 31 M-row `kind=book` polymarket day whose
/// rows carry a wide JSON payload.
///
/// 262,144 rows keeps a single column under 2 GiB unless rows average more than ~8 KB in ONE column,
/// which no codec in this crate comes near. Chunking costs nothing: a Parquet file holds any number
/// of batches, and the writer's own `max_row_group_row_count` still decides row-group boundaries, so
/// the output file is unchanged in layout and byte-identical in content — only the in-memory arrays
/// feeding it are bounded. Nothing about the manifest, the part names, or the publish path changes.
const COMPACT_BATCH_ROWS: usize = 262_144;

/// Re-encode `rows` under `C`'s current schema as batches of at most [`COMPACT_BATCH_ROWS`] rows.
///
/// Always returns at least one batch, so an empty compaction still writes a valid (empty) part —
/// the shape the single-batch code produced before.
fn encode_chunked<C: SeriesCodec>(
    rows: &[C::Row],
    schema: &Arc<Schema>,
) -> Result<Vec<RecordBatch>, DataError> {
    if rows.is_empty() {
        let idxs: Vec<usize> = Vec::new();
        return Ok(vec![RecordBatch::try_new(schema.clone(), C::columns(rows, &idxs)).map_err(q)?]);
    }
    let mut out = Vec::with_capacity(rows.len().div_ceil(COMPACT_BATCH_ROWS));
    for start in (0..rows.len()).step_by(COMPACT_BATCH_ROWS) {
        let end = (start + COMPACT_BATCH_ROWS).min(rows.len());
        let idxs: Vec<usize> = (start..end).collect();
        out.push(RecordBatch::try_new(schema.clone(), C::columns(rows, &idxs)).map_err(q)?);
    }
    Ok(out)
}

fn write_parquet(
    path: &Path,
    schema: Arc<Schema>,
    batches: Vec<RecordBatch>,
    profile: WriteProfile,
    commit_keys: &[String],
) -> Result<(), DataError> {
    let level = match profile {
        WriteProfile::Hot | WriteProfile::Grouped => 1,
        WriteProfile::Sealed => 3,
    };
    let mut builder = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(level).map_err(q)?));
    match profile {
        // Compaction: big groups, because a sealed part is scanned by ts range, not by symbol.
        WriteProfile::Sealed => {
            builder = builder.set_max_row_group_row_count(Some(1_048_576));
        }
        // Grouped: SMALL groups, because a grouped part is read one symbol at a time and pruning
        // can only skip whole row groups (see `GROUPED_ROW_GROUP_ROWS`).
        WriteProfile::Grouped => {
            builder = builder.set_max_row_group_row_count(Some(GROUPED_ROW_GROUP_ROWS));
        }
        WriteProfile::Hot => {}
    }
    // Stamp the commit keys into the footer — see [`COMMIT_KEYS_META`]. This is what makes the part
    // self-describing and the manifest genuinely rebuildable rather than rebuildable-looking.
    if !commit_keys.is_empty() {
        builder = builder.set_key_value_metadata(Some(vec![KeyValue::new(
            COMMIT_KEYS_META.to_string(),
            commit_keys.join("\n"),
        )]));
    }
    let props = builder.build();
    let file = std::fs::File::create(path).map_err(io)?;
    let mut w = ArrowWriter::try_new(file, schema, Some(props)).map_err(q)?;
    for batch in &batches {
        w.write(batch).map_err(q)?;
    }
    // `into_inner` finalizes exactly as `close` does (footer written, buffers flushed) but hands the
    // `File` back so it can be fsynced. This fsync is UNCONDITIONAL and is the store's core
    // durability invariant: a manifest entry is never published before the part it names is durable.
    // Without it the footer sits in page cache while the manifest entry naming this part goes on to
    // be fsynced and published, so a power loss yields a durable manifest pointing at a truncated
    // part — and the WAL cannot repair that, because the commit key is already in the manifest
    // (on this very part's `FileEntry` since v3), which makes the retry a no-op. [`Durability`]
    // varies what happens ABOVE this line, never this line.
    let file = w.into_inner().map_err(q)?;
    file.sync_all().map_err(io)?;
    Ok(())
}

/// fsync a DIRECTORY, so a file created or renamed inside it survives a power loss.
///
/// On POSIX a `create` or `rename` is only durable once the CONTAINING DIRECTORY is fsynced —
/// fsyncing the file alone leaves its name recoverable-but-absent. Windows exposes no
/// directory-handle flush through `std::fs` (opening a directory as a `File` fails outright), so
/// this is a deliberate no-op there rather than a silent error; NTFS orders metadata through its own
/// journal, and the POSIX gap this closes does not transfer directly.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> Result<(), DataError> {
    std::fs::File::open(dir).map_err(io)?.sync_all().map_err(io)
}

#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> Result<(), DataError> {
    Ok(())
}

/// `file://` URL to a single Parquet file — works on Windows (a bare `C:\..` path fails
/// DataFusion's URL parse) AND unix.
///
/// The path is PERCENT-ENCODED, and that is not cosmetic: a series symbol lands verbatim in the
/// `symbol=` partition directory, and this store already carries symbols containing `#` — the
/// Polymarket window convention `<slug>#<outcome_index>` (`vike_strategy::CheapNp`'s symbol
/// grammar). In a URL `#` opens the FRAGMENT, so an unencoded path silently truncates there and
/// DataFusion reports "No files found ... Cannot infer schema from an empty location" for a file
/// that is sitting right there on disk. `?` (query) and `%` (the escape itself) have the same
/// hazard, as does any byte outside the URL path grammar.
fn file_url(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let p = p.strip_prefix('/').unwrap_or(&p);
    let mut out = String::from("file:///");
    for b in p.bytes() {
        // RFC 3986 unreserved + the sub-delims/path characters DataFusion's object-store URL
        // parser accepts literally. Everything else — notably `#`, `?`, `%`, space and any
        // non-ASCII byte — is escaped.
        let safe = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'/'
                    | b':'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b'@'
            );
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parse a bar interval ("1m"/"5m"/"15m"/"1h"/"4h"/"1d"/"30s") → step in epoch-ms. Delegates to
/// the single interval vocabulary in [`vike_model::time::interval_ms`].
fn interval_ms(interval: &str) -> Result<i64, DataError> {
    vike_model::time::interval_ms(interval)
        .ok_or_else(|| DataError::Query(format!("bad interval {interval:?}")))
}

impl HistStore for DataFusionHist {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        // parts may arrive unordered; contract is ts-ascending (BarCodec::sort_key = (ts, 0))
        self.scan_series::<BarCodec>(&self.bars_dir(venue, symbol, interval), range, "")
    }

    /// The BOUNDED twin of `load_bars` — the same series, the same parts, the same exact
    /// `ts` filter, answered by an aggregate over the `ts` column instead of a decode of every row
    /// (see `DataFusionHist::ts_edges` in the `query` module, which carries the argument and the
    /// layouts it must survive). The trait default would be correct here and would materialise the
    /// whole range, which is the defect this method exists to remove.
    fn bar_edges(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<BarEdges, DataError> {
        self.ts_edges(&self.bars_dir(venue, symbol, interval), range)
    }

    /// The BOUNDED twin of `load_bars` for a caller that wants only the START of a range — the same
    /// series, the same parts and the same exact `ts` filter, read a block of parts at a time and
    /// stopped once `n` rows of the range are in (see `DataFusionHist::collect_head` in the `query`
    /// module, which carries the loop and why its blocks join into a prefix of the whole read). The
    /// trait default would be correct here and would materialise the whole range, which is the
    /// defect this method exists to remove.
    fn load_bars_head(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        n: usize,
    ) -> Result<Vec<Bar>, DataError> {
        // One layout, and it holds one series, so `keep` is never asked about a row.
        let bars = [Layout { dir: self.bars_dir(venue, symbol, interval), symbol: None }];
        self.collect_head::<BarCodec>(&bars, range, "", n, |_| true)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.scan_quotes_capped(venue, symbol, range, None)
    }

    fn scan_quotes_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.scan_symbol_across_layouts::<QuoteCodec>(
            "quote",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.scan_trades_capped(venue, symbol, range, None)
    }

    fn scan_trades_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.scan_symbol_across_layouts::<TradeCodec>(
            "trade",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )
    }

    // ---- store inventory / metadata: the TRAIT overrides delegate to the inherent methods -----
    // Promote the concrete `DataFusionHist::{list_series, inventory, series_gaps, coverage_report}`
    // (the inherent impl above) onto the `HistStore` trait so a `&dyn HistStore` — e.g. the datahub server answering a
    // `RemoteHistStore` — reaches the real manifest walk, not the trait default (which REFUSES the
    // catalog pair and answers the derived views empty).
    // `DataFusionHist::method(self)` resolves to the INHERENT method (inherent items shadow trait
    // items in path resolution), so this is a one-line delegation, NOT recursion.

    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        DataFusionHist::list_series(self)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        DataFusionHist::inventory(self)
    }

    /// The trait half of the INHERENT `DataFusionHist::series_facts` — one manifest parse, one
    /// answer, no second spelling. Promoted so a routed reader can ask for it at all.
    fn series_facts(&self, id: &SeriesId) -> Result<(SeriesCoverage, Vec<String>), DataError> {
        DataFusionHist::series_facts(self, id)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        DataFusionHist::series_gaps(self, id)
    }

    fn coverage_report(
        &self,
    ) -> Result<Vec<crate::store::coverage::InstrumentCoverage>, DataError> {
        DataFusionHist::coverage_report(self)
    }

    // ...and the two PROVENANCE/REMOVAL verbs, promoted for the same reason and one more: the
    // datahub's delete verb answers through `&dyn HistStore`, so without these the server would
    // reach the trait's refusing defaults while holding the one store that can actually answer.
    fn series_commits(&self, id: &SeriesId) -> Result<Vec<String>, DataError> {
        DataFusionHist::series_commits(self, id)
    }

    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        DataFusionHist::delete_series_checked(self, id, require_produced_by)
    }

    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<BarCodec>(
            &self.bars_dir(venue, symbol, interval),
            commit_key,
            bars,
            WriteProfile::Hot,
        )
    }

    fn append_quotes(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<QuoteCodec>(
            &self.ticks_dir("quote", venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<TradeCodec>(
            &self.ticks_dir("trade", venue, symbol),
            commit_key,
            ticks,
            WriteProfile::Hot,
        )
    }

    fn append_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        let rows = book_rows(updates);
        let written_rows = self.append_series::<BookCodec>(
            &self.ticks_dir("book", venue, symbol),
            commit_key,
            &rows,
            WriteProfile::Hot,
        )?;
        // append_series (via commit_rows) returns per-level ROWS; the seam's contract is EVENTS
        // written.
        Ok(if written_rows == 0 { 0 } else { updates.len() })
    }

    fn append_depth(
        &self,
        venue: &str,
        symbol: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // Byte-identical to `append_book_updates` except the `kind` — same codec, same row shape,
        // same idempotency. The SERIES is the disclosure that this lane is conflated; the trait doc
        // has the argument for why that separation is not cosmetic.
        Self::refuse_a_hostile_symbol(symbol)?;
        let rows = book_rows(updates);
        let written_rows = self.append_series::<BookCodec>(
            &self.ticks_dir("depth", venue, symbol),
            commit_key,
            &rows,
            WriteProfile::Hot,
        )?;
        Ok(if written_rows == 0 { 0 } else { updates.len() })
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.scan_depth_capped(venue, symbol, range, None)
    }

    fn scan_depth_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it: a copy is how the per-series
        // budget would survive here after being fixed there. Nothing writes a grouped depth part
        // today, so the group half of this read finds nothing — as it did before the walk.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            "depth",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )?;
        book_updates_from_rows(rows, symbol)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // (ts, seq) sort (BookCodec::sort_key): rows of one event share both; events order by time
        // then feed seq. sort_by_key is stable, preserving intra-event level order from the write.
        // Both layouts, like quotes/trades: the per-symbol series first (unchanged), then any
        // grouped series with the symbol predicate pushed into the read. `ctx` stays `symbol` so a
        // row whose own symbol column is absent or empty still resolves correctly.
        self.scan_book_updates_capped(venue, symbol, range, None)
    }

    fn scan_book_updates_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<BookUpdate>, DataError> {
        // ⚠ The budget is in ROWS and one event is many rows — the write side explodes a
        // `BookUpdate` into one row per price level. So a budget bounds the READ, which is the
        // allocation that matters, while the event count it yields is smaller and
        // data-dependent. The cut never splits a `ts`, so it never splits an event either.
        // ⚠ The SAME layout walk as the tick verbs, not a copy of it — `scan_depth_capped` says why.
        // The rows arrive sorted by `(ts, seq)` (`BookCodec::sort_key`), which is what the regroup
        // below keys on.
        let rows = self.scan_symbol_across_layouts::<BookCodec>(
            "book",
            venue,
            symbol,
            range,
            |r| r.symbol == symbol,
            budget,
        )?;
        book_updates_from_rows(rows, symbol)
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        Self::refuse_a_hostile_symbol(symbol)?;
        self.append_series::<PropertiesCodec>(
            &self.ticks_dir("properties", venue, symbol), // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        self.scan_series::<PropertiesCodec>(&self.ticks_dir("properties", venue, symbol), range, "")
        // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
    }

    fn append_equity(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[EquitySample],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<EquityCodec>(
            &self.ticks_dir("equity", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.scan_series::<EquityCodec>(&self.ticks_dir("equity", venue, symbol), range, symbol)
    }

    /// The bounded twin of `scan_equity`: the same series, the same decode argument (the symbol,
    /// standing in for the venue column), read a block of parts at a time until the budget is met.
    fn scan_equity_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<EquitySample>, DataError> {
        self.scan_one_series_capped::<EquityCodec>(
            self.ticks_dir("equity", venue, symbol),
            range,
            symbol,
            budget,
        )
    }

    fn append_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecFillRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // The same guard `append_exec_orders` carries, for the same reason — and this side is the
        // one that MATTERS MORE, which is why it must not be left behind again.
        //
        // A reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds the
        // trade_ids it finds into the SEEN-FILL dedup set (`vike-app`'s did, as this said in the
        // present tense until 2026-09-28; `crates/vike-core/src/journal_view.rs`'s
        // `journal_view_from_store` is that walk today, reached by no production root yet). A fill
        // that never reaches that set is
        // a fill reconciliation believes it has not booked — a `MissingFill`, which is one of the two
        // kinds `hybrid` AUTO-APPLIES. So an unattributable fill row does not merely sit unreadable:
        // it is a live-money double-book waiting for the next reconcile pass.
        //
        // ⚠ The write side and the read side must land TOGETHER. `parse_series_id` (this file) now
        // refuses such a leaf, so leaving this guard off would create a shape that writes fine and
        // then silently vanishes from `list_series` — stranding exactly the trade_ids the dedup set
        // needs. Measured before this landed: zero `symbol=` leaves across 24,699 `symbol=*`
        // directories on the live boxes, so nothing existing is affected.
        //
        // ⚠ `vike_journal::materialize`'s `materialize_once` guards its ORDER path
        // (`symbol.is_empty() || venue.is_empty()`) but not its FILL path, which keys on
        // `(f.venue, f.symbol)` — that is how this shape could still be produced.
        if symbol.is_empty() {
            return Err(DataError::Query(format!(
                "append_exec_fills({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable, invisible to every scan, \
                 and its trade_ids would be missing from the reconcile seen-fill set"
            )));
        }
        self.append_series::<ExecFillCodec>(
            &self.ticks_dir("exec_fill", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_fills(&self, venue: &str, symbol: &str) -> Result<Vec<ExecFillRow>, DataError> {
        // Every field (venue+symbol included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<ExecFillCodec>(
            &self.ticks_dir("exec_fill", venue, symbol),
            TsRange::all(),
            "",
        )
    }

    /// The bounded twin of `scan_exec_fills` for a caller that wants only the START of a range of
    /// the series: the same series and decode, read a block of parts at a time and stopped once `n`
    /// rows are in — the walk `load_bars_head` takes. A COUNT, not a budget, so `n == 0` is
    /// "nothing" (`collect_head` answers it without reading) rather than the whole range.
    fn scan_exec_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        n: usize,
    ) -> Result<Vec<ExecFillRow>, DataError> {
        let fills = [Layout { dir: self.ticks_dir("exec_fill", venue, symbol), symbol: None }];
        self.collect_head::<ExecFillCodec>(&fills, range, "", n, |_| true)
    }

    fn append_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[ExecOrderRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        // REJECTS an empty `symbol` — the per-symbol twin of `append_book_updates_grouped`'s guard,
        // and for the same reason: a row that cannot be attributed must never become durable.
        //
        // A grouped series tells rows apart by their symbol COLUMN, so an empty column there is
        // unattributable. Here the symbol is the `symbol=` PATH SEGMENT, and an empty one is worse
        // than merely unaddressable: `crates/vike-data/src/store/series.rs`'s `SeriesId::group` reserves an
        // empty `symbol` as the GROUPED-series sentinel ("exactly one of `symbol`/`group` is
        // meaningful for any given series"), so
        // `parse_series_id` reads the leaf back as a `SeriesId { symbol: "", group: None }` that no
        // consumer can tell from a grouped id by the very field that doc says to tell them apart by.
        // Reconciliation queries this kind to learn what it knows; a leaf it cannot name is a row
        // that silently is not there. Enforced writer-side rather than papered over on read.
        //
        // The check is on the ARGUMENT and therefore unconditional — deliberately NOT skipped for an
        // empty `rows`, because `commit_rows` reaches `SeriesLock::acquire` (which `create_dir_all`s
        // the leaf) BEFORE it can notice the batch is empty: "no rows" is not "no partition".
        //
        // Only `symbol`, not `venue`: an empty `venue=` collides with no sentinel and the grouped
        // sibling checks no venue either, so widening here would be a local invention rather than
        // this precedent.
        if symbol.is_empty() {
            return Err(DataError::Query(format!(
                "append_exec_orders({venue}): empty symbol — a per-symbol series is addressed BY its \
                 `symbol=` path segment, so an empty one is unattributable and would be invisible to \
                 every scan"
            )));
        }
        self.append_series::<ExecOrderCodec>(
            &self.ticks_dir("exec_order", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_exec_orders(&self, venue: &str, symbol: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        self.scan_series::<ExecOrderCodec>(
            &self.ticks_dir("exec_order", venue, symbol),
            TsRange::all(),
            "",
        )
    }

    fn append_funding(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[FundingRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<FundingCodec>(
            &self.ticks_dir("exec_funding", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_funding(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<FundingRow>, DataError> {
        // Every field (hash included) is a stored column, so decode ignores `ctx` — pass "".
        self.scan_series::<FundingCodec>(&self.ticks_dir("exec_funding", venue, symbol), range, "")
    }

    fn append_chain_snapshot(
        &self,
        venue: &str,
        underlying: &str,
        rows: &[ChainRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<ChainCodec>(
            &self.ticks_dir("chain", venue, underlying),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TsRange,
    ) -> Result<Vec<ChainRow>, DataError> {
        // Every field (underlying included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, preserving within-snapshot row order from the write.
        self.scan_series::<ChainCodec>(&self.ticks_dir("chain", venue, underlying), range, "")
    }

    fn append_cohort(
        &self,
        venue: &str,
        asset: &str,
        rows: &[CohortRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<CohortCodec>(
            &self.ticks_dir("cohort", venue, asset),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        // Every field (asset included) is a stored column, so decode ignores `ctx` — pass "".
        // `sort_by_key` on (ts, 0) is stable, so the labels of one hour come back in the order they
        // were written: the caller's own axis/grading order, not an arbitrary one.
        self.scan_series::<CohortCodec>(&self.ticks_dir("cohort", venue, asset), range, "")
    }

    /// The bounded twin of `scan_cohort`. The walk's blocks join in the whole read's order, so the
    /// labels of one hour still come back in the order they were written.
    fn scan_cohort_capped(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.scan_one_series_capped::<CohortCodec>(
            self.ticks_dir("cohort", venue, asset),
            range,
            "",
            budget,
        )
    }

    fn append_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[PerpMetricRow],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_series::<PerpMetricsCodec>(
            &self.ticks_dir("perp_metrics", venue, symbol),
            commit_key,
            rows,
            WriteProfile::Hot,
        )
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        // `(venue, symbol)` is the whole identity and lives in the path, so no column needs
        // re-injecting on decode — pass "".
        self.scan_series::<PerpMetricsCodec>(
            &self.ticks_dir("perp_metrics", venue, symbol),
            range,
            "",
        )
    }

    /// The bounded twin of `scan_perp_metrics`.
    fn scan_perp_metrics_capped(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
        budget: Option<usize>,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.scan_one_series_capped::<PerpMetricsCodec>(
            self.ticks_dir("perp_metrics", venue, symbol),
            range,
            "",
            budget,
        )
    }

    fn resample_quotes_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let step = interval_ms(interval)?;
        let quotes = self.scan_quotes(venue, symbol, range)?; // ts-ascending
        let bars = consolidate_quotes(&quotes, step); // the parity-tested tick->bar math
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }

    fn resample_trades_to_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let step = interval_ms(interval)?;
        let trades = self.scan_trades(venue, symbol, range)?; // ts-ascending
        let bars = consolidate_trades(&trades, step);
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }
}

#[path = "compaction_encode_tests.rs"]
#[cfg(test)]
mod compaction_encode_tests;

#[path = "unwritable_root_tests.rs"]
#[cfg(test)]
mod unwritable_root_tests;

#[path = "plan_merge_groups_tests.rs"]
#[cfg(test)]
mod plan_merge_groups_tests;

#[path = "parse_series_id_tests.rs"]
#[cfg(test)]
mod parse_series_id_tests;
