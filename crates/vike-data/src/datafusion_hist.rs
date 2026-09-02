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
//! (crash-recovery log + replay), [`mod@query`] (DataFusion read/streaming paths), [`mod@codec`]
//! (Arrow `RecordBatch` <-> domain-type schemas/encoders/decoders for bars/quotes/trades/book). This
//! file owns the `DataFusionHist` type itself plus the operations that don't belong to one of those
//! (open/ingest orchestration, compaction, retention, the `HistStore` trait impl) and the small set
//! of path/write helpers all four submodules share.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use datafusion::arrow::array::ArrayRef;
use datafusion::arrow::datatypes::Schema;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::basic::{Compression, ZstdLevel};
use datafusion::parquet::file::metadata::KeyValue;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::prelude::{col, ParquetReadOptions, SessionContext};
use tokio::runtime::Runtime;

use vike_model::{
    consolidate_quotes, consolidate_trades, Bar, BookUpdate, EquitySample, QuoteTick,
    SymbolProperties, TradeTick,
};

use crate::chain_log::ChainRow;
use crate::cohort_log::CohortRow;
use crate::exec_log::{ExecFillRow, ExecOrderRow};
use crate::funding_log::FundingRow;
use crate::hist::{DataError, HistStore, TsRange};
use crate::hist_maint::{
    CompactionConfig, CompactionReport, Durability, MaintenanceConfig, MaintenanceReport,
    PruneReport, RetentionPolicy, SeriesMaintenance, SourceRankPolicy, WriteOpts, WriteProfile,
    GROUPED_ROW_GROUP_ROWS,
};
use crate::perp_metrics_log::PerpMetricRow;
use crate::series::{SeriesCoverage, SeriesId};

mod bulk;
mod codec;
mod gaps;
// The STORE's persisted source-precedence rule (`_sources.json`) — what lets `run_maintenance`
// resolve two writers' overlapping rows on its own. Absent = today's behaviour, byte-identical.
mod manifest;
mod query;
pub mod sources;
mod wal;

pub use bulk::{BulkConfig, BulkFlushReport, BulkIngestSession, GroupResolver};
// `find_gaps` is re-exported from the crate ROOT out of `coverage` (ungated) rather than here —
// see `gaps.rs`. Leaving a second gated path to the same function would make `vike_data::find_gaps`
// mean different things depending on features.
pub use manifest::RebuildReport;
pub use query::RowStream;
pub use sources::{load_policy, save_policy, StoreSourcePolicy};

use codec::{
    book_rows, book_updates_from_rows, f64_col, i64_col, BarCodec, BookCodec, ChainCodec,
    CohortCodec, EquityCodec, ExecFillCodec, ExecOrderCodec, FundingCodec, PerpMetricsCodec,
    PropertiesCodec, QuoteCodec, SeriesCodec, TradeCodec,
};
use gaps::{day_gap_to_ms_range, parse_utc_date};
use manifest::{merge_tmp_name, read_manifest, seal_into_manifest, write_manifest, SeriesLock};
use wal::{wal_append, wal_rewrite_keeping_unapplied};

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
}

impl DataFusionHist {
    /// Open (creating the root dir if absent). Owns a tokio runtime so the sync [`HistStore`]
    /// methods drive DataFusion's async engine via `block_on` — callers stay sync. Runs WAL crash
    /// [`recovery`](Self::recover) once before returning, so any append that crashed after its WAL
    /// fsync but before its manifest publish is re-applied and visible.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DataError> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(|e| unwritable_store_root(&root, &e))?;
        let rt = Runtime::new().map_err(io)?;
        let store = Self {
            root,
            rt,
            skip_publish_for_test: AtomicBool::new(false),
            stop_after_merge_for_test: AtomicBool::new(false),
            pause_in_merge_for_test: Mutex::new(None),
        };
        store.recover()?;
        Ok(store)
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

