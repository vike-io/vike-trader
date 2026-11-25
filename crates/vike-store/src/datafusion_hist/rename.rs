//! Renaming a stored instrument: every per-symbol series of one venue filed under an OLD name moves
//! under a NEW one with no history lost — decision 0290's store migration. The operator spelling is
//! `vike-cli data history rename`, which spawns `vike-datahub store rename` on the store's own box
//! (`crates/vike-cli/src/cmd/data/hist/rename.rs`'s `execute_rename` carries why it is a verb and
//! not a deploy step).
//!
//! # What survives
//!
//! Every row, every part's coverage (its `date=`, row count and ts bounds go into the target's
//! manifest as they were) and every COMMIT KEY, on the part that holds its rows, with the old name
//! rewritten to the new one wherever it is a whole field of the key ([`rename_in_commit_key`]): a
//! producer that keys the same window by the NEW name (`dukascopy-archive:AAPL.CFD:<day>`, a kline
//! window's `oanda:EURUSD:5s:<window>`) finds it and does not store it again, and a key that does
//! not name the instrument is kept as it was. The keys move in the manifest AND in the part's own
//! footer, so a rebuild of the target's manifest finds the same ones. The source's key-only commits
//! (empty-day markers) join the target, rewritten the same way, through
//! [`HistoryStoreDataFusion::spend_keys_without_rows`]. No key is minted.
//!
//! The row's own symbol is rewritten to the new name for the kinds whose rows carry one
//! ([`symbol_column`]).
//!
//! # A MOVE and a MERGE are one procedure
//!
//! For each part of the source, in manifest order:
//!
//! 1. it is written into the target's `date=` directory under an UNPUBLISHED name
//!    (`merge_tmp_name`, which neither the read path nor `rebuild_manifest` ever indexes) —
//!    re-encoded with the new keys and row symbol, or hard-linked when neither changes;
//! 2. under the TARGET's series lock it is published as [`moved_part_name`] — unless the target
//!    already lists that name (an interrupted run published it) or already holds, on the same
//!    `date=`, every (rewritten) commit key it carries (the same batches, stored under both names:
//!    the merge's dedup, by key, and by row only in the one split below; [`target_holds`] says why
//!    the date counts);
//! 3. under the SOURCE's lock, which a rename holds from start to end, the part's removal is
//!    published and its file unlinked (manifest first, unlink after).
//!
//! Then the source's key-only commits join the target and the emptied source is removed. A target
//! with no manifest is a MOVE and one with a manifest is a MERGE; the only difference is whether step
//! 2 can find anything there.
//!
//! # A name with a `/`
//!
//! A name the store cannot file today was filed, before that rule, in a NESTED leaf: hyperliquid's
//! spot `PURR/USDC` sits inside `symbol=PURR`, where no listing reaches it. Such an OLD name is
//! accepted spelled exactly as it sits — two names and ONE `/` — and its leaf is probed directly (one
//! per kind in scope, at the interval and lane the request names) instead of listed; every other `/`
//! is refused. Removing the emptied source never touches a directory other than its own `date=`
//! partitions, so a series filed at `symbol=PURR` and a nested leaf inside it survive each other's
//! rename.
//!
//! # Crash-safe and idempotent
//!
//! A re-run finishes what a crash left, at any point: an unpublished file is overwritten (step 1); a
//! published name is found and not copied again (step 2); a part already dropped from the source is
//! not listed there any more (step 3); an emptied source is first folded to an EMPTY base and only
//! then removed, so a crash during the removal leaves either an empty series (re-run removes it) or
//! a leaf no listing shows. A re-run after success lists no source and does nothing.
//! ⚠ One residual, narrower than one part: a crash between steps 2 and 3, then a COMPACTION of the
//! target before the re-run, then a part with NO commit key — the re-run can tell such a part is
//! there neither by its name (the compaction consumed it) nor by its keys (it has none), and copies
//! it again. Every part a backfill, an import or the recorder writes carries a key.
//!
//! A part sharing SOME of its keys with the target is refused rather than guessed at: copying it
//! would store the shared batches twice and dropping it would lose the rest. ONE exception, the
//! execution kinds (`exec_fill`, `exec_order`), whose every row is a whole fact a second copy of
//! which is the same row: the retired journal materializer keyed a pass by its window AND the series' name
//! (`mat-fill-oanda-EUR_USD-398408-398435`), so a window an older build materialized under both
//! spellings of one instrument holds two batches the rename gives ONE key, with rows that overlap but
//! need not be equal (the CI box's oanda `EUR_USD` fills of 2026-10-26, measured 2026-11-17: a compacted
//! part of four keys, two of them the target's, five rows of which two are the target's own rows).
//! Such a part is SPLIT ([`HeldRows`]): a row identical to a row of the target's parts carrying the
//! shared keys on that date (every field but the symbol) stays the target's and is not copied; every
//! other row moves, under the part's other keys. The target's copy of each shared batch is kept as
//! it is.
//!
//! # ⚠ It holds the OLD series' lock for the whole move
//!
//! So no compaction or retention of the old name can publish over a part being moved. That is
//! harmless after the deploy, when nothing writes the old name; a recorder still writing it would
//! spin out its flush budget against this lock and DISCARD its buffer. Run it after the deploy. The
//! target's lock is taken per part, for one manifest read and one publish, like any commit.
//!
//! # What a move costs
//!
//! A part is read, decoded and re-encoded once (zstd level 3, the sealed profile),
//! [`REWRITE_BATCH_ROWS`] rows at a time plus the writer's one open row group — never the series,
//! never a whole part — because its footer carries its keys and every keyed part's keys name the
//! instrument. Only a part whose keys and rows would not change is hard-linked. MEASURED on the CI box,
//! 2026-11-16 (`du -sh`): dukascopy's `quote AAPLUSUSD`, the largest move of decision 0290, is
//! 748 MB in 1,683 parts holding 123,301,277 rows; oanda's `bar EUR_USD 5s` is 389 MB holding
//! 35,470,965; the 40 share-CFD quote series together are 15 GB (401,176,192 rows). A move reads its
//! series once and writes about as much. Each part leaves the source as soon as the target has it,
//! so the extra disk the move needs is one part, not the series. MEASURED on COPIES of those two
//! series in a the CI box lane, 2026-11-16, release build at nice 19 (`examples/series_digest.rs` before
//! and after): the 5 s bars moved in 39 s (3,666 parts) and the quotes in 50 s (1,683 parts), with
//! the row count, the first and last timestamps, an ordered checksum over every row and every
//! commit key (rewritten) unchanged, and every quote reading its new symbol.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{Array, ArrayRef, BooleanArray, Int64Array, StringArray};
use datafusion::arrow::compute::{cast, filter_record_batch};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use datafusion::parquet::arrow::{ARROW_SCHEMA_META_KEY, ArrowWriter};
use datafusion::parquet::file::metadata::KeyValue;
use serde::Serialize;

