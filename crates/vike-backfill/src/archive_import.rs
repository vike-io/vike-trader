//! **The archive import's per-day store half, for every venue**: one request's hold on a `(store
//! root, symbol)`, the one-owner-per-UTC-day table, the day-owner lock and the day-by-day flow.
//!
//! `docs/decisions/0100-the-archive-import-reads-the-datahubs-own-box-and-a-day-has-one-owner.md` is
//! the verdict: the store dedups by commit key, never by row, so a row two lanes stored is stored
//! twice, and every UTC day of a series therefore has ONE owner. That rule and the order of a day
//! (classify, load, store, derive the bars) hold for every format the datahub imports, so they live
//! here. Everything a venue knows stays with the venue, behind [`ArchiveLane`]: the row type, the key
//! templates, which of the series' keys own which day, the append call, where and how bars are derived,
//! and the sentences that name its other lanes.
//! `crates/vike-backfill/src/venues/dukascopy/archive.rs`'s `DukascopyLane`,
//! `crates/vike-backfill/src/venues/binance_archive.rs`'s `BinanceArchiveLane` and
//! `crates/vike-backfill/src/venues/binance_archive/klines.rs`'s `BinanceKlinesLane` are the lanes.
//!
//! One rule here changes an owner rather than respecting one: decision 0268's point 4, a day another
//! lane holds with fewer rows than a whole day, REPLACED whole by the archive's day when the import
//! asks ([`ArchiveImport::replacing_holed`], [`holed_class`]). Only a lane whose rows have a whole-day
//! count can answer [`ArchiveLane::day_fill`], so only such a lane ever replaces a day.
//!
//! It names no file, no decoder and no wire crate (a day's refusal and bar counts are
//! `vike_data::archive_day`'s, which the wire carries as they are): the datahub walks the imports
//! directory, decodes a day, maps the result onto its own plan and answer, and hands this module
//! ROWS, so this crate still
//! names no bridge (`docs/decisions/0094-backfill-names-no-venue.md`).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, PoisonError};

use vike_data::archive_day::{ArchiveDayBars, ArchiveDayRefusal};
use vike_data::{SeriesCoverage, SeriesId, TimestampRange};
use vike_model::MS_PER_DAY;
use vike_store::HistoryStoreDataFusion;

use crate::error::CollectError;

// ── what differs between venues ───────────────────────────────────────────────────────────────

/// **What one venue's archive lane knows**, and all that [`ArchiveImport`] asks of it. The flow is
/// shared; a one-venue rule belongs in an implementation of this trait and never in this module.
pub trait ArchiveLane {
    /// The row a decoded file holds, the type the lane's append call takes.
    type Row;