    /// The leaf directory for a series: `kind=…/venue=…/symbol=…[/interval=…]`. Parts live one
    /// level deeper under `date=…` (see [`part_dir`]).
    fn series_dir(&self, kind: &str, venue: &str, symbol: &str, interval: Option<&str>) -> PathBuf {
        let mut p = self
            .root
            .join(format!("kind={kind}"))
            .join(format!("venue={venue}"))
            .join(format!("symbol={symbol}"));
        if let Some(iv) = interval {
            p = p.join(format!("interval={iv}"));
        }
        p
    }
    fn bars_dir(&self, venue: &str, symbol: &str, interval: &str) -> PathBuf {
        self.series_dir("bar", venue, symbol, Some(interval))
    }
    fn ticks_dir(&self, kind: &str, venue: &str, symbol: &str) -> PathBuf {
        self.series_dir(kind, venue, symbol, None)
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
    fn series_dir_of(&self, id: &SeriesId) -> PathBuf {
        match &id.group {
            Some(g) => self.group_dir(&id.kind, &id.venue, g),
            None => self.series_dir(&id.kind, &id.venue, &id.symbol, id.interval.as_deref()),
        }
    }

    fn group_dir(&self, kind: &str, venue: &str, group: &str) -> PathBuf {
        self.root
            .join(format!("kind={kind}"))
            .join(format!("venue={venue}"))
            .join(format!("group={group}"))
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
    fn group_dirs(&self, kind: &str, venue: &str) -> Result<Vec<PathBuf>, DataError> {
        let parent = self.root.join(format!("kind={kind}")).join(format!("venue={venue}"));
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

    /// Scan one symbol across BOTH layouts: its own per-symbol series (today's path, checked first
    /// and unchanged) plus any grouped series for the same `kind`/`venue`.
    ///
    /// Grouped parts hold many symbols, so their rows are filtered by `keep`. The codec's decode
    /// already resolves each row's own `symbol_col`, so that filter is a plain equality check on
    /// decoded rows rather than anything schema-aware. A store with no grouped series does exactly
    /// what it did before — one `scan_series` call, then an empty directory listing.
    fn scan_symbol_across_layouts<C: SeriesCodec>(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        range: TsRange,
        keep: impl Fn(&C::Row) -> bool,
    ) -> Result<Vec<C::Row>, DataError> {
        let mut out = self.scan_series::<C>(&self.ticks_dir(kind, venue, symbol), range, symbol)?;
        let groups = self.group_dirs(kind, venue)?;
        if groups.is_empty() {
            return Ok(out); // no grouped series — byte-identical to the pre-grouping path
        }
        for dir in groups {
            // Push the symbol predicate INTO the read so DataFusion prunes row groups by the
            // `symbol_col` statistics; `keep` then only has to drop whatever survives a row group
            // that genuinely straddles two symbols.
            let rows = self.scan_series_for::<C>(&dir, range, symbol, Some(symbol))?;
            out.extend(rows.into_iter().filter(&keep));
        }
        out.sort_by_key(C::sort_key);
        Ok(out)
    }

    // ---- ingest: split a batch by UTC day, seal one part per date, publish once (idempotent) --

    /// Append `ts.len()` rows to `series_dir`, split into one sealed part per UTC `date=`, then
    /// publish the manifest ONCE (atomic). `build_cols(idxs)` builds the Arrow columns for a row
    /// subset — the caller indexes its own slice, so this stays schema-agnostic. Idempotent by
    /// `commit_key` (batch-level, never per-row value). Returns rows written (0 if the key was
    /// already committed or the batch is empty).
    fn commit_rows<F>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        ts: &[i64],
        schema: Arc<Schema>,
        build_cols: F,
        profile: WriteProfile,
    ) -> Result<usize, DataError>
    where
        F: Fn(&[usize]) -> Vec<ArrayRef>,
    {
        let _guard = SeriesLock::acquire(series_dir)?;
        let mut m = read_manifest(series_dir)?;
        // idempotency: a batch key already committed is a no-op (NEVER dedup by row value)
        if let Some(k) = commit_key {
            if m.commits.iter().any(|c| c == k) {
                return Ok(0);
            }
        }
        if ts.is_empty() {
            return Ok(0);
        }
        // (1) WAL the accepted append + fsync it BEFORE sealing, so a crash before the manifest
        // publish (2) is recovered on next open. Keyed appends only — keyless has no key to guard a
        // replay against (see the WAL section), so it keeps today's manifest-boundary durability.
        if let Some(k) = commit_key {
            let all: Vec<usize> = (0..ts.len()).collect();
            let batch = RecordBatch::try_new(schema.clone(), build_cols(&all)).map_err(q)?;
            wal_append(series_dir, k, &batch)?;
        }
        // (2a) seal one parquet part per UTC day into the manifest struct (still in memory)
        let written = seal_into_manifest(
            series_dir,
            &mut m,
            commit_key,
            ts,
            &schema,
            &build_cols,
            WriteOpts { profile, durability: Durability::Fsync },
        )?;
        if self.skip_publish_for_test.load(Ordering::SeqCst) {
            // TEST-ONLY: parts sealed + WAL fsynced, but the manifest is NOT published — exactly the
            // (1)→(2) crash window. A fresh `open` must recover these rows from the WAL.
            return Ok(written);
        }
        // (2b) publish the manifest by atomic rename — the durability boundary
        write_manifest(series_dir, &m, Durability::Fsync)?;
        // (3) the commit_key is now durable in the manifest → GC the applied WAL record(s)
        if commit_key.is_some() {
            wal_rewrite_keeping_unapplied(series_dir, &m)?;
        }
        Ok(written)
    }

    // ---- append_series / scan_series: the SeriesCodec-generic shape behind every HistStore ------
    // append_*/scan_*/load_bars method (dedup site: these were 5 nearly-identical append bodies and
    // 4 nearly-identical scan bodies, differing only in which `codec::` schema/columns/decode trio
    // ran). `commit_rows` above already took its `schema`/`build_cols` as parameters — these two
    // just supply them FROM the codec, so behavior (schema Arc, column builders, decoders, WAL/
    // manifest path) is unchanged; see [`codec::SeriesCodec`] for the byte-identity argument.

    /// Append MANY symbols' rows to ONE grouped series in a SINGLE commit — the write shape
    /// storage-study item #5 exists for.
    ///
    /// **This, not the `group=` path, is where the win lives.** Pointing the existing per-symbol
    /// verbs at a shared directory would be strictly WORSE than today: 562 `append_quotes` calls
    /// against one series dir contend on ONE `SeriesLock` and still pay 562 commits, where today's
    /// 562 independent series at least lock independently. The 201x measured in PR #938 came from
    /// one call carrying all the rows — which is exactly what this verb is.
    ///
    /// Every row carries its own symbol (the additive `symbol_col` column), so one part holds the
    /// whole group and reads tell rows apart by that column instead of by the path.
    ///
    /// **Rejects rows with an empty symbol.** On a per-symbol path an empty symbol is legal and
    /// means "tag me from the path" — here there is no such path, so an empty symbol would make a
    /// row permanently unattributable and silently invisible to every `scan_*(venue, symbol)`.
    /// Enforced writer-side rather than papered over on read.
    /// Quotes for many symbols → one grouped series, one commit.
    pub fn append_quotes_grouped(
        &self,
        venue: &str,
        group: &str,
        ticks: &[QuoteTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_grouped::<QuoteCodec>("quote", venue, group, ticks, commit_key, |r| &r.symbol)
    }

    /// Trades for many symbols → one grouped series, one commit.
    pub fn append_trades_grouped(
        &self,
        venue: &str,
        group: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        self.append_grouped::<TradeCodec>("trade", venue, group, ticks, commit_key, |r| &r.symbol)
    }

    /// Book updates for many symbols → one grouped series, one commit.
    ///
    /// **The one that matters by volume.** A Polymarket token's market is ~352,935 book rows against
    /// ~17,364 quotes and ~1,087 trades, so grouping quotes and trades alone would leave ~95% of the
    /// data still committing per symbol.
    ///
    /// Explodes to per-level rows exactly like the per-symbol path — each row now carries its own
    /// symbol, so the regroup on read reconstructs every event under the right one.
    pub fn append_book_updates_grouped(
        &self,
        venue: &str,
        group: &str,
        updates: &[BookUpdate],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        if let Some(bad) = updates.iter().position(|u| u.symbol.is_empty()) {
            return Err(DataError::Query(format!(
                "append_book_updates_grouped({venue}/{group}): update {bad} has an empty symbol — a \
                 grouped series tells rows apart by their symbol column, so an empty one is \
                 unattributable and would be invisible to every scan"
            )));
        }
        let rows = book_rows(updates);
        self.append_grouped::<BookCodec>("book", venue, group, &rows, commit_key, |r| &r.symbol)
    }

    /// The generic behind [`Self::append_quotes_grouped`] / [`Self::append_trades_grouped`].
    /// Private because `SeriesCodec` is an internal seam — callers get the concrete verbs above.
    fn append_grouped<C: SeriesCodec>(
        &self,
        kind: &str,
        venue: &str,
        group: &str,
        rows: &[C::Row],
        commit_key: Option<&str>,
        symbol_of: impl Fn(&C::Row) -> &str,
    ) -> Result<usize, DataError>
    where
        C::Row: Clone,
    {
        if let Some(bad) = rows.iter().position(|r| symbol_of(r).is_empty()) {
            return Err(DataError::Query(format!(
                "append_grouped({kind}/{venue}/{group}): row {bad} has an empty symbol — a grouped \
                 series tells rows apart by their symbol column, so an empty one is unattributable \
                 and would be invisible to every scan"
            )));
        }
        // SORT SYMBOL-MAJOR. This is the single most load-bearing line in the grouped write path,
        // and it is not tidiness: a one-symbol read prunes row groups by the `symbol_col`
        // statistics, and in a ts-ordered part with symbols interleaved EVERY row group spans every
        // symbol — each min/max covers everything and nothing prunes. Measured without it: 562
        // per-symbol scans took 20,945 ms against a grouped series vs 2,731 ms per-symbol (7.7x
        // slower), which ate almost all of the 140x write win and left a NET of 1.15x.
        //
        // `(symbol, ts)` rather than `(ts, …)` costs nothing downstream: the manifest's ts_min/max
        // come from the ts VALUES, not from row order, and every scan re-sorts its own output by
        // `C::sort_key` before returning. The archive files are `token_id`-sorted for the same
        // reason — it is what gets a 1-of-562 read down to 8.3% of compressed bytes.
        let mut ordered: Vec<usize> = (0..rows.len()).collect();
        ordered.sort_by(|&a, &b| {
            symbol_of(&rows[a])
                .cmp(symbol_of(&rows[b]))
                .then_with(|| C::sort_key(&rows[a]).cmp(&C::sort_key(&rows[b])))
        });
        let sorted: Vec<C::Row> = ordered.into_iter().map(|i| rows[i].clone()).collect();
        self.append_series::<C>(
            &self.group_dir(kind, venue, group),
            commit_key,
            &sorted,
            WriteProfile::Grouped,
        )
    }

    /// Append `rows` to `series_dir`, `C`-encoded. The `ts` vector `commit_rows` needs for
    /// date-splitting comes from `C::sort_key(row).0` — the same `ts` every codec's original
    /// `append_*` body computed by hand (`rows.iter().map(|r| r.ts).collect()` / `.0` for properties).
    fn append_series<C: SeriesCodec>(
        &self,
        series_dir: &Path,
        commit_key: Option<&str>,
        rows: &[C::Row],
        profile: WriteProfile,
    ) -> Result<usize, DataError> {
        let ts: Vec<i64> = rows.iter().map(|r| C::sort_key(r).0).collect();
        self.commit_rows(
            series_dir,
            commit_key,
            &ts,
            C::schema(),
            |idxs| C::columns(rows, idxs),
            profile,
        )
    }

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

    // ---- maintenance: compaction + retention (backend ops, not part of the HistStore seam) --

    /// Decode the given parts to domain rows, sort by ts, and RE-ENCODE with the CURRENT schema for
    /// `kind` — the decode-time schema-upgrade compaction path (book-recording plan Task 4). Parts
    /// are read per file (schema-tolerant, via [`Self::read_batches_per_file`]), so inputs written
    /// under an older schema revision (missing a later-added nullable column) decode under their own
    /// schema; re-encoding through `C::schema()` + `C::columns()` (dispatched below by `kind`) writes
    /// the merged part under today's schema, upgrading them. The f64 decode→encode is bit-exact
    /// (`Float64Array::value(i)` → `Float64Array::from`), so the store's `to_bits()` parity gate
    /// holds across a compaction. The domain `sort_by_key` is stable, keeping input (file) order
    /// among equal-key rows.
    fn compact_roundtrip(
        &self,
        kind: &str,
        urls: Vec<String>,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>), DataError> {
        // Dispatch: `kind` is a runtime string (from `SeriesId`/the on-disk `kind=` path segment),
        // so the match is the unavoidable runtime->type boundary; each arm is otherwise a one-liner
        // onto the ONE generic rewrite (`compact_roundtrip_generic`), byte-identical to the former
        // per-kind bodies (same decode fns via `SeriesCodec`, same sort key, same schema/columns).
        // Ticks/equity re-encode with NO symbol/venue column (see `quote_columns`/`equities_to_batch`
        // etc.), so the decode-time context is irrelevant here — pass ""; a later real scan
        // re-injects the true symbol/venue from its own argument.
        match kind {
            "bar" => self.compact_roundtrip_generic::<BarCodec>(urls, ""),
            "quote" => self.compact_roundtrip_generic::<QuoteCodec>(urls, ""),
            "trade" => self.compact_roundtrip_generic::<TradeCodec>(urls, ""),
            "properties" => self.compact_roundtrip_generic::<PropertiesCodec>(urls, ""), // (was kind=filters; renamed with SymbolFilters→SymbolProperties)
            "equity" => self.compact_roundtrip_generic::<EquityCodec>(urls, ""),
            // Execution trade-log kinds (Tier-2). Every field is a stored column (venue+symbol
            // included), so the `ctx=""` re-encode is lossless — see the codec note.
            "exec_fill" => self.compact_roundtrip_generic::<ExecFillCodec>(urls, ""),
            "exec_order" => self.compact_roundtrip_generic::<ExecOrderCodec>(urls, ""),
            // Realized funding (Tier-2): every field is a stored column, so the `ctx=""` re-encode is
            // lossless — see the codec note.
            "funding" => self.compact_roundtrip_generic::<FundingCodec>(urls, ""),
            // Option-chain snapshots (PIT options surface): every field is a stored column
            // (underlying included), so the `ctx=""` re-encode is lossless — see the codec note.
            "chain" => self.compact_roundtrip_generic::<ChainCodec>(urls, ""),
            // Cohort open interest (the graded positioning panel): every field is a stored column
            // (asset included), so the `ctx=""` re-encode is lossless — see the codec note.
            "cohort" => self.compact_roundtrip_generic::<CohortCodec>(urls, ""),
            // Perp market-context metrics: `(venue, symbol)` is the path and nothing else is an
            // identity column, so the `ctx=""` re-encode is lossless — see the codec note.
            "perp_metrics" => self.compact_roundtrip_generic::<PerpMetricsCodec>(urls, ""),
            // Per-level ROWS are the domain unit for the book kind — a row-level rewrite preserves
            // every event (no regroup needed); `BookCodec::sort_key` is `(ts, seq)`, keeping rows of
            // one event adjacent and in feed order, exactly as `scan_book_updates` does.
            // `depth` shares BookCodec: same rows, different (conflating) lane — see `append_depth`.
            "book" | "depth" => self.compact_roundtrip_generic::<BookCodec>(urls, ""),
            other => Err(DataError::Query(format!("compaction: unknown series kind {other:?}"))),
        }
    }

    /// Decode every batch, sort by `C::sort_key`, and re-encode ONE part under `C`'s CURRENT schema
    /// — the shared decode→sort→re-encode rewrite behind every `compact_roundtrip` arm.
    fn compact_roundtrip_generic<C: SeriesCodec>(
        &self,
        urls: Vec<String>,
        ctx: &str,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>), DataError> {
        let batches = self.read_batches_per_file(urls)?;
        let mut rows: Vec<C::Row> = Vec::new();
        for b in &batches {
            rows.extend(C::decode(b, ctx)?);
        }
        rows.sort_by_key(C::sort_key);
        let schema = C::schema();
        Ok((schema.clone(), encode_chunked::<C>(&rows, &schema)?))
    }

    /// SOURCE-RANKED supersession twin of [`Self::compact_roundtrip`] — the opt-in variant that,
    /// after tagging each part's rows with that part's source `rank` (parallel to `urls`, in the
    /// same order), keeps ONE row per natural key by the lowest rank present and drops the
    /// superseded duplicates. Same runtime `kind`→type dispatch as `compact_roundtrip`; each arm is
    /// a one-liner onto [`Self::compact_roundtrip_superseding_generic`]. Returns the re-encoded part
    /// PLUS the count of rows dropped as superseded. (`ctx=""` for every arm, exactly like
    /// `compact_roundtrip` — see its note.)
    fn compact_roundtrip_superseding(
        &self,
        kind: &str,
        urls: Vec<String>,
        ranks: &[usize],
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, usize), DataError> {
        let ranked: Vec<(String, usize)> = urls.into_iter().zip(ranks.iter().copied()).collect();
        match kind {
            "bar" => self.compact_roundtrip_superseding_generic::<BarCodec>(ranked, ""),
            "quote" => self.compact_roundtrip_superseding_generic::<QuoteCodec>(ranked, ""),
            "trade" => self.compact_roundtrip_superseding_generic::<TradeCodec>(ranked, ""),
            "properties" => {
                self.compact_roundtrip_superseding_generic::<PropertiesCodec>(ranked, "")
            }
            "equity" => self.compact_roundtrip_superseding_generic::<EquityCodec>(ranked, ""),
            "exec_fill" => self.compact_roundtrip_superseding_generic::<ExecFillCodec>(ranked, ""),
            "exec_order" => {
                self.compact_roundtrip_superseding_generic::<ExecOrderCodec>(ranked, "")
            }
            "funding" => self.compact_roundtrip_superseding_generic::<FundingCodec>(ranked, ""),
            "chain" => self.compact_roundtrip_superseding_generic::<ChainCodec>(ranked, ""),
            // ⚠ `CohortCodec::sort_key` is `(ts, 0)`, so a supersession RUN is a whole HOUR — every
            // label, axis, grading and basis recorded at that ts. [`supersede_by_rank`] keeps every
            // row sharing the run's minimum rank, so a single-source series (which is all this kind
            // has today) drops nothing; but a SECOND source ranked above the first would supersede
            // that hour whole, taking axes and gradings the winning source never served. Opt-in
            // only — plain `compact_series` cannot reach this arm — and the reason a cohort store
            // fed by two sources wants `compact_series`, not its superseding twin.
            "cohort" => self.compact_roundtrip_superseding_generic::<CohortCodec>(ranked, ""),
            // `PerpMetricsCodec::sort_key` is `(ts, 0)` and one funding interval is exactly ONE row
            // here, so a supersession run is a single observation and the higher-ranked source
            // simply wins it — the cohort arm's whole-hour hazard above has no analogue.
            "perp_metrics" => {
                self.compact_roundtrip_superseding_generic::<PerpMetricsCodec>(ranked, "")
            }
            "book" | "depth" => self.compact_roundtrip_superseding_generic::<BookCodec>(ranked, ""),
            other => Err(DataError::Query(format!("compaction: unknown series kind {other:?}"))),
        }
    }

    /// Decode each `(url, rank)` part PER FILE (so a row keeps its part's source rank — the row
    /// schema carries no source column, so the tag must be attached before the merge), STABLE-sort
    /// the tagged rows by `C::sort_key`, then [`supersede_by_rank`] keeps one source per natural-key
    /// run and re-encodes ONE part under `C`'s current schema. Byte-identical to
    /// [`Self::compact_roundtrip_generic`] when no run holds a cross-source collision (every row in
    /// a run shares the run's minimum rank → nothing dropped, same stable order).
    fn compact_roundtrip_superseding_generic<C: SeriesCodec>(
        &self,
        ranked_urls: Vec<(String, usize)>,
        ctx: &str,
    ) -> Result<(Arc<Schema>, Vec<RecordBatch>, usize), DataError> {
        let mut ranked: Vec<(usize, C::Row)> = Vec::new();
        for (url, rank) in ranked_urls {
            // per-file read keeps the part→rank association the flat `read_batches_per_file` loses
            let batches = self.read_batches_per_file(vec![url])?;
            for b in &batches {
                for row in C::decode(b, ctx)? {
                    ranked.push((rank, row));
                }
            }
        }
        // STABLE sort by the natural key — identical ordering to the non-superseding path's
        // `rows.sort_by_key(C::sort_key)`, so a collision-free series stays byte-identical.
        ranked.sort_by_key(|(_, row)| C::sort_key(row));
        let (rows, superseded) = supersede_by_rank::<C>(ranked);
        let schema = C::schema();
        Ok((schema.clone(), encode_chunked::<C>(&rows, &schema)?, superseded))
    }

    /// Merge the small fragments in each `date=` partition of a series into one sealed, ts-sorted
    /// part (reduces read fan-out). Manifest-first publish: the new part + rewritten manifest land
    /// atomically, THEN the superseded fragments are unlinked (a crash leaves orphans the READ path
    /// cannot see, never a dangling manifest reference — and, since manifest recovery reads the
    /// directory instead, orphans it can now tell apart; see [`manifest::rebuild_manifest`]). The
    /// commit-log is untouched — compaction never drops
    /// or resurrects a committed batch (must-fix #5). Row content is preserved EXACTLY (every
    /// duplicate kept); [`Self::compact_series_superseding`] is the opt-in variant that drops
    /// source-superseded duplicates.
    pub fn compact_series(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
    ) -> Result<CompactionReport, DataError> {
        self.compact_series_inner(kind, venue, symbol, interval, cfg, None)
    }

    /// OPT-IN source-ranked window supersession compaction — the same per-`date=` merge as
    /// [`Self::compact_series`], but when two overlapping capture windows record the SAME natural
    /// key (`(ts, seq)` for a book event; `ts` for a trade/quote/bar) exactly one row survives per
    /// `policy`'s source precedence and the superseded duplicate is DROPPED (counted in
    /// [`CompactionReport::rows_superseded`]) rather than double-counted in a later scan/replay. A
    /// part's source is derived from its commit-key namespace ([`SourceRankPolicy::rank_of`]); rows
    /// are tagged with their part's rank BEFORE the merge-sort (the row schema carries no source
    /// column, so this needs no codec/schema change).
    ///
    /// This CHANGES stored output, so it is a deliberate maintenance operation the operator runs for
    /// a multi-writer series (the four Polymarket writers on one `venue=polymarket/symbol=<token_id>`
    /// series) — NOT part of the always-on [`Self::run_maintenance`] pass. When `policy` produces no
    /// cross-source collision in any natural-key run (e.g. a single-source series, or an empty
    /// policy) it drops nothing and is byte-identical to [`Self::compact_series`].
    pub fn compact_series_superseding(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
        policy: &SourceRankPolicy,
    ) -> Result<CompactionReport, DataError> {
        self.compact_series_inner(kind, venue, symbol, interval, cfg, Some(policy))
    }

    /// Shared body of [`Self::compact_series`] (`ranks = None`) and
    /// [`Self::compact_series_superseding`] (`ranks = Some`). With `None` the per-date rewrite is the
    /// duplicate-preserving [`Self::compact_roundtrip`]; with `Some(policy)` it is the
    /// source-superseding [`Self::compact_roundtrip_superseding`], each part's rank derived from its
    /// `commit_keys`. Everything else (per-`date=` threshold, manifest-first publish, commit-key
    /// union carried forward, unlink-after-publish) is identical for both.
    fn compact_series_inner(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        interval: Option<&str>,
        cfg: &CompactionConfig,
        ranks: Option<&SourceRankPolicy>,
    ) -> Result<CompactionReport, DataError> {
        self.compact_dir_inner(&self.series_dir(kind, venue, symbol, interval), kind, cfg, ranks)
    }

    /// [`Self::compact_series_inner`] over an explicit series DIR, so a GROUPED series
    /// (`group=…`, which has no `symbol=` segment) can be compacted too. Maintenance resolves the
    /// dir from the `SeriesId` — see `series_dir_of`.
    /// ## The lock is held for the PLAN and the PUBLISH, never for the merge
    ///
    /// This used to hold [`SeriesLock`] across the whole decode→sort→re-encode→write loop. On a
    /// large series that outlasts `SeriesLock::acquire`'s ~4 s spin, so a concurrent
    /// `RecorderSink` flush times out — and its failure path **discards the buffer**, up to
    /// `max_rows` (5,000) rows. Measured on the CI box (2026-08-02): **110,354 rows discarded in about an
    /// hour** from one 52 MB `kind=book` series, while `hist_sched`'s own doc claimed compaction was
    /// "safe to run alongside live appends… loses no rows". True while compaction is fast; false
    /// exactly when a series is big enough to be worth compacting.
    ///
    /// So: take the lock to decide, RELEASE it to do the work, take it again to swap.
    ///
    /// **Why the unlocked merge is safe.** Compaction removes exactly the input files it read, BY
    /// NAME, and adds one output containing their rows. A concurrent append creates a differently
    /// named part, so it is untouched by the swap and survives. The publish phase re-reads the
    /// manifest and verifies every input is still listed; if any vanished (another pass merged them,
    /// or retention pruned them) that date is ABANDONED and its orphan output deleted, rather than
    /// publishing a manifest that references files someone else already removed.
    ///
    /// Output names carry a `part-c` prefix, so they cannot collide with an append's `part-NNNNN`
    /// even though the merge runs unlocked.
    ///
    /// **And they are not written under that name.** The merge writes
    /// [`manifest::MERGE_TMP_PREFIX`]`+part-c…` and the publish RENAMES it, so for the whole unlocked
    /// window the output is distinguishable from the inputs it duplicates by name alone. That is not
    /// cosmetic: the inputs are still live in the manifest and hold the same rows, so anything
    /// reading the DIRECTORY (as manifest recovery must) would otherwise count them twice — measured
    /// as 34–68% row inflation across a SIGKILL soak. `rebuild_manifest` carries the other half of
    /// the argument, for the window after this publish and before the unlinks below.
    fn compact_dir_inner(
        &self,
        series_dir: &Path,
        kind: &str,
        cfg: &CompactionConfig,
        ranks: Option<&SourceRankPolicy>,
    ) -> Result<CompactionReport, DataError> {
        let series_dir = series_dir.to_path_buf();
        let mut report = CompactionReport::default();
        if !series_dir.exists() {
            return Ok(report);
        }

        // ---- PHASE 1: PLAN (locked, fast — a manifest read and some arithmetic) ----------------
        struct DatePlan {
            date: String,
            /// The input part names, in manifest order — the identity the publish phase re-verifies.
            inputs: Vec<String>,
            urls: Vec<String>,
            file_ranks: Vec<usize>,
            ts_min: i64,
            ts_max: i64,
            commit_keys: Vec<String>,
            out_name: String,
        }

        let plans: Vec<DatePlan> = {
            let _guard = SeriesLock::acquire(&series_dir)?;
            let m = read_manifest(&series_dir)?;
            let dates: BTreeSet<String> = m.files.iter().map(|f| f.date.clone()).collect();
            let mut plans = Vec::new();
            let mut group_seq = 0usize;
            for date in dates {
                let idxs: Vec<usize> = m
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.date == date)
                    .map(|(i, _)| i)
                    .collect();
                if idxs.len() < cfg.min_parts {
                    continue; // not worth compacting this date yet
                }
                let dir = part_dir(&series_dir, &date);
                for group in plan_merge_groups(&dir, &m.files, &idxs, cfg) {
                    let mut commit_keys: Vec<String> =
                        group.iter().flat_map(|&i| m.files[i].commit_keys.clone()).collect();
                    commit_keys.sort();
                    commit_keys.dedup();
                    plans.push(DatePlan {
                        urls: group
                            .iter()
                            .map(|&i| file_url(&dir.join(&m.files[i].name)))
                            .collect(),
                        file_ranks: match ranks {
                            None => Vec::new(),
                            Some(p) => {
                                group.iter().map(|&i| p.rank_of(&m.files[i].commit_keys)).collect()
                            }
                        },
                        ts_min: group.iter().map(|&i| m.files[i].ts_min).min().expect("non-empty"),
                        ts_max: group.iter().map(|&i| m.files[i].ts_max).max().expect("non-empty"),
                        inputs: group.iter().map(|&i| m.files[i].name.clone()).collect(),
                        // `part-c` prefix: cannot collide with an append's `part-NNNNN`, which is
                        // what makes it safe to write this while unlocked. The `-NNN` suffix keeps
                        // one pass's several groups apart; `next_part_name` ignores the whole name
                        // either way (it wants a bare u64, and neither form parses as one).
                        out_name: format!("part-c{:08}-{group_seq:03}.parquet", m.version + 1),
                        commit_keys,
                        date: date.clone(),
                    });
                    group_seq += 1;
                }
            }
            plans
        }; // <-- lock released; the expensive part runs without it

        if plans.is_empty() {
            return Ok(report);
        }

        // ---- PHASE 2: MERGE (UNLOCKED — the decode/sort/re-encode that used to block writers) ---
        struct Merged {
            plan: DatePlan,
            rows: usize,
            superseded: usize,
        }
        let mut done: Vec<Merged> = Vec::new();
        for plan in plans {
            // decode→sort→re-encode with the current schema — bounded to ONE date (NOT a global
            // ORDER BY over the series), and upgrades any older-schema inputs on the way out. With a
            // supersession policy, tag each part's rows with its source rank (from that part's
            // commit-key namespace) and drop cross-source duplicates.
            let (schema, batches, superseded) = match ranks {
                None => {
                    let (schema, batches) = self.compact_roundtrip(kind, plan.urls.clone())?;
                    (schema, batches, 0)
                }
                Some(_) => {
                    self.compact_roundtrip_superseding(kind, plan.urls.clone(), &plan.file_ranks)?
                }
            };
            let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            if rows == 0 {
                continue;
            }
            // Under the UNPUBLISHED name — the merge output and every input it copied are in this
            // directory together until phase 3 renames it, and anything that reads the directory
            // rather than the manifest (there is one such caller: `rebuild_manifest`) has to be able
            // to tell them apart. See [`manifest::MERGE_TMP_PREFIX`].
            write_parquet(
                &part_dir(&series_dir, &plan.date).join(merge_tmp_name(&plan.out_name)),
                schema,
                batches,
                WriteProfile::Sealed,
                &plan.commit_keys,
            )?;
            done.push(Merged { plan, rows, superseded });
        }

        if done.is_empty() {
            return Ok(report);
        }
        // TEST-ONLY: park HERE — every merge output is written, no lock is held, and the publish
        // has not begun. That is the window an append has to be able to land in, and holding it
        // open is what lets a test WITNESS the overlap instead of racing for it. See
        // `pause_in_merge_for_test`. The guard is dropped before the wait so a second compaction
        // meets an empty seam rather than this thread's mutex.
        let paused = self.pause_in_merge_for_test.lock().expect("pause seam mutex").take();
        if let Some((parked, resume)) = paused {
            let _ = parked.send(());
            let _ = resume.recv();
        }
        if self.stop_after_merge_for_test.load(Ordering::SeqCst) {
            // TEST-ONLY: the merge outputs are on disk and NOTHING is published — the exact window a
            // SIGKILL mid-compaction leaves. See `stop_after_merge_for_test`.
            return Ok(report);
        }

        // ---- PHASE 3: PUBLISH (locked, fast — re-read, verify, swap) ---------------------------
        let _guard = SeriesLock::acquire(&series_dir)?;
        // RE-READ: the manifest may have advanced while we merged (that is the point of releasing).
        let mut m = read_manifest(&series_dir)?;
        let mut to_delete: Vec<PathBuf> = Vec::new();
        let mut drop_idx: BTreeSet<usize> = BTreeSet::new();
        let mut merged_entries: Vec<manifest::FileEntry> = Vec::new();

        for d in done {
            let dir = part_dir(&series_dir, &d.plan.date);
            // Every input must still be listed. If one is not, somebody else consumed it and our
            // output would reference rows they have already re-homed — abandon this date rather than
            // publish a manifest that disagrees with the disk.
            let idxs: Vec<usize> = d
                .plan
                .inputs
                .iter()
                // Match on (name, DATE): part names are unique within a `date=` dir, NOT across the
                // series — `part-00001.parquet` exists under every date, so matching on name alone
                // picks the first file with that name in ANY date and drops the wrong one.
                .filter_map(|name| {
                    m.files.iter().position(|f| &f.name == name && f.date == d.plan.date)
                })
                .collect();
            let tmp = dir.join(merge_tmp_name(&d.plan.out_name));
            if idxs.len() != d.plan.inputs.len() {
                tracing::warn!(
                    series = %series_dir.display(),
                    date = %d.plan.date,
                    "compaction: inputs changed under an unlocked merge — abandoning this date's \
                     output (it will be retried next pass)"
                );
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            // PUBLISH the output by rename, while holding the lock and BEFORE the manifest that
            // names it. Until this instant the file carried [`manifest::MERGE_TMP_PREFIX`] and was
            // provably not part of the series; after it, the manifest write below makes it so. A
            // rename that fails abandons this date exactly like a failed verify — better to redo the
            // merge next pass than to leave a final-named orphan a rebuild would count twice.
            //
            // The remove-then-retry is the same fallback [`write_manifest`] uses, and it is not
            // theoretical here: `rename` REPLACES on unix but FAILS on Windows when the destination
            // exists, and a destination CAN exist — as an orphan left by a build that wrote the
            // final name directly, or by a crash between this rename and the publish below. It is
            // never a LIVE part: `out_name` embeds `m.version + 1`, and every publish bumps the
            // version, so no manifest this store ever wrote can name the file being replaced.
            // Without the fallback such a date would abandon on every pass, forever.
            let out_path = dir.join(&d.plan.out_name);
            let mut renamed = std::fs::rename(&tmp, &out_path);
            if renamed.is_err() && out_path.exists() {
                let _ = std::fs::remove_file(&out_path);
                renamed = std::fs::rename(&tmp, &out_path);
            }
            if let Err(e) = renamed {
                tracing::warn!(
                    series = %series_dir.display(),
                    date = %d.plan.date,
                    error = %e,
                    "compaction: could not publish the merge output — abandoning this date"
                );
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            // On POSIX the rename is durable only once the containing directory is fsynced, and the
            // manifest published below is about to name it.
            fsync_dir(&dir)?;
            for &i in &idxs {
                to_delete.push(dir.join(&m.files[i].name));
                drop_idx.insert(i);
            }
            merged_entries.push(manifest::FileEntry {
                name: d.plan.out_name,
                date: d.plan.date,
                ts_min: d.plan.ts_min,
                ts_max: d.plan.ts_max,
                rows: d.rows,
                commit_keys: d.plan.commit_keys,
            });
            report.parts_merged += idxs.len();
            report.parts_written += 1;
            report.rows += d.rows;
            report.rows_superseded += d.superseded;
        }

        if merged_entries.is_empty() {
            return Ok(report); // every date was abandoned
        }
        // rebuild files: drop the merged inputs, add the sealed outputs
        let mut files: Vec<manifest::FileEntry> = std::mem::take(&mut m.files)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !drop_idx.contains(i))
            .map(|(_, f)| f)
            .collect();
        files.extend(merged_entries);
        m.files = files;
        m.version += 1;
        write_manifest(&series_dir, &m, Durability::Fsync)?;
        // manifest-first: only NOW unlink the superseded fragments
        for p in to_delete {
            let _ = std::fs::remove_file(p);
        }
        Ok(report)
    }

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
        self.apply_retention_at(&self.series_dir(kind, venue, symbol, interval), policy)
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
        let now = vike_model::clock::now_ms();
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
        // GC commit keys no surviving part still references (a spanning-day batch or a compacted
        // part may keep a key alive on another file that survives the prune)
        for f in &dropped {
            for k in &f.commit_keys {
                if !m.files.iter().any(|sf| sf.commit_keys.contains(k)) {
                    m.commits.retain(|c| c != k);
                }
            }
        }
        m.version += 1;
        write_manifest(&series_dir, &m, Durability::Fsync)?;
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

    /// Rebuild one series' manifest FROM ITS PARTS and publish it, replacing whatever is there.
    ///
    /// The manifest is a CACHE, not ground truth — this is the operation that proves it. A lost or
    /// corrupt `_manifest.json` used to make every part beneath it unreachable, because the read
    /// path deliberately never LISTs directories (spec must-fix #5): data sitting on disk was simply
    /// invisible, with no way back. Now it is one call.
    ///
    /// Precedent: NautilusTrader's `reset_all_file_names()` re-derives its filename index from
    /// Parquet row-group statistics; ArcticDB documents an explicit fallback to iterating storage
    /// "in case we have consistency issues in the ref keys". Both treat the index as regenerable.
    /// This store now does too.
    ///
    /// Takes the series lock, so it is safe against a concurrent append or compaction. Returns what
    /// was recovered AND what could not be — see [`manifest::RebuildReport`]; in particular, parts
    /// written before commit keys were stamped into footers come back readable but contribute
    /// nothing to the idempotency log, which is reported rather than hidden.
    pub fn rebuild_series_manifest(&self, id: &SeriesId) -> Result<RebuildReport, DataError> {
        let dir = self.series_dir_of(id);
        let _guard = SeriesLock::acquire(&dir)?;
        let (m, report) = manifest::rebuild_manifest(&dir)?;
        write_manifest(&dir, &m, Durability::Fsync)?;
        Ok(report)
    }

    /// Enumerate every series in the store: walk the root tree for leaf dirs holding a
    /// `_manifest.json`, and parse each one's `(kind, venue, symbol, interval)` straight back out of
    /// its `kind=…/venue=…/symbol=…[/interval=…]` path segments. Returned sorted (deterministic order
    /// for reproducible maintenance + stable test assertions).
    ///
    /// This is a MAINTENANCE walk (feeds [`Self::run_maintenance`]), NOT the hot read path — a
    /// directory scan is fine here, exactly like WAL [`recovery`](wal). A leaf mid-creation
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
        let m = read_manifest(&dir)?; // private, same module
        if m.files.is_empty() {
            return Ok(SeriesCoverage::default());
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
            let part = part_dir(&dir, &f.date).join(&f.name);
            if let Ok(md) = std::fs::metadata(&part) {
                bytes += md.len();
            }
        }
        Ok(SeriesCoverage {
            first_ts: first,
            last_ts: last,
            rows,
            bytes,
            parts: m.files.len(),
            dates: dates.len(),
        })
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
    /// hole" view. Derived purely from the manifest's `date=` file index ([`gaps::find_gaps`] over
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

    /// Move a PER-SYMBOL tick series into a GROUPED one, then delete the original.
    ///
    /// The repair for a store that has the same instrument in both layouts. That happens for two
    /// real reasons: a family recorded before grouping existed, and — the case this was written for —
    /// two recorder bugs that let rows escape to per-symbol series before they were fixed (a
    /// max-rows flush that skipped the group resolver, and a rotation that unmapped a symbol while
    /// its rows were still buffered).
    ///
    /// Such a store is not WRONG — `scan_*` reads both layouts, so no row is lost or hidden. It is
    /// untidy in ways that cost later: the coverage report lists one instrument twice (they are
    /// deliberately different `InstrumentKey`s), maintenance treats them as unrelated series, and
    /// every scan opens both.
    ///
    /// **Order is deliberate: copy, verify, THEN delete.** The rows are appended under a
    /// `migrate:{kind}:{symbol}` commit key (so re-running is a no-op rather than a double-append),
    /// the grouped read is checked to contain them, and only then is the source removed. A failure
    /// anywhere before that leaves BOTH copies — untidy, which is what it already was, rather than
    /// missing.
    ///
    /// Returns rows moved. `Ok(0)` when the source series does not exist or is empty.
    pub fn migrate_series_to_group(
        &self,
        kind: &str,
        venue: &str,
        symbol: &str,
        group: &str,
    ) -> Result<usize, DataError> {
        let key = format!("migrate:{kind}:{symbol}");
        let moved = match kind {
            "quote" => {
                // The PER-SYMBOL series only. `scan_quotes` spans BOTH layouts, so using it here
                // would re-read rows already in the group and copy them back into it — which is
                // exactly what made the first version non-idempotent.
                let rows = self.scan_series::<QuoteCodec>(
                    &self.ticks_dir("quote", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                if rows.is_empty() {
                    return Ok(0);
                }
                // Stamp the symbol: a grouped series tells rows apart by their symbol column, and a
                // per-symbol row may legally have left it empty (the path carried it).
                let rows: Vec<QuoteTick> = rows
                    .into_iter()
                    .map(|mut q| {
                        q.symbol = symbol.to_string();
                        q
                    })
                    .collect();
                self.append_quotes_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            "trade" => {
                let rows = self.scan_series::<TradeCodec>(
                    &self.ticks_dir("trade", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let rows: Vec<TradeTick> = rows
                    .into_iter()
                    .map(|mut t| {
                        t.symbol = symbol.to_string();
                        t
                    })
                    .collect();
                self.append_trades_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            "book" => {
                let raw = self.scan_series::<BookCodec>(
                    &self.ticks_dir("book", venue, symbol),
                    TsRange::all(),
                    symbol,
                )?;
                let rows = book_updates_from_rows(raw, symbol)?;
                if rows.is_empty() {
                    return Ok(0);
                }
                let rows: Vec<BookUpdate> = rows
                    .into_iter()
                    .map(|mut u| {
                        u.symbol = symbol.to_string();
                        u
                    })
                    .collect();
                self.append_book_updates_grouped(venue, group, &rows, Some(&key))?;
                rows.len()
            }
            other => {
                return Err(DataError::Query(format!(
                    "migrate_series_to_group: kind `{other}` has no grouped form"
                )))
            }
        };

        // VERIFY before deleting — read the GROUP directory specifically, not `scan_*`, which spans
        // both layouts and would largely re-report what we started with.
        //
        // Counted as ROWS, not events: a `book` event is one row per level, and two identical copies
        // of one event FOLD BACK into a single event on read (the regroup keys on `(ts, seq)`), so an
        // event-count comparison reads 1 where 2 were expected. That is how the first version of this
        // check failed on the book lane.
        let gdir = self.group_dir(kind, venue, group);
        let landed = match kind {
            "quote" => self
                .scan_series_for::<QuoteCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
            "trade" => self
                .scan_series_for::<TradeCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
            _ => self
                .scan_series_for::<BookCodec>(&gdir, TsRange::all(), symbol, Some(symbol))?
                .len(),
        };
        if landed == 0 {
            return Err(DataError::Query(format!(
                "migrate_series_to_group({kind}/{venue}/{symbol} -> {group}): the grouped write \
                 landed no rows — refusing to delete the source"
            )));
        }

        self.delete_series(&SeriesId::per_symbol(kind, venue, symbol, None))?;
        Ok(moved)
    }

    /// Delete an entire stored series — its `kind=/venue=/symbol=[/interval=]` leaf dir with the
    /// manifest, commit-log, and every Parquet part. **Irreversible.** Removes ONLY that one
    /// series' dir; siblings are untouched. Idempotent: an absent series is `Ok(())`. Powers the
    /// Data Manager's per-series Delete action (the GUI confirms first).
    pub fn delete_series(&self, id: &SeriesId) -> Result<(), DataError> {
        let dir = self.series_dir_of(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| DataError::Query(format!("delete series {}: {e}", dir.display())))?;
        }
        Ok(())
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
                let compaction =
                    self.compact_dir_inner(&dir, &series.kind, &cfg.compaction, ranks.as_ref())?;
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

    /// Ingest an EXTERNAL Parquet file (a venue export or the Python bar-cache) as bars for
    /// `(venue, symbol, interval)`. Reads `[ts, open, high, low, close, volume]` (funding = None),
    /// then appends via [`HistStore::append_bars`] — so it date-partitions and is idempotent by
    /// `commit_key` like any other append. `ts` is expected as Int64 epoch-ms. This keeps DataFusion
    /// contained in vike-data: consumers hand it a path, not a parquet reader.
    pub fn append_bars_from_parquet(
        &self,
        path: &Path,
        venue: &str,
        symbol: &str,
        interval: &str,
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        let url = file_url(path);
        let batches = self.rt.block_on(async move {
            let ctx = SessionContext::new();
            let df = ctx
                .read_parquet(vec![url], ParquetReadOptions::default())
                .await
                .map_err(q)?
                .select_columns(&["ts", "open", "high", "low", "close", "volume"])
                .map_err(q)?
                .sort_by(vec![col("ts")])
                .map_err(q)?;
            df.collect().await.map_err(q)
        })?;
        let mut bars = Vec::new();
        for b in &batches {
            let (ts, open, high, low, close, volume) = (
                i64_col(b, "ts")?,
                f64_col(b, "open")?,
                f64_col(b, "high")?,
                f64_col(b, "low")?,
                f64_col(b, "close")?,
                f64_col(b, "volume")?,
            );
            for i in 0..b.num_rows() {
                bars.push(Bar {
                    ts: ts.value(i),
                    open: open.value(i),
                    high: high.value(i),
                    low: low.value(i),
                    close: close.value(i),
                    volume: volume.value(i),
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                });
            }
        }
        self.append_bars(venue, symbol, interval, &bars, commit_key)
    }
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
/// `root`: `kind=…/venue=…/symbol=…[/interval=…]`. Returns `None` if `dir` isn't under `root`, if any
/// segment isn't one of the expected keys (so a stray dir can't masquerade as a series), or if the
/// `symbol=` segment is EMPTY (see the arm below — that value is the grouped-series sentinel, not a
/// symbol). The inverse of [`DataFusionHist::series_dir`].
fn parse_series_id(root: &Path, dir: &Path) -> Option<SeriesId> {
    let rel = dir.strip_prefix(root).ok()?;
    let (mut kind, mut venue, mut symbol, mut interval, mut group) = (None, None, None, None, None);
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
            // `crates/vike-data/src/series.rs`'s `SeriesId::group` RESERVES an empty `symbol` as the
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
            // retention-pruning it, and `vike-app`'s reconcile pre-seed — which iterates
            // `list_series()` and scans each `kind=exec_fill` series by `id.symbol` — stops folding
            // its trade ids into the seen-fill dedup set. That consumer is the sentinel doc's own
            // "silently scanning `\"\"`" case, so refusing is right; doing it QUIETLY would swap one
            // silence for another.
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
        } else if let Some(v) = seg.strip_prefix("group=") {
            group = Some(v.to_string());
        } else {
            return None; // unexpected segment → not a well-formed series leaf
        }
    }
    // A GROUPED leaf has no `symbol=` segment — it holds many symbols, told apart by the row-level
    // symbol column. Before this arm existed, `parse_series_id` returned None for such a dir, so
    // `list_series` silently SKIPPED grouped series and `run_maintenance` never compacted or
    // retention-pruned them. Latent rather than live (nothing wrote grouped series yet), and
    // exactly the kind of silent omission that only shows up as unbounded disk growth much later.
    if let Some(g) = group {
        return Some(SeriesId::grouped(kind?, venue?, g));
    }
    Some(SeriesId { kind: kind?, venue: venue?, symbol: symbol?, interval, group: None })
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
    // part — and the WAL cannot repair that, because the commit key is already in `m.commits`, which
    // makes the retry a no-op. [`Durability`] varies what happens ABOVE this line, never this line.
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
/// Polymarket window convention `<slug>#<outcome_index>` (`vike_backtest::CheapNp`'s symbol
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

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.scan_symbol_across_layouts::<QuoteCodec>("quote", venue, symbol, range, |r| {
            r.symbol == symbol
        })
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.scan_symbol_across_layouts::<TradeCodec>("trade", venue, symbol, range, |r| {
            r.symbol == symbol
        })
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

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, DataError> {
        DataFusionHist::series_gaps(self, id)
    }

    fn coverage_report(&self) -> Result<Vec<crate::coverage::InstrumentCoverage>, DataError> {
        DataFusionHist::coverage_report(self)
    }

    fn append_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        bars: &[Bar],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
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
        let mut rows =
            self.scan_series::<BookCodec>(&self.ticks_dir("depth", venue, symbol), range, symbol)?;
        for dir in self.group_dirs("depth", venue)? {
            let g = self.scan_series_for::<BookCodec>(&dir, range, symbol, Some(symbol))?;
            rows.extend(g.into_iter().filter(|r| r.symbol == symbol));
        }
        rows.sort_by_key(BookCodec::sort_key);
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
        let mut rows =
            self.scan_series::<BookCodec>(&self.ticks_dir("book", venue, symbol), range, symbol)?;
        for dir in self.group_dirs("book", venue)? {
            let g = self.scan_series_for::<BookCodec>(&dir, range, symbol, Some(symbol))?;
            rows.extend(g.into_iter().filter(|r| r.symbol == symbol));
        }
        rows.sort_by_key(BookCodec::sort_key);
        book_updates_from_rows(rows, symbol)
    }

    fn append_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        rows: &[(i64, SymbolProperties)],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
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
        // `vike-app`'s reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds
        // the trade_ids it finds into the SEEN-FILL dedup set. A fill that never reaches that set is
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
        // ⚠ `vike_ops::journal_mat`'s `materialize_once` guards its ORDER path
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
        // than merely unaddressable: `crates/vike-data/src/series.rs`'s `SeriesId::group` reserves an
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
            &self.ticks_dir("funding", venue, symbol),
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
        self.scan_series::<FundingCodec>(&self.ticks_dir("funding", venue, symbol), range, "")
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

#[cfg(test)]
mod compaction_encode_tests {
    use super::*;
    use crate::datafusion_hist::codec::BarCodec;
    use vike_model::Bar;

    fn bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| Bar {
                ts: i as i64 * 1000,
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: 1.5,
                volume: 10.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT.binance".to_string()),
            })
            .collect()
    }

    /// The regression for the the CI box `offset overflow` panic: one `date=` partition must NOT be
    /// re-encoded into a single Arrow batch, because one `StringArray`'s i32 offsets cap a single
    /// array at ~2 GiB and arrow PANICS (not `Err`s) past it — which killed the maintenance thread.
    #[test]
    fn compaction_encode_chunks_instead_of_building_one_giant_batch() {
        let schema = BarCodec::schema();
        let rows = bars(COMPACT_BATCH_ROWS + 7);
        let out = encode_chunked::<BarCodec>(&rows, &schema).unwrap();
        assert_eq!(out.len(), 2, "must split past the row cap, not emit one batch");
        assert_eq!(out[0].num_rows(), COMPACT_BATCH_ROWS);
        assert_eq!(out[1].num_rows(), 7);
        assert_eq!(
            out.iter().map(|b| b.num_rows()).sum::<usize>(),
            rows.len(),
            "chunking is lossless"
        );
    }

    /// Under the cap the output is exactly what the old single-batch code produced — so the
    /// overwhelming majority of compactions are unchanged.
    #[test]
    fn compaction_encode_is_one_batch_under_the_cap() {
        let schema = BarCodec::schema();
        let out = encode_chunked::<BarCodec>(&bars(1000), &schema).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].num_rows(), 1000);
    }

    /// An empty compaction still yields one (empty) batch, as before — a valid part, not zero parts.
    #[test]
    fn compaction_encode_of_nothing_is_one_empty_batch() {
        let schema = BarCodec::schema();
        let out = encode_chunked::<BarCodec>(&bars(0), &schema).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].num_rows(), 0);
    }
}

#[cfg(test)]
mod unwritable_root_tests {
    use super::*;

    /// **The sandbox diagnosis.** `EROFS` from a systemd unit reads as a disk problem; the unit's
    /// own `ProtectSystem=strict` is the actual cause, and `ReadWritePaths=` is the actual fix.
    /// The message must name BOTH — and the path, so an operator can paste it straight into the
    /// unit.
    #[test]
    fn a_read_only_root_names_readwritepaths_and_the_path() {
        let e = std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem);
        let msg = unwritable_store_root(Path::new("/opt/vike/market_data/hist"), &e).to_string();
        assert!(msg.contains("/opt/vike/market_data/hist"), "the path must be quotable: {msg}");
        assert!(msg.contains("ReadWritePaths="), "the fix must be named: {msg}");
        assert!(msg.contains("ProtectSystem=strict"), "the cause must be named: {msg}");
        assert!(msg.contains("VIKE_HIST_STORE"), "the other way out must be named: {msg}");
    }

    /// The sibling shape — a root owned by another user, or under a `-m700` parent — gets the same
    /// treatment: it is the same question ("who is allowed to write here") wearing a different
    /// errno.
    #[test]
    fn a_permission_denied_root_gets_the_same_diagnosis() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let msg = unwritable_store_root(Path::new("/srv/tape"), &e).to_string();
        assert!(msg.contains("ReadWritePaths="), "{msg}");
        assert!(msg.contains("/srv/tape"), "{msg}");
    }