use crate::hist_maint::{Durability, WriteProfile};
use vike_data::exec_log::{ExecFillRow, ExecOrderRow};
use vike_data::store::hist::HistoryStoreError;
use vike_data::store::series::SeriesId;
use vike_model::StoreKindId;
use vike_paths::store_path::store_name_of_user_name;

use super::codec::{ExecFillCodec, ExecOrderCodec, SeriesCodec};
use super::delta::DeltaFrame;
use super::manifest::{
    FileEntry, MANIFEST, MANIFEST_LOCK, Manifest, SeriesLock, fold_base, merge_tmp_name, publish,
    read_manifest,
};
use super::parquet_io::writer_properties;
use super::wal::{WAL, WAL_SET_ASIDE};
use super::{COMMIT_KEYS_META, HistoryStoreDataFusion, fsync_dir, io, part_dir, q};

/// Rows decoded and re-encoded at a time when a part is rewritten.
const REWRITE_BATCH_ROWS: usize = 65_536;

/// One series a rename selects: where it is filed now, where it goes, and how much it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RenameStep {
    /// The series as it is filed now, under the OLD name.
    pub source: SeriesId,
    /// The same series under the NEW name: every dimension but the symbol is the source's.
    pub target: SeriesId,
    /// Rows the source's manifest lists.
    pub rows: u64,
    /// Parts the source's manifest lists.
    pub parts: usize,
    /// `true` when the target already has a manifest, so the parts JOIN it (a MERGE, deduplicated
    /// by commit key) instead of founding it (a MOVE). One procedure either way.
    pub merge: bool,
}

impl RenameStep {
    /// The plan line an operator reads before anything is written.
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {} -> {} (interval {}, source {}): {} rows in {} parts",
            if self.merge { "MERGE" } else { "MOVE" },
            self.source.kind,
            self.source.venue,
            self.source.symbol,
            self.target.symbol,
            self.source.interval.as_deref().unwrap_or("-"),
            self.source.source.as_deref().unwrap_or("-"),
            self.rows,
            self.parts
        )
    }
}