    /// The word that opens this lane's refusals and log lines, such as `dukascopy-archive`.
    fn source(&self) -> &str;
    /// The series symbol the rows land under.
    fn symbol(&self) -> &str;
    /// The series the lane's rows land in: what a session reads its coverage and keys from, once.
    fn row_series(&self) -> SeriesId;
    /// A row's time, in epoch milliseconds.
    fn row_timestamp(row: &Self::Row) -> i64;
    /// The one-owner table over the series' commit log: which keys own which days.
    fn owners(&self, keys: &[String]) -> DayOwners;
    /// The sentence a day refused as [`ArchiveDayClass::Overlapped`] gets, `keys` already joined.
    fn overlapped_detail(&self, keys: &str) -> String;
    /// Store one decoded day. `bars` are the request's intervals, for a refusal to name.
    ///
    /// The outer `Err` is the store failing, which ends the request; the inner `Err` is that one day
    /// refused, with nothing stored and no key spent, and the request carries on.
    fn append_day(
        &self,
        hist: &HistoryStoreDataFusion,
        rows: &[Self::Row],
        day: i64,
        end: i64,
        bars: &[String],
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError>;
    /// What to do about a bar interval that does not divide a UTC day. One sentence.
    fn undividable_interval_remedy(&self) -> String;
    /// The series `interval`'s bars land in. A venue's own candles may not equal bars built from its
    /// rows, so this may be a `source=` lane of its own.
    fn bar_series(&self, interval: &str) -> SeriesId;
    /// The commit key of `interval`'s bars over `range`.
    fn bar_commit_key(&self, interval: &str, range: TimestampRange) -> String;
    /// Derive `interval`'s bars over `range` from the stored rows; the bars written.
    fn derive_bars(
        &self,
        hist: &HistoryStoreDataFusion,
        interval: &str,
        range: TimestampRange,
    ) -> Result<usize, CollectError>;
    /// The refusal a day ends in when the store refused [`Self::derive_bars`] for some intervals:
    /// `refused` is each interval with the store's reason, `written` what the others wrote, and
    /// `rows` says whether the day's rows are stored.
    fn bars_refused(
        &self,
        refused: &[(String, String)],
        written: &[ArchiveDayBars],
        rows: &str,
    ) -> ArchiveDayRefusal;
    /// How full the stored day is, for the replace rule ([`holed_class`]): `None`, the default, for
    /// a lane that never replaces a day, whose rows have no whole-day count (ticks).
    fn day_fill(
        &self,
        _hist: &HistoryStoreDataFusion,
        _day: i64,
        _end: i64,
    ) -> Result<Option<ArchiveDayFill>, CollectError> {
        Ok(None)
    }
    /// Replace the whole stored day with `rows`, under the lane's day key: only reached for a
    /// [`ArchiveDayClass::Replace`] day, which a lane whose [`Self::day_fill`] answers `None` never
    /// gets. The `Result`s read as [`Self::append_day`]'s.
    fn replace_day(
        &self,
        _hist: &HistoryStoreDataFusion,
        _rows: &[Self::Row],
        _day: i64,
        _end: i64,
    ) -> Result<Result<ArchiveDayAppend, ArchiveDayRefusal>, CollectError> {
        Ok(Err(ArchiveDayRefusal::new(
            "NoReplace",
            format!("{} replaces no stored day. Nothing was written.", self.source()),
        )))
    }
}

/// How many rows the store holds for one day against how many a whole day holds: what
/// [`ArchiveLane::day_fill`] answers, and a day is HOLED when `stored < full`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveDayFill {
    /// Distinct rows stored on the day.
    pub stored: usize,
    /// Rows a whole day holds.
    pub full: usize,
}

/// What [`ArchiveLane::append_day`] stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveDayAppend {
    /// Rows stored.
    pub rows: usize,
    /// The key the rows are stored under, for a later refusal to name.
    pub key: String,
    /// Every key the append spent, each with its lane, so a later question in the same request
    /// answers as a fresh read of the store would.
    pub spent: Vec<(DayOwnerRole, String)>,
}

// ── the per-day answers ───────────────────────────────────────────────────────────────────────

// A day's refusal and its bar counts are `vike_data::archive_day`'s, the words the datahub's wire
// carries unchanged. The classes this module mints are `TooRecent`, `Overlapped` and `OutsideDay`; a
// lane and a `load` closure handed to `ArchiveImport::import_day` mint their own, and the import
// passes them through untouched.

/// What [`ArchiveImport::import_day`] did with one day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayResult {
    /// Stored: `ticks` rows under the archive day key, and the bars derived from them.
    Imported {
        /// Rows stored.
        ticks: usize,
        /// Bars written per requested interval.
        bars: Vec<ArchiveDayBars>,
    },
    /// The file decoded to NO rows. Nothing is stored and no key is spent, so the day stays
    /// [`ArchiveDayClass::Free`] and a later re-publish can still fill it.
    Empty,
    /// A HELD day: nothing decoded, and every requested interval derived from the stored rows — `0`
    /// rows for one already stored.
    ToppedUp {
        /// Bars written per requested interval.
        bars: Vec<ArchiveDayBars>,
    },
    /// A [`ArchiveDayClass::Replace`] day: the stored day removed whole, and the file's rows stored
    /// in its place under the archive day key, which owns the day from now on.
    Replaced {
        /// Rows stored from the file.
        rows: usize,
        /// Rows the store held for the day before, now removed.
        stored: usize,
    },
    /// The day was refused, and the request carries on to the next. Its `detail` says whether its
    /// rows were stored: a refusal before or AT the row write spends no key; a refusal of a BAR
    /// write leaves the day's rows stored and its bars at that interval not.
    Refused(ArchiveDayRefusal),
}