    /// …and every OTHER failure is passed through verbatim. A gate that decorated unrelated errors
    /// would send operators to the unit file for a full disk or a bad symlink.
    #[test]
    fn an_unrelated_io_error_is_not_decorated() {
        let e = std::io::Error::new(std::io::ErrorKind::NotADirectory, "a file is in the way");
        let msg = unwritable_store_root(Path::new("/x"), &e).to_string();
        assert!(!msg.contains("ReadWritePaths="), "unrelated errors must not be decorated: {msg}");
        assert_eq!(msg, io(&e).to_string(), "…and must read exactly as they did before");
    }

    /// End to end through the real `open`, on a genuinely unwritable parent — proving the
    /// decoration is actually WIRED, not merely defined. Skipped when the process can write to a
    /// `0o555` directory anyway (running as root, or a filesystem without unix modes).
    #[cfg(unix)]
    #[test]
    fn open_under_an_unwritable_parent_reports_the_sandbox_diagnosis() {
        use std::os::unix::fs::PermissionsExt;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let parent = std::env::temp_dir().join(format!("vike-ro-store-{nanos}"));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

        let root = parent.join("market_data").join("hist");
        let outcome = DataFusionHist::open(&root).err().map(|e| e.to_string());

        // Restore before asserting, so a failure does not leave an unremovable directory behind.
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&parent);