/// What one series' rename did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RenameOutcome {
    /// Parts written into the target and dropped from the source.
    pub parts_moved: usize,
    /// The rows those parts hold (a split part's: the rows it moved, the module doc).
    pub rows_moved: u64,
    /// Parts the target already held — published there by an interrupted run, or carrying only
    /// commit keys the target already has — dropped from the source without a second copy.
    pub parts_already_in_target: usize,
}

impl HistoryStoreDataFusion {
    /// Every PER-SYMBOL series of `venue` filed under `from` (narrowed to `kind`, `interval` and the
    /// `source=` lane when given; every kind, interval and lane, the sourceless series included,
    /// when not), each with the series it becomes under `to`. Writes nothing. A `from` with a `/`
    /// is a nested leaf, probed rather than listed (the module doc's "A name with a `/`").
    ///
    /// # Errors
    ///
    /// A blank name, `from == to`, a name the store cannot file
    /// ([`vike_paths::store_path::refuse_a_path_hostile_symbol`]; for `from`, any `/` but the nested
    /// leaf's one), and `from` naming a GROUPED series of `venue`: its rows are many instruments told
    /// apart by their own symbol, so renaming the group renames none of them.
    pub fn plan_rename(
        &self,
        venue: &str,
        from: &str,
        to: &str,
        kind: Option<StoreKindId>,
        interval: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<RenameStep>, HistoryStoreError> {
        // Both names as the store files them (decision 0290, «Б» simplified): a user name's builder-dex `:` is `-`.
        let (from, to) = (store_name_of_user_name(from), store_name_of_user_name(to));
        let (from, to) = (from.as_str(), to.as_str());
        refuse_a_rename(from, to)?;
        let in_scope = |id: &SeriesId| id.venue == venue && kind.is_none_or(|k| k == id.kind);
        let selected: Vec<SeriesId> = if from.contains('/') {
            StoreKindId::ALL
                .into_iter()
                .filter(|k| kind.is_none_or(|wanted| wanted == *k))
                .map(|k| SeriesId {
                    kind: k,
                    venue: venue.to_string(),
                    symbol: from.to_string(),
                    interval: interval.map(str::to_string),
                    group: None,
                    source: source.map(str::to_string),
                })
                .filter(|id| self.series_dir_of(id).join(MANIFEST).is_file())
                .collect()
        } else {
            let listed = self.list_series()?;
            if let Some(group) =
                listed.iter().find(|id| in_scope(id) && id.group.as_deref() == Some(from))
            {
                return Err(HistoryStoreError::Query(format!(
                    "`{from}` is a GROUPED {} series of {venue} (`group={from}`), and a rename \
                     moves per-symbol series only: a grouped part holds many instruments told \
                     apart by each row's own symbol, so renaming the group would rename none of them",
                    group.kind
                )));
            }
            listed
                .into_iter()
                .filter(|id| {
                    in_scope(id)
                        && id.group.is_none()
                        && id.symbol == from
                        && interval.is_none_or(|i| id.interval.as_deref() == Some(i))
                        && source.is_none_or(|s| id.source.as_deref() == Some(s))
                })
                .collect()
        };
        let mut steps = Vec::with_capacity(selected.len());
        for id in selected {
            let coverage = self.series_coverage(&id)?;
            let target = SeriesId { symbol: to.to_string(), ..id.clone() };
            let merge = self.series_dir_of(&target).join(MANIFEST).is_file();
            steps.push(RenameStep {
                source: id,
                target,
                rows: coverage.rows,
                parts: coverage.parts,
                merge,
            });
        }
        Ok(steps)
    }

    /// Move one per-symbol series under `to` — a MOVE into an absent target or a MERGE into an
    /// existing one — and remove it from its old name. The module doc is the contract; an absent
    /// source is a rename already done and answers an empty [`RenameOutcome`].
    ///
    /// # Errors
    ///
    /// The [`Self::plan_rename`] refusals, a grouped `source`, a source whose write-ahead log holds a
    /// record its recovery could not apply (the rename would delete it unread), a part sharing only
    /// some of its commit keys with the target, and any I/O failure — after which a re-run finishes
    /// the job.
    pub fn rename_series(
        &self,
        source: &SeriesId,
        to: &str,
    ) -> Result<RenameOutcome, HistoryStoreError> {
        if let Some(group) = &source.group {
            return Err(HistoryStoreError::Query(format!(
                "a rename moves per-symbol series only, and this one is the GROUP `{group}`"
            )));
        }
        // The new name as the store files it (decision 0290, «Б» simplified): a user name's builder-dex `:` is `-`.
        let to = store_name_of_user_name(to);
        let to = to.as_str();
        refuse_a_rename(&source.symbol, to)?;
        let target = SeriesId { symbol: to.to_string(), ..source.clone() };
        let moving = Move {
            store: self,
            source,
            from_dir: self.series_dir_of(source),
            to_dir: self.series_dir_of(&target),
            column: symbol_column(source.kind),
            to,
        };
        // Probed BEFORE the lock, which creates the leaf: no source manifest is a rename that is done.
        if !moving.from_dir.join(MANIFEST).is_file() {
            return Ok(RenameOutcome::default());
        }
        let guard = SeriesLock::acquire(&moving.from_dir)?;
        if moving.from_dir.join(WAL).exists() {
            return Err(HistoryStoreError::Query(format!(
                "{} holds a write-ahead record its recovery could not apply (the open logs it at \
                 error level, with its remedy): renaming the series would delete it unread. Resolve \
                 it first, then re-run; nothing was moved",
                moving.from_dir.display()
            )));
        }
        let mut from_manifest = read_manifest(&moving.from_dir)?;
        let mut target_seen = read_manifest(&moving.to_dir)?;
        let mut outcome = RenameOutcome::default();
        for part in from_manifest.files.clone() {
            if let Some(rows) = moving.part(&part, &mut target_seen)? {
                outcome.parts_moved += 1;
                outcome.rows_moved += rows;
            } else {
                outcome.parts_already_in_target += 1;
            }
            from_manifest.files.retain(|f| !(f.name == part.name && f.date == part.date));
            from_manifest.version += 1;
            let frame = DeltaFrame {
                version: from_manifest.version,
                files_rm: vec![(part.name.clone(), part.date.clone())],
                ..Default::default()
            };
            publish(
                &moving.from_dir,
                &mut from_manifest,
                frame,
                Durability::Fsync,
                self.fold_bytes(),
            )?;
            remove_if_present(&part_dir(&moving.from_dir, &part.date).join(&part.name))?;
        }
        let key_only: Vec<String> = from_manifest
            .orphan_commits
            .iter()
            .map(|k| rename_in_commit_key(k, &source.symbol, to))
            .collect();
        let key_only: Vec<&str> = key_only.iter().map(String::as_str).collect();
        self.spend_keys_without_rows(&target, &key_only)?;
        // An EMPTY base is folded in before anything is removed: a crash mid-removal then leaves an
        // empty series or an unlisted leaf, never a base naming parts that are gone.
        fold_base(&moving.from_dir, &from_manifest)?;
        remove_emptied_source(&moving.from_dir, guard)?;
        if (source.interval.is_some() || source.symbol.contains('/'))
            && let Some(parent) = moving.from_dir.parent()
        {
            // The leaf's own parent — `symbol=EUR_USD` above `interval=5s`, `symbol=PURR` above a
            // nested leaf — goes too when nothing else is in it.
            let _ = std::fs::remove_dir(parent);
        }
        Ok(outcome)
    }
}

/// One series' move: the two leaves, and how its parts are carried across.
struct Move<'a> {
    store: &'a HistoryStoreDataFusion,
    source: &'a SeriesId,
    from_dir: PathBuf,
    to_dir: PathBuf,
    column: Option<&'static str>,
    to: &'a str,
}