/// What the archive import does with ONE UTC day that has a daily file — the design's one-owner table
/// (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §4.2), computed by
/// [`DayOwners::classify`] and checked in VARIANT ORDER, the first that applies deciding: so a recent
/// day is never imported, and a held day is never also reported as overlapped.
///
/// It is the store half of `crates/vike-datahub-client/src/archive.rs`'s `ArchiveDayClassWire`, one variant for
/// one, minus that type's plan-time `Refused` (a mixed-layout day or a header past a cap), which is
/// the datahub's to decide before the store is ever asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayClass {
    /// The day ends inside the lane's publication margin. Refused: "import again later". The archive
    /// never writes a provisional key.
    TooRecent,
    /// The archive already stored the day: its day key is spent. The file is skipped and never
    /// decoded; any requested interval still missing derives from the stored rows.
    HeldByArchive,
    /// Another lane already stored the day under ITS key over the day's own bounds, a
    /// [`DayOwnerRole::Http`] key. Skipped and topped up exactly as [`Self::HeldByArchive`].
    HeldByHttp,
    /// The ONLY key meeting the day is a [`DayOwnerRole::Provisional`] one whose range IS the day. The
    /// import stores the day and supersedes `key` in the same locked publish.
    Supersede {
        /// The provisional key the import supersedes.
        key: String,
    },
    /// Some OTHER key the lane read meets the day. REFUSED and listed: importing the whole day would
    /// store the overlapped rows twice, and importing the rest of it would trust a range this lane
    /// did not write. The day stays the other lane's to fill.
    Overlapped {
        /// Every key that meets the day, in the store's commit-log order.
        keys: Vec<String>,
    },
    /// No key meets the day: the import decodes it, stores it under the archive day key and derives
    /// the requested bars.
    Free,
    /// A day [`Self::HeldByHttp`] or [`Self::Overlapped`] whose stored rows are fewer than a whole
    /// day's, under an import that asked to replace such a day (decision 0268, point 4;
    /// [`holed_class`]). The import replaces the whole day with the file's and the archive day key
    /// owns it from then on. Never the class of a whole day, nor of one this lane already owns.
    Replace {
        /// Every key that met the day.
        keys: Vec<String>,
        /// Distinct rows the store holds for the day.
        stored: usize,
        /// Rows a whole day holds.
        full: usize,
    },
}

impl ArchiveDayClass {
    /// Whether the import STORES this day — [`Self::Free`], [`Self::Supersede`] and
    /// [`Self::Replace`]; the number a plan's typed confirmation counts.
    pub fn is_importable(&self) -> bool {
        matches!(self, Self::Free | Self::Supersede { .. } | Self::Replace { .. })
    }
}

// ── the one-owner table ───────────────────────────────────────────────────────────────────────

/// Which part a key plays in the one-owner table. A lane's [`ArchiveLane::owners`] reads each of its
/// series' keys into one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayOwnerRole {
    /// A key of the venue's other lane: over a day's own bounds it holds that day
    /// ([`ArchiveDayClass::HeldByHttp`]).
    Http,
    /// A key the import may supersede when its range IS the day ([`ArchiveDayClass::Supersede`]).
    Provisional,
    /// This lane's own day key ([`ArchiveDayClass::HeldByArchive`]).
    Archive,
    /// Any other writer's key the lane chose to read: it holds nothing whole, and a day it meets is
    /// [`ArchiveDayClass::Overlapped`].
    Other,
}

/// One of the series' keys, read: its lane and its inclusive `[start, end]` — `None` when its bounds
/// do not parse, which [`DayOwners::classify`] reads as meeting EVERY day, the conservative direction.
#[derive(Debug, Clone)]
pub struct DayOwnerKey {
    /// The part the key plays.
    pub lane: DayOwnerRole,
    /// Its inclusive range, or `None` when the lane could not read one.
    pub range: Option<(i64, i64)>,
    /// The key as the store holds it.
    pub key: String,
}