        match outcome {
            Some(msg) => assert!(
                msg.contains("ReadWritePaths="),
                "an unwritable root must carry the diagnosis: {msg}"
            ),
            // root, or a filesystem ignoring the mode: the fixture proved nothing, so say so.
            None => eprintln!("skipped: this process can write under a 0o555 directory"),
        }
    }
}

#[cfg(test)]
mod plan_merge_groups_tests {
    use super::*;

    /// Manifest entries for parts of the given `(rows, bytes)`, with the files written to `dir` so
    /// the `target_bytes` check has something real to stat.
    fn parts(dir: &Path, spec: &[(usize, u64)]) -> Vec<manifest::FileEntry> {
        std::fs::create_dir_all(dir).unwrap();
        spec.iter()
            .enumerate()
            .map(|(i, &(rows, bytes))| {
                let name = format!("part-{i:05}.parquet");
                std::fs::write(dir.join(&name), vec![0u8; bytes as usize]).unwrap();
                manifest::FileEntry {
                    name,
                    date: "1970-01-01".into(),
                    ts_min: i as i64,
                    ts_max: i as i64,
                    rows,
                    commit_keys: vec![],
                }
            })
            .collect()
    }

    /// A config that only the knob under test constrains: huge `target_bytes` so the size check
    /// never fires, `min_parts` at the normal 2.
    fn rows_only(max_merge_rows: usize) -> CompactionConfig {
        CompactionConfig { target_bytes: u64::MAX, min_parts: 2, max_merge_rows }
    }