impl Move<'_> {
    /// Carry one source part into the target (the module doc's steps 1 and 2): the rows written there,
    /// or `None` when the target already held them. `seen` is the target's manifest as last read: a
    /// cheap look that saves a copy, re-checked under the target's lock before anything is published.
    fn part(
        &self,
        part: &FileEntry,
        seen: &mut Manifest,
    ) -> Result<Option<u64>, HistoryStoreError> {
        // The part as the target will list it: its moved name, and its keys spelled with the new name.
        let entry = FileEntry {
            name: moved_part_name(self.source, part),
            commit_keys: part
                .commit_keys
                .iter()
                .map(|k| rename_in_commit_key(k, &self.source.symbol, self.to))
                .collect(),
            ..part.clone()
        };
        let holds = target_holds(seen, &entry);
        let shared = match &holds {
            TargetHolds::All => return Ok(None),
            TargetHolds::Nothing => Vec::new(),
            TargetHolds::Some(shared) if HeldRows::splits(self.source.kind) => shared.clone(),
            TargetHolds::Some(shared) => return Err(partly_held(&entry, shared.len())),
        };
        let date_dir = part_dir(&self.to_dir, &part.date);
        std::fs::create_dir_all(&date_dir).map_err(io)?;
        let unpublished = date_dir.join(merge_tmp_name(&entry.name));
        remove_if_present(&unpublished)?;
        let from = part_dir(&self.from_dir, &part.date).join(&part.name);
        let published_entry = if shared.is_empty() {
            if self.column.is_none() && entry.commit_keys == part.commit_keys {
                link_or_copy(&from, &unpublished)?;
            } else {
                rewrite_part(&from, &unpublished, self.column, self.to, &entry.commit_keys, None)?;
            }
            entry.clone()
        } else {
            let held = HeldRows::read(self.source.kind, &self.to_dir, seen, &entry.date, &shared)?;
            let keys: Vec<String> =
                entry.commit_keys.iter().filter(|k| !shared.contains(k)).cloned().collect();
            let kept = rewrite_part(&from, &unpublished, self.column, self.to, &keys, Some(&held))?;
            tracing::info!(
                part = %part.name, date = %part.date, rows = part.rows, kept = kept.rows,
                shared = ?shared,
                "rename: a part shares some of its batches with the target: the rows the target holds \
                 stay its own, the rest move under the part's other keys"
            );
            if kept.rows == 0 {
                // Every row is the target's already: nothing to copy, and its other keys are spent there.
                remove_if_present(&unpublished)?;
                let target = SeriesId { symbol: self.to.to_string(), ..self.source.clone() };
                let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                self.store.spend_keys_without_rows(&target, &keys)?;
                return Ok(None);
            }
            FileEntry {
                commit_keys: keys,
                rows: kept.rows,
                ts_min: kept.ts_min,
                ts_max: kept.ts_max,
                ..entry.clone()
            }
        };
        let _guard = SeriesLock::acquire(&self.to_dir)?;
        let mut m = read_manifest(&self.to_dir)?;
        match target_holds(&m, &entry) {
            TargetHolds::All => {
                remove_if_present(&unpublished)?;
                *seen = m;
                return Ok(None);
            }
            now if now == holds => {}
            TargetHolds::Some(now) => return Err(partly_held(&entry, now.len())),
            TargetHolds::Nothing => {
                return Err(HistoryStoreError::Query(format!(
                    "the target's batches of date {} changed while a part was carried; nothing of it was \
                     published, and a re-run finishes the rename",
                    entry.date
                )));
            }
        }
        // An unlisted file under this name is this rename's own, left by a crash before its publish
        // — and `rename` onto an existing file fails on Windows.
        let published = date_dir.join(&published_entry.name);
        remove_if_present(&published)?;
        std::fs::rename(&unpublished, &published).map_err(io)?;
        fsync_dir(&date_dir)?;
        m.files.push(published_entry.clone());
        m.version += 1;
        let rows = published_entry.rows as u64;
        let frame = DeltaFrame {
            version: m.version,
            files_add: vec![published_entry],
            ..Default::default()
        };
        publish(&self.to_dir, &mut m, frame, Durability::Fsync, self.store.fold_bytes())?;
        *seen = m;
        Ok(Some(rows))
    }
}