impl DayOwnerKey {
    /// Whether this key's range shares a millisecond with `[day, end]`. An inverted range is read as
    /// the span between its two bounds, so it can only meet MORE days than its writer meant.
    fn meets(&self, day: i64, end: i64) -> bool {
        self.range.is_none_or(|(a, b)| a.min(b) <= end && a.max(b) >= day)
    }
}

/// The keys of ONE series that own days, read once — what every day of an archive import is classified
/// against ([`Self::classify`]).
///
/// Built from the series' commit log, which the design reads ONCE per request rather than asking
/// `series_has_commit` per day: that call re-parses the whole manifest every time, and an exact-key
/// question cannot see a key whose RANGE meets a day under other bounds — which is exactly the
/// [`ArchiveDayClass::Overlapped`] case. [`ArchiveImport::open`] makes that read under the day-owner
/// lock; a lane's plan-only read makes it lock-free. A key the lane did not read owns no day.
#[derive(Debug, Clone)]
pub struct DayOwners {
    /// How long after a day ends it is still [`ArchiveDayClass::TooRecent`], in milliseconds.
    margin_ms: i64,
    keys: Vec<DayOwnerKey>,
}

impl DayOwners {
    /// A table over `keys`, judging a day's recency with a publication margin of `margin_ms`.
    pub fn new(margin_ms: i64, keys: Vec<DayOwnerKey>) -> Self {
        Self { margin_ms, keys }
    }

    /// The design's §4.2 table for the UTC day starting at `day`, judged at `now_ms` — see
    /// [`ArchiveDayClass`] for each row and why the order is load-bearing. `day` is the start of a
    /// UTC day; [`ArchiveImport::import_day`] refuses one that is not.
    pub fn classify(&self, day: i64, now_ms: i64) -> ArchiveDayClass {
        let end = day.saturating_add(MS_PER_DAY - 1);
        if !settled_at(end, now_ms, self.margin_ms) {
            return ArchiveDayClass::TooRecent;
        }
        let exact = |lane| self.keys.iter().any(|k| k.lane == lane && k.range == Some((day, end)));
        if exact(DayOwnerRole::Archive) {
            return ArchiveDayClass::HeldByArchive;
        }
        if exact(DayOwnerRole::Http) {
            return ArchiveDayClass::HeldByHttp;
        }
        let meeting: Vec<&DayOwnerKey> = self.keys.iter().filter(|k| k.meets(day, end)).collect();
        match meeting.as_slice() {
            [] => ArchiveDayClass::Free,
            [only] if only.lane == DayOwnerRole::Provisional && only.range == Some((day, end)) => {
                ArchiveDayClass::Supersede { key: only.key.clone() }
            }
            _ => ArchiveDayClass::Overlapped {
                keys: meeting.iter().map(|k| k.key.clone()).collect(),
            },
        }
    }

    /// Every key that meets the UTC day starting at `day`, in commit-log order.
    pub fn keys_meeting(&self, day: i64) -> Vec<String> {
        let end = day.saturating_add(MS_PER_DAY - 1);
        self.keys.iter().filter(|k| k.meets(day, end)).map(|k| k.key.clone()).collect()
    }

    /// Record what [`ArchiveImport::import_day`] just spent for `[day, end]`, so a later question in
    /// the same request answers [`ArchiveDayClass::HeldByArchive`], as a fresh read of the store would.
    fn record_import(&mut self, day: i64, end: i64, spent: Vec<(DayOwnerRole, String)>) {
        for (lane, key) in spent {
            self.keys.push(DayOwnerKey { lane, range: Some((day, end)), key });
        }
    }
}