    fn all(files: &[manifest::FileEntry]) -> Vec<usize> {
        (0..files.len()).collect()
    }

    #[test]
    fn a_date_within_budget_stays_one_group() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(10, 10), (10, 10), (10, 10), (10, 10)]);
        assert_eq!(
            plan_merge_groups(d.path(), &files, &all(&files), &rows_only(1000)),
            vec![vec![0, 1, 2, 3]]
        );
    }

    /// The property the OOM was the absence of: no group may exceed the budget just because the
    /// date does. Bytes are held constant here — ROWS are what bounds the merge.
    #[test]
    fn an_oversized_date_splits_into_groups_within_the_row_budget() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(40, 1); 6]);
        let groups = plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100));
        assert_eq!(groups, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
        for g in &groups {
            assert!(g.len() * 40 <= 100, "group over budget: {g:?}");
        }
    }

    /// Byte-identical parts that differ only in ROW COUNT must group differently — the regression
    /// guard for the first fix, which bounded by compressed bytes and still peaked at 8.95 GB on
    /// the CI box because compressed size says nothing about decoded size.
    #[test]
    fn grouping_follows_rows_not_file_size() {
        let d = tempfile::tempdir().unwrap();
        let fat = parts(d.path(), &[(60, 10), (60, 10), (60, 10)]);
        let lean = parts(d.path(), &[(10, 10), (10, 10), (10, 10)]);
        let cfg = rows_only(100);
        assert_eq!(plan_merge_groups(d.path(), &fat, &all(&fat), &cfg), vec![vec![0, 1]]);
        assert_eq!(plan_merge_groups(d.path(), &lean, &all(&lean), &cfg), vec![vec![0, 1, 2]]);
    }

    /// A leftover single part is not a merge — merging one part into one part only renames it.
    #[test]
    fn a_trailing_single_part_group_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(60, 1), (60, 1), (60, 1)]);
        assert_eq!(
            plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)),
            vec![vec![0, 1]]
        );
    }

    /// A part whose own row count reaches the budget can never be merged within it. Skipping it is
    /// what keeps the at-least-2 rule from pairing two such parts and blowing the bound wide open.
    #[test]
    fn a_part_at_the_row_budget_is_skipped_rather_than_paired() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(500, 1), (500, 1), (10, 1), (10, 1)]);
        assert_eq!(
            plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)),
            vec![vec![2, 3]],
            "the two oversized parts were paired — that is 1000 rows in one decode"
        );
    }

    /// `target_bytes` keeps its own meaning: a part already at the sealed-file size is finished,
    /// however few rows it holds.
    #[test]
    fn a_part_at_target_bytes_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(10, 500), (10, 10), (10, 10)]);
        let cfg = CompactionConfig { target_bytes: 100, min_parts: 2, max_merge_rows: 1000 };
        assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &cfg), vec![vec![1, 2]]);
    }

    /// A part the planner cannot stat is treated as not-yet-at-target — the merge reports the real
    /// error, and a missing file never silently drops its siblings out of the pass.
    #[test]
    fn an_unstattable_part_still_joins_its_group() {
        let d = tempfile::tempdir().unwrap();
        let mut files = parts(d.path(), &[(10, 10), (10, 10)]);
        files.push(manifest::FileEntry {
            name: "part-99999.parquet".into(),
            date: "1970-01-01".into(),
            ts_min: 9,
            ts_max: 9,
            rows: 10,
            commit_keys: vec![],
        });
        let cfg = CompactionConfig { target_bytes: 100, min_parts: 2, max_merge_rows: 1000 };
        assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &cfg), vec![vec![0, 1, 2]]);
    }

    /// `min_parts = 1` is the caller saying a LONE fragment is worth a pass — the one-part "merge"
    /// re-encodes it under the current schema, which is how an older-schema part is upgraded (and
    /// what `run_maintenance_handles_chain_series` leans on). The trailing-single rule must not
    /// quietly take that mode away.
    #[test]
    fn min_parts_of_one_keeps_a_lone_part_as_its_own_group() {
        let d = tempfile::tempdir().unwrap();
        let files = parts(d.path(), &[(10, 10)]);
        let one = CompactionConfig { min_parts: 1, ..rows_only(100) };
        assert_eq!(plan_merge_groups(d.path(), &files, &all(&files), &one), vec![vec![0]]);
        assert!(plan_merge_groups(d.path(), &files, &all(&files), &rows_only(100)).is_empty());
    }
}