/// **The rows the target holds of a source part's shared batches**, for the execution kinds a split
/// is defined on (the module doc): the rows of every target part of the date that carries one of the
/// shared keys, decoded, each with its symbol cleared, so a source row is held when it equals one of
/// them in every other field.
enum HeldRows {
    Fills(Vec<ExecFillRow>),
    Orders(Vec<ExecOrderRow>),
}

impl HeldRows {
    /// Whether a part of `kind` that shares some of its keys with the target may be split by row.
    fn splits(kind: StoreKindId) -> bool {
        matches!(kind, StoreKindId::ExecFill | StoreKindId::ExecOrder)
    }

    /// The held rows of the target `to_dir` (manifest `m`) on `date`, in the parts carrying `shared`.
    fn read(
        kind: StoreKindId,
        to_dir: &Path,
        m: &Manifest,
        date: &str,
        shared: &[String],
    ) -> Result<Self, HistoryStoreError> {
        let mut held = match kind {
            StoreKindId::ExecFill => Self::Fills(Vec::new()),
            StoreKindId::ExecOrder => Self::Orders(Vec::new()),
            other => {
                return Err(HistoryStoreError::Query(format!(
                    "a {other} part is never split by row"
                )));
            }
        };
        let parts = m
            .files
            .iter()
            .filter(|f| f.date == date && f.commit_keys.iter().any(|k| shared.contains(k)));
        for f in parts {
            let file = std::fs::File::open(part_dir(to_dir, date).join(&f.name)).map_err(io)?;
            for batch in
                ParquetRecordBatchReaderBuilder::try_new(file).map_err(q)?.build().map_err(q)?
            {
                let batch = batch.map_err(q)?;
                match &mut held {
                    Self::Fills(rows) => rows.extend(
                        ExecFillCodec::decode(&batch, "")?
                            .into_iter()
                            .map(|r| ExecFillRow { symbol: String::new(), ..r }),
                    ),
                    Self::Orders(rows) => rows.extend(
                        ExecOrderCodec::decode(&batch, "")?
                            .into_iter()
                            .map(|r| ExecOrderRow { symbol: String::new(), ..r }),
                    ),
                }
            }
        }
        Ok(held)
    }