/// **Decision 0268's point 4**: `class`, the one-owner table's answer for `day`, under an import that
/// asked to replace a holed day. A day another lane holds ([`ArchiveDayClass::HeldByHttp`] or
/// [`ArchiveDayClass::Overlapped`]) whose stored rows are fewer than a whole day's, by the lane's
/// [`ArchiveLane::day_fill`], is [`ArchiveDayClass::Replace`]; every other class, a whole day
/// included, is returned unchanged. A plan and an import ask it the same way, so a dry run lists
/// exactly the days an import replaces.
pub fn holed_class<L: ArchiveLane>(
    lane: &L,
    hist: &HistoryStoreDataFusion,
    owners: &DayOwners,
    day: i64,
    class: ArchiveDayClass,
) -> Result<ArchiveDayClass, CollectError> {
    if !matches!(class, ArchiveDayClass::HeldByHttp | ArchiveDayClass::Overlapped { .. }) {
        return Ok(class);
    }
    let end = day.saturating_add(MS_PER_DAY - 1);
    match lane.day_fill(hist, day, end)? {
        Some(ArchiveDayFill { stored, full }) if stored < full => {
            Ok(ArchiveDayClass::Replace { keys: owners.keys_meeting(day), stored, full })
        }
        _ => Ok(class),
    }
}

/// Whether something ending at `to` is SETTLED at `now_ms`: its last millisecond more than
/// `margin_ms` old.
pub(crate) fn settled_at(to: i64, now_ms: i64, margin_ms: i64) -> bool {
    to < now_ms.saturating_sub(margin_ms)
}

/// The last millisecond of the archive day starting at `day` — `None` unless `day` is a UTC midnight
/// at or after 1970 whose last millisecond is representable.
fn archive_day_end(day: i64) -> Option<i64> {
    if day < 0 || day.rem_euclid(MS_PER_DAY) != 0 {
        return None;
    }
    day.checked_add(MS_PER_DAY - 1)
}

// ── the day-owner lock (the design's change C) ────────────────────────────────────────────────

/// The `(store root, symbol)` pairs whose [`DayOwnerLock`] is held right now, in this process.
static DAY_OWNERS_HELD: Mutex<BTreeSet<(PathBuf, String)>> = Mutex::new(BTreeSet::new());

/// Woken every time a pair leaves [`DAY_OWNERS_HELD`].
static DAY_OWNER_RELEASED: Condvar = Condvar::new();

/// The ONE in-process lock a venue's lanes take around their STORE steps, per `(store root, symbol)`
/// — the design's change C (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §4.3).
/// [`ArchiveImport`] holds it for its whole request, from the moment it reads the series' keys to
/// classify a day until its last day is stored; another lane of the venue holds it for each of its own
/// store steps and never across a download. So the question each lane asks before it writes — does
/// the other lane own this day? — can never be answered by a store the other lane is about to change.
///
/// ⚠ **One process is enough**, because docs/decisions/0084 makes the datahub the store's only
/// writer; across processes the store's own `SeriesLock` still serialises each append, and its
/// `has_commit` under that lock still makes a same-key repeat a no-op — but it cannot see that two
/// DIFFERENT keys cover one day, which is what this lock is for.
///
/// ⚠ **Not re-entrant.** A thread that holds an [`ArchiveImport`] and then runs another lane over the
/// same `(store root, symbol)` waits for itself forever.
///
/// The root is canonicalised once per acquire (falling back to the path as given when that fails),
/// so two handles opened on one directory share one lock. A pair is a value in a set rather than a
/// mutex of its own, so nothing is kept for a symbol nobody holds; the guard's `Drop` frees it — on
/// unwind too — and wakes every waiter, each of which re-checks its own pair.
pub(crate) struct DayOwnerLock {
    held: (PathBuf, String),
}

impl DayOwnerLock {
    /// Block until no one holds `(hist's root, symbol)`, then hold it until the guard drops.
    pub(crate) fn acquire(hist: &HistoryStoreDataFusion, symbol: &str) -> Self {
        let root = hist.root();
        let held = (
            std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            symbol.to_string(),
        );
        let mut set = DAY_OWNERS_HELD.lock().unwrap_or_else(PoisonError::into_inner);
        while set.contains(&held) {
            set = DAY_OWNER_RELEASED.wait(set).unwrap_or_else(PoisonError::into_inner);
        }
        set.insert(held.clone());
        Self { held }
    }
}

impl Drop for DayOwnerLock {
    fn drop(&mut self) {
        let mut set = DAY_OWNERS_HELD.lock().unwrap_or_else(PoisonError::into_inner);
        set.remove(&self.held);
        drop(set);
        DAY_OWNER_RELEASED.notify_all();
    }
}