#[cfg(test)]
mod parse_series_id_tests {
    use super::*;

    /// The three leaf shapes `parse_series_id` must tell apart, each driven through the REAL encode
    /// side (`ticks_dir` / `bars_dir` / `group_dir`) rather than a hand-built string, so the test
    /// exercises the actual encode/parse pair the store round-trips through.
    ///
    /// The empty one is the defect: `crates/vike-data/src/series.rs`'s `SeriesId::group` reserves an
    /// empty `symbol` as the GROUPED-series sentinel, while this parser accepted `symbol=` and
    /// handed back `SeriesId { symbol: "", group: None }` — a PER-SYMBOL id carrying the sentinel
    /// value, addressable by no scan and recognisable as grouped by nothing. The sentinel and the
    /// parser disagreed about the same value.
    #[test]
    fn an_empty_symbol_segment_is_refused_while_both_real_layouts_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let df = DataFusionHist::open(d.path()).unwrap();

        // Control 1: an ordinary tick series still round-trips.
        assert_eq!(
            parse_series_id(d.path(), &df.ticks_dir("quote", "binance", "BTCUSDT")),
            Some(SeriesId::per_symbol("quote", "binance", "BTCUSDT", None)),
            "a per-symbol tick leaf must still parse"
        );

        // Control 2: so does a bar series, whose leaf carries the extra `interval=` segment.
        assert_eq!(
            parse_series_id(d.path(), &df.bars_dir("binance", "BTCUSDT", "1m")),
            Some(SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string()))),
            "a per-symbol bar leaf must still parse, interval included"
        );

        // Control 3: and a GROUPED leaf, which has no `symbol=` segment at all.
        assert_eq!(
            parse_series_id(d.path(), &df.group_dir("quote", "polymarket", "btc-5m")),
            Some(SeriesId::grouped("quote", "polymarket", "btc-5m")),
            "a grouped leaf must still parse as grouped"
        );

        // The finding: the same encoder, handed the sentinel value, produces a `symbol=` leaf.
        let sentinel = df.ticks_dir("exec_fill", "binance", "");
        assert!(
            sentinel.ends_with("symbol="),
            "the fixture really is the empty-`symbol=` shape: {}",
            sentinel.display()
        );
        assert_eq!(
            parse_series_id(d.path(), &sentinel),
            None,
            "`symbol=` is the grouped sentinel, not a symbol — it must not parse as a per-symbol id"
        );
    }

    /// An empty `group=` is deliberately NOT refused alongside it. It is degenerate-looking but
    /// UNAMBIGUOUS: `group.is_some()` — the field every consumer tells the two layouts apart by —
    /// still answers correctly, so it collides with no sentinel. Pinned so the narrower rule is a
    /// decision on the record rather than an oversight someone "tidies up" later.
    #[test]
    fn an_empty_group_segment_is_not_refused() {
        let d = tempfile::tempdir().unwrap();
        let df = DataFusionHist::open(d.path()).unwrap();
        let id = parse_series_id(d.path(), &df.group_dir("quote", "binance", ""))
            .expect("an empty group= still parses");
        assert!(id.group.is_some(), "it is recognisable as GROUPED, which is the whole test");
    }

    /// The WRITE side of the same law, and the half with live-money consequences.
    ///
    /// `vike-app`'s reconcile pre-seed scans every `kind=exec_fill` leaf by `id.symbol` and folds
    /// the trade_ids into the SEEN-FILL dedup set. A fill missing from that set reads as a
    /// `MissingFill` — one of the two kinds `hybrid` auto-applies — so an unattributable fill row is
    /// a double-book waiting for the next pass, not merely an unreadable one.
    ///
    /// This must hold together with the parser above: refusing the leaf on READ while still
    /// accepting it on WRITE would strand exactly those trade_ids.
    #[test]
    fn an_exec_fill_with_no_symbol_is_refused_and_a_normal_one_is_not() {
        let d = tempfile::tempdir().unwrap();
        let df = DataFusionHist::open(d.path()).unwrap();

        let err = df
            .append_exec_fills("binance", "", &[], None)
            .expect_err("an unattributable fill must never become durable");
        let msg = err.to_string();
        assert!(msg.contains("empty symbol"), "the refusal names the offence: {msg}");
        assert!(msg.contains("seen-fill"), "and why it matters, not just that it is unaddressable");

        // The control: a normal symbol still appends. Without it, a guard that refused EVERYTHING
        // would pass the assertion above.
        df.append_exec_fills("binance", "BTCUSDT", &[], None)
            .expect("an attributable fill still appends");
    }
}