    /// `true` for each row of `batch` (a source part's) that the target does NOT hold: the rows to move.
    fn keep(&self, batch: &RecordBatch) -> Result<BooleanArray, HistoryStoreError> {
        Ok(match self {
            Self::Fills(held) => ExecFillCodec::decode(batch, "")?
                .into_iter()
                .map(|r| Some(!held.contains(&ExecFillRow { symbol: String::new(), ..r })))
                .collect(),
            Self::Orders(held) => ExecOrderCodec::decode(batch, "")?
                .into_iter()
                .map(|r| Some(!held.contains(&ExecOrderRow { symbol: String::new(), ..r })))
                .collect(),
        })
    }
}

/// `key` with every whole-field `from` replaced by `to`: a field is bounded by the key's start or end
/// or by `:` or `-`, so `oanda:EUR_USD:5s:1-2` becomes `oanda:EURUSD:5s:1-2` and
/// `mat-fill-oanda-EUR_USD-1-2` becomes `mat-fill-oanda-EURUSD-1-2`, while `EUR_USDX` is not
/// `EUR_USD`. A key with no such field is returned as it was. Every template
/// `vike_data::store::store_kind::STORE_KINDS` declares — held verbatim to its producer by
/// `crates/vike-ops/tests/venues/store_kind_gate/shape.rs`'s
/// `every_declared_commit_key_template_exists_in_its_producer` — bounds its instrument field this
/// way, which `every_declared_key_template_renames_to_the_key_minted_for_the_new_name` holds.
fn rename_in_commit_key(key: &str, from: &str, to: &str) -> String {
    if from.is_empty() {
        return key.to_string();
    }
    let separator = |i: usize| matches!(key.as_bytes()[i], b':' | b'-');
    let mut out = String::with_capacity(key.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = key[search..].find(from) {
        let at = search + found;
        let end = at + from.len();
        if (at == 0 || separator(at - 1)) && (end == key.len() || separator(end)) {
            out.push_str(&key[copied..at]);
            out.push_str(to);
            copied = end;
            search = end;
        } else {
            search = at + key[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&key[copied..]);
    out
}

/// The column a kind's rows carry their own instrument in, which a rename rewrites — `None` for a
/// kind whose identity is the path alone. Exhaustive, so a new kind does not compile until it is
/// decided; `every_named_symbol_column_is_one_its_codec_writes` holds each name to the codec.
fn symbol_column(kind: StoreKindId) -> Option<&'static str> {
    match kind {
        StoreKindId::Quote | StoreKindId::Trade | StoreKindId::Book | StoreKindId::Depth => {
            Some("symbol_col")
        }
        StoreKindId::ExecFill | StoreKindId::ExecOrder => Some("symbol"),
        StoreKindId::Chain => Some("underlying"),
        StoreKindId::Cohort => Some("asset"),
        // A text series's symbol slot names its COLLECTOR (`rss`, `forexfactory`), never an instrument; a
        // row's `subject` is the text's own field, not its identity, so a rename leaves it as written.
        // A score series is keyed by its text kind and model, an SEC series by a CIK: none names an instrument. An
        // option-trade log's symbol slot is its CURRENCY (`BTC`, `USDC`), and one log holds several underlyings, so its
        // rows' `underlying` is a field of each trade (setting it to the new name would rewrite the others'). A forecast
        // row names its mount, never a symbol (the path does).
        StoreKindId::Bar
        | StoreKindId::Equity
        | StoreKindId::ExecForecast
        | StoreKindId::ExecFunding
        | StoreKindId::FinancialFact
        | StoreKindId::InstitutionalHolding
        | StoreKindId::OptionTrade
        | StoreKindId::PerpMetrics
        | StoreKindId::Properties
        | StoreKindId::Text
        | StoreKindId::TextScore => None,
    }
}

/// The name a moved part is published under in the target: deterministic in the source series and
/// the part's whole manifest entry, so a re-run recognises a part an interrupted run published, and
/// distinct for every source merged into one target. FNV-1a 64 (`vike_utility::fnv`) because it is
/// stable across builds, which a re-run after a redeploy needs.
fn moved_part_name(source: &SeriesId, part: &FileEntry) -> String {
    let identity = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        source.kind,
        source.venue,
        source.symbol,
        source.interval.as_deref().unwrap_or(""),
        source.source.as_deref().unwrap_or(""),
        part.date,
        part.name,
        part.ts_min,
        part.ts_max,
        part.rows,
        part.commit_keys.join("\n")
    );
    format!("part-moved-{}.parquet", vike_utility::fnv::fnv1a64_hex(identity.as_bytes()))
}

/// What the target holds of a source part (as the target would list it: moved name, rewritten keys).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetHolds {
    /// None of its keys on its date: copy it.
    Nothing,
    /// Its name listed, or every commit key it carries on a part of the SAME `date=`: drop it.
    All,
    /// Some of its keys, not all (these): split it, or refuse it (the module doc says which).
    Some(Vec<String>),
}