// ── one request ───────────────────────────────────────────────────────────────────────────────

/// ONE archive import request's hold on a `(store root, symbol)`: the day-owner lock, the series'
/// keys read once under it, and the request's bar intervals — what [`Self::import_day`] stores each
/// day through. Dropping it releases the lock.
///
/// The datahub's shape is: walk and plan, then open this, then [`Self::import_day`] for every day of
/// the request, in order, then drop. The lock is held for the whole request — the design's §5 caps
/// a request at 31 days, which is the datahub's to enforce at its door — so a day this session
/// classified cannot be written by another lane of the venue before this session stores it: that lane
/// takes the same lock for each of its store steps. ⚠ So a thread holding one must not run that lane
/// over the same series: the lock is not re-entrant.
pub struct ArchiveImport<'h, L> {
    hist: &'h HistoryStoreDataFusion,
    lane: L,
    bars: Vec<String>,
    now_ms: i64,
    coverage: SeriesCoverage,
    owners: DayOwners,
    /// Set by [`Self::replacing_holed`]: a holed day another lane holds is replaced, not skipped.
    replace_holed: bool,
    _owner: DayOwnerLock,
}

impl<'h, L: ArchiveLane> ArchiveImport<'h, L> {
    /// Validate `bars`, take the day-owner lock for `(hist's root, the lane's symbol)` — waiting for
    /// any other lane's store step in flight — and read the series' coverage and keys ONCE.
    ///
    /// `bars` are the intervals derived per stored day. Each must have a positive width that DIVIDES a
    /// UTC day; one that does not (`7m`) has buckets straddling midnight, which a day's import cannot
    /// build, and is REFUSED here, before anything is locked, read or written. An empty list imports
    /// the rows alone.
    ///
    /// `now_ms` is the one clock read the whole request is judged against.
    pub fn open(
        hist: &'h HistoryStoreDataFusion,
        lane: L,
        bars: &[String],
        now_ms: i64,
    ) -> Result<Self, CollectError> {
        for interval in bars {
            let divides = vike_marketdata::interval::bar_step_ms(interval)
                .is_some_and(|step| MS_PER_DAY % step == 0);
            if !divides {
                return Err(CollectError::Refused(format!(
                    "{} {}: interval {interval:?} does not divide a UTC day, so a day's import \
                     cannot build its bars — a bucket would straddle midnight. {} Nothing was \
                     locked, read or written.",
                    lane.source(),
                    lane.symbol(),
                    lane.undividable_interval_remedy(),
                )));
            }
        }
        let owner = DayOwnerLock::acquire(hist, lane.symbol());
        let (coverage, keys) = hist.series_facts(&lane.row_series())?;
        let owners = lane.owners(&keys);
        Ok(Self {
            hist,
            lane,
            bars: bars.to_vec(),
            now_ms,
            coverage,
            owners,
            replace_holed: false,
            _owner: owner,
        })
    }

    /// Replace a holed day another lane holds rather than skip it: the import's explicit option
    /// (decision 0268, point 4; [`holed_class`]). Without it every day classifies as before.
    pub fn replacing_holed(mut self) -> Self {
        self.replace_holed = true;
        self
    }

    /// The series' coverage as this session read it, under the lock — what a plan reports as the
    /// store's size before the import.
    pub fn coverage(&self) -> &SeriesCoverage {
        &self.coverage
    }

    /// [`DayOwners::classify`] for `day`, against the keys this session read and the days it has
    /// imported since, at the session's `now_ms`.
    pub fn classify(&self, day: i64) -> ArchiveDayClass {
        self.owners.classify(day, self.now_ms)
    }

    /// [`Self::classify`], then, under [`Self::replacing_holed`], [`holed_class`]: the class
    /// [`Self::import_day`] acts on. `Err` is the store failing to say how full the day is.
    pub fn classify_for_import(&self, day: i64) -> Result<ArchiveDayClass, CollectError> {
        let class = self.classify(day);
        if !self.replace_holed {
            return Ok(class);
        }
        holed_class(&self.lane, self.hist, &self.owners, day, class)
    }