/// [`TargetHolds`] of `entry` in the target's manifest `m`. The date is part of the identity: a batch
/// that straddles a UTC midnight stamps its one key on a part per date, so the key alone would call the
/// second day already moved once the first was — and drop it. A compaction merges within a date and
/// keeps the union of the keys, so a moved part it absorbed is still found.
fn target_holds(m: &Manifest, entry: &FileEntry) -> TargetHolds {
    let same_date = move || m.files.iter().filter(move |f| f.date == entry.date);
    if same_date().any(|f| f.name == entry.name) {
        return TargetHolds::All;
    }
    let held: Vec<String> = entry
        .commit_keys
        .iter()
        .filter(|k| same_date().any(|f| f.commit_keys.contains(k)))
        .cloned()
        .collect();
    if held.is_empty() {
        TargetHolds::Nothing
    } else if held.len() == entry.commit_keys.len() {
        TargetHolds::All
    } else {
        TargetHolds::Some(held)
    }
}

/// The refusal of a part sharing `held` of its keys with the target, of a kind no row split is defined on.
fn partly_held(entry: &FileEntry, held: usize) -> HistoryStoreError {
    HistoryStoreError::Query(format!(
        "a part of date {} carries {} commit keys and the target already holds {held} of them on \
         that date: copying it would store those batches twice and dropping it would lose the rest. \
         The rename stopped here with nothing lost (the parts before it are in the target, the rest \
         still under the old name); keys: {:?}",
        entry.date,
        entry.commit_keys.len(),
        entry.commit_keys
    ))
}

/// What a rewrite wrote: its rows and their `ts` bounds.
struct Written {
    rows: usize,
    ts_min: i64,
    ts_max: i64,
}

/// Write `from` to `to` with its footer's commit keys set to `keys` and, when `column` names one the
/// part has, every row's `column` set to `symbol` — streaming. The part's schema, its other columns
/// and its other footer entries are carried across unchanged. With `held`, a row the target holds is
/// left out (a split, the module doc). The answer is what was written.
fn rewrite_part(
    from: &Path,
    to: &Path,
    column: Option<&str>,
    symbol: &str,
    keys: &[String],
    held: Option<&HeldRows>,
) -> Result<Written, HistoryStoreError> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(from).map_err(io)?)
        .map_err(q)?;
    let schema = reader.schema().clone();
    // `ARROW:schema` is the writer's own entry and it writes it again; the keys are the new ones.
    let mut footer: Vec<KeyValue> = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .map(|kv| {
            kv.iter()
                .filter(|e| e.key != ARROW_SCHEMA_META_KEY && e.key != COMMIT_KEYS_META)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if !keys.is_empty() {
        footer.push(KeyValue::new(COMMIT_KEYS_META.to_string(), keys.join("\n")));
    }
    let at = column.and_then(|c| schema.index_of(c).ok());
    let ts_at = schema.index_of("ts").ok();
    let mut written = Written { rows: 0, ts_min: i64::MAX, ts_max: i64::MIN };
    let properties = writer_properties(WriteProfile::Sealed, footer)?;
    let file = std::fs::File::create(to).map_err(io)?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(properties)).map_err(q)?;
    for batch in reader.with_batch_size(REWRITE_BATCH_ROWS).build().map_err(q)? {
        let batch = batch.map_err(q)?;
        let batch = match held {
            None => batch,
            Some(held) => filter_record_batch(&batch, &held.keep(&batch)?).map_err(q)?,
        };
        if let Some(ts) = ts_at.and_then(|i| batch.column(i).as_any().downcast_ref::<Int64Array>())
        {
            for t in ts.iter().flatten() {
                written.ts_min = written.ts_min.min(t);
                written.ts_max = written.ts_max.max(t);
            }
        }
        written.rows += batch.num_rows();
        let batch = match at {
            None => batch,
            Some(at) => {
                let renamed: ArrayRef = Arc::new(StringArray::from_iter_values(
                    std::iter::repeat_n(symbol, batch.num_rows()),
                ));
                let mut columns = batch.columns().to_vec();
                columns[at] = cast(&renamed, schema.field(at).data_type()).map_err(q)?;
                RecordBatch::try_new(schema.clone(), columns).map_err(q)?
            }
        };
        writer.write(&batch).map_err(q)?;
    }
    writer.into_inner().map_err(q)?.sync_all().map_err(io)?;
    Ok(written)
}