    /// Import the UTC day starting at `day`: classify it, and act on its class — the design's §4.2
    /// table and §4.4:
    ///
    /// - [`ArchiveDayClass::Free`] / [`ArchiveDayClass::Supersede`]: call `load` (the decode), store
    ///   the rows through the lane's [`ArchiveLane::append_day`], then derive every requested interval.
    /// - [`ArchiveDayClass::HeldByArchive`] / [`ArchiveDayClass::HeldByHttp`]: `load` is NEVER called;
    ///   each requested interval still missing derives from the stored rows — which is also how a
    ///   request killed between a day's rows and its bars RESUMES.
    /// - [`ArchiveDayClass::TooRecent`] / [`ArchiveDayClass::Overlapped`]: `load` is never called and
    ///   nothing is written; the day is [`ArchiveDayResult::Refused`].
    ///
    /// The answer is per day, so the caller goes on to the next: `load`'s own refusal (a file that
    /// does not decode), an empty payload, a row outside the day, and a refusal from the lane's append
    /// each cost that one day and no key. `Err` is reserved for what is not one day's problem — the
    /// store failing, or a `day` that is not a UTC midnight at or after 1970, which is the caller's
    /// mistake and is refused before anything is read.
    ///
    /// Holds at most ONE day's rows: they are dropped once stored, before the bars read the day back.
    /// Logs one line per day, the boundary.
    pub fn import_day(
        &mut self,
        day: i64,
        load: impl FnOnce() -> Result<Vec<L::Row>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let source = self.lane.source().to_string();
        let symbol = self.lane.symbol().to_string();
        let Some(end) = archive_day_end(day) else {
            return Err(CollectError::Refused(format!(
                "{source} {symbol}: {day} is not the start of a UTC day at or after 1970, \
                 which is what an archive day is. Nothing was read or written."
            )));
        };
        let result = match self.classify_for_import(day)? {
            ArchiveDayClass::TooRecent => ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "TooRecent",
                format!(
                    "the day ends inside the {} ms publication margin, so the vendor may still be \
                     publishing it — import it again later. Nothing was read and no commit key was \
                     spent.",
                    self.owners.margin_ms
                ),
            )),
            ArchiveDayClass::Overlapped { keys } => {
                let keys = keys.join(", ");
                ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                    "Overlapped",
                    self.lane.overlapped_detail(&keys),
                ))
            }
            ArchiveDayClass::HeldByArchive | ArchiveDayClass::HeldByHttp => {
                let DayBars { written, refused } = self.derive_day(day, end)?;
                if refused.is_empty() {
                    ArchiveDayResult::ToppedUp { bars: written }
                } else {
                    ArchiveDayResult::Refused(self.lane.bars_refused(
                        &refused,
                        &written,
                        "its ticks were already stored, and stay so",
                    ))
                }
            }
            ArchiveDayClass::Free | ArchiveDayClass::Supersede { .. } => {
                self.store_day(day, end, load)?
            }
            ArchiveDayClass::Replace { stored, .. } => {
                self.replace_stored_day(day, end, stored, load)?
            }
        };
        tracing::info!("{source} {symbol} [{day}, {end}]: {result:?}");
        Ok(result)
    }

    /// `load`'s rows for `[day, end]`, or the day's result when there are none to store: the file's
    /// own refusal, an empty file, or a row outside the day.
    fn load_day(
        day: i64,
        end: i64,
        load: impl FnOnce() -> Result<Vec<L::Row>, ArchiveDayRefusal>,
    ) -> Result<Vec<L::Row>, ArchiveDayResult> {
        let rows = load().map_err(ArchiveDayResult::Refused)?;
        if rows.is_empty() {
            return Err(ArchiveDayResult::Empty);
        }
        // The decoder places every row inside its file's day; this holds the one-owner rule even
        // against a caller that did not, because a stray row would sit on a day this key does not
        // name — and another lane may own that day.
        if let Some(stray) = rows.iter().find(|r| !(day..=end).contains(&L::row_timestamp(r))) {
            return Err(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "OutsideDay",
                format!(
                    "a tick at {} lies outside the day [{day}, {end}] the file names. Nothing was \
                     stored and no commit key was spent.",
                    L::row_timestamp(stray)
                ),
            )));
        }
        Ok(rows)
    }

    /// The store half of a REPLACE day — see [`Self::import_day`]. The file must hold MORE rows
    /// than the store does, or replacing the day would gain nothing, or lose rows the live path
    /// caught and the archive did not: such a day is refused and stays as it was.
    fn replace_stored_day(
        &mut self,
        day: i64,
        end: i64,
        stored: usize,
        load: impl FnOnce() -> Result<Vec<L::Row>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let rows = match Self::load_day(day, end, load) {
            Ok(rows) => rows,
            Err(result) => return Ok(result),
        };
        if rows.len() <= stored {
            return Ok(ArchiveDayResult::Refused(ArchiveDayRefusal::new(
                "ArchiveNotFuller",
                format!(
                    "the file holds {} rows for this day and the store already holds {stored}, so \
                     replacing the day would gain nothing and could lose rows. The stored day stays \
                     as it is. Nothing was written and no commit key was spent.",
                    rows.len()
                ),
            )));
        }
        let appended = match self.lane.replace_day(self.hist, &rows, day, end)? {
            Ok(appended) => appended,
            Err(refusal) => return Ok(ArchiveDayResult::Refused(refusal)),
        };
        self.owners.record_import(day, end, appended.spent);
        Ok(ArchiveDayResult::Replaced { rows: appended.rows, stored })
    }

    /// The store half of a FREE or SUPERSEDE day — see [`Self::import_day`].
    fn store_day(
        &mut self,
        day: i64,
        end: i64,
        load: impl FnOnce() -> Result<Vec<L::Row>, ArchiveDayRefusal>,
    ) -> Result<ArchiveDayResult, CollectError> {
        let rows = match Self::load_day(day, end, load) {
            Ok(rows) => rows,
            Err(result) => return Ok(result),
        };
        let appended = match self.lane.append_day(self.hist, &rows, day, end, &self.bars)? {
            Ok(appended) => appended,
            Err(refusal) => return Ok(ArchiveDayResult::Refused(refusal)),
        };
        // The bars re-read the day from the store: the decoded rows are dead weight now.
        drop(rows);
        let ArchiveDayAppend { rows: ticks, key, spent } = appended;
        self.owners.record_import(day, end, spent);
        let DayBars { written, refused } = self.derive_day(day, end)?;
        if refused.is_empty() {
            return Ok(ArchiveDayResult::Imported { ticks, bars: written });
        }
        let stored = format!("its {ticks} ticks ARE stored under `{key}`, and stay so");
        Ok(ArchiveDayResult::Refused(self.lane.bars_refused(&refused, &written, &stored)))
    }

    /// Derive every requested interval for `[day, end]` from the stored rows, skipping one whose bar
    /// key is already spent without reading the day back.
    fn derive_day(&self, day: i64, end: i64) -> Result<DayBars, CollectError> {
        let range = TimestampRange::of(day, end);
        let mut bars = DayBars { written: Vec::new(), refused: Vec::new() };
        for interval in &self.bars {
            let series = self.lane.bar_series(interval);
            let key = self.lane.bar_commit_key(interval, range);
            let rows = if self.hist.series_has_commit(&series, &key)? {
                0
            } else {
                match self.lane.derive_bars(self.hist, interval, range) {
                    Ok(rows) => rows,
                    Err(CollectError::Data(e)) if e.is_supersede_refusal() => {
                        bars.refused.push((interval.clone(), e.to_string()));
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            };
            bars.written.push(ArchiveDayBars { interval: interval.clone(), rows: rows as u64 });
        }
        Ok(bars)
    }
}

/// What [`ArchiveImport`]'s per-day bars did, interval by interval.
struct DayBars {
    /// The bars written per requested interval — `0` for one already stored.
    written: Vec<ArchiveDayBars>,
    /// The intervals whose bar write the store refused, each with the store's reason.
    refused: Vec<(String, String)>,
}