/// A hard link to `from` at `to`, or a durable copy where the filesystem refuses one. A part is never
/// rewritten in place, so a link shared by the two leaves is safe.
fn link_or_copy(from: &Path, to: &Path) -> Result<(), HistoryStoreError> {
    if std::fs::hard_link(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(io)?;
    std::fs::OpenOptions::new().write(true).open(to).and_then(|f| f.sync_all()).map_err(io)
}

/// Remove an emptied source leaf: its files and its `date=` partitions, then — the lock released —
/// the lock file and the leaf itself when nothing else is in it. A directory of any other name is not
/// this series' (it is a nested leaf, `PURR/USDC` inside `symbol=PURR`) and stays, with the leaf; so
/// does a WAL recovery set aside (`WAL_SET_ASIDE`), which is evidence and never deleted here.
fn remove_emptied_source(dir: &Path, guard: SeriesLock) -> Result<(), HistoryStoreError> {
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let path = entry.path();
        if entry.file_type().map_err(io)?.is_dir() {
            if entry.file_name().to_string_lossy().starts_with("date=") {
                std::fs::remove_dir_all(&path).map_err(io)?;
            }
        } else if entry.file_name() != MANIFEST_LOCK
            && !entry.file_name().to_string_lossy().starts_with(WAL_SET_ASIDE)
        {
            remove_if_present(&path)?;
        }
    }
    drop(guard);
    remove_if_present(&dir.join(MANIFEST_LOCK))?;
    match std::fs::remove_dir(dir) {
        Err(e) if !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {
            Err(io(e))
        }
        _ => Ok(()),
    }
}

fn remove_if_present(path: &Path) -> Result<(), HistoryStoreError> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(io(e)),
        _ => Ok(()),
    }
}

/// The refusals both verbs share: blank names, one name twice, and a name the store cannot file —
/// except an OLD name spelled as a nested leaf, two names around ONE `/` (the module doc's "A name
/// with a `/`").
fn refuse_a_rename(from: &str, to: &str) -> Result<(), HistoryStoreError> {
    if from.trim().is_empty() || to.trim().is_empty() {
        return Err(HistoryStoreError::Query(
            "a rename needs the name stored now AND the new name, neither blank".to_string(),
        ));
    }
    if from == to {
        return Err(HistoryStoreError::Query(format!(
            "`{from}` is already the name: there is nothing to rename"
        )));
    }
    let hostile = |name: &str| {
        vike_paths::store_path::refuse_a_path_hostile_symbol(name).map_err(HistoryStoreError::Query)
    };
    match from.split_once('/') {
        Some((outer, inner))
            if ![outer, inner].iter().any(|p| p.is_empty() || *p == "." || *p == "..")
                && !inner.contains('/') =>
        {
            hostile(outer)?;
            hostile(inner)?;
        }
        Some(_) => {
            return Err(HistoryStoreError::Query(format!(
                "`{from}`: an old name may hold ONE `/`, between two names, and only to reach a \
                 series filed in a nested leaf before the store refused the character \
                 (`PURR/USDC` inside `symbol=PURR`); any other `/` names no series"
            )));
        }
        None => hostile(from)?,
    }
    HistoryStoreDataFusion::refuse_a_hostile_symbol(to)
}

#[path = "rename_tests.rs"]
#[cfg(test)]
mod rename_tests;
