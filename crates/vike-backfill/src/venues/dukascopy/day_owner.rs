//! One owner per UTC day: the day-owner lock both quote lanes take, and the day classification.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, PoisonError};

use vike_data::{DataFusionHist, SeriesCoverage};
use vike_model::MS_PER_DAY;

use crate::error::CollectError;

#[cfg(doc)]
use super::archive::ArchiveImport;
#[cfg(doc)]
use super::backfill_quotes_then_bars;
use super::chunks::settled;
use super::{
    archive_quote_commit_key, provisional_quote_commit_key, quote_commit_key, quote_series,
};

// ── the day-owner lock (the design's change C) ────────────────────────────────────────────────

/// The `(store root, symbol)` pairs whose [`DayOwnerLock`] is held right now, in this process.
static DAY_OWNERS_HELD: Mutex<BTreeSet<(PathBuf, String)>> = Mutex::new(BTreeSet::new());

/// Woken every time a pair leaves [`DAY_OWNERS_HELD`].
static DAY_OWNER_RELEASED: Condvar = Condvar::new();

/// The ONE in-process lock both Dukascopy quote lanes take around their STORE steps, per `(store
/// root, symbol)` — the design's change C (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md`
/// §4.3). [`ArchiveImport`] holds it for its whole request, from the moment it reads the series'
/// keys to classify a day until its last day is stored; [`backfill_quotes_then_bars`] holds it for
/// each chunk's store step and never across a download, re-asking the archive under it. So the
/// question each lane asks before it writes — does the other lane own this day? — can never be
/// answered by a store the other lane is about to change.
///
/// ⚠ **One process is enough**, because docs/decisions/0084 makes the datahub the store's only
/// writer; across processes the store's own `SeriesLock` still serialises each append, and its
/// `has_commit` under that lock still makes a same-key repeat a no-op — but it cannot see that two
/// DIFFERENT keys cover one day, which is what this lock is for.
///
/// ⚠ **Not re-entrant.** A thread that holds an [`ArchiveImport`] and then runs the HTTP lane over
/// the same `(store root, symbol)` waits for itself forever.
///
/// The root is canonicalised once per acquire (falling back to the path as given when that fails),
/// so two handles opened on one directory share one lock. A pair is a value in a set rather than a
/// mutex of its own, so nothing is kept for a symbol nobody holds; the guard's `Drop` frees it — on
/// unwind too — and wakes every waiter, each of which re-checks its own pair.
pub(super) struct DayOwnerLock {
    held: (PathBuf, String),
}

impl DayOwnerLock {
    /// Block until no one holds `(hist's root, symbol)`, then hold it until the guard drops.
    pub(super) fn acquire(hist: &DataFusionHist, symbol: &str) -> Self {
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

// ── THE ARCHIVE HALF: one owner per UTC day, and the import of one day ──────────────────────────
//
// What the datahub's archive import calls (docs/decisions/0100). It names no file, no decoder and no
// wire type: the datahub walks the imports directory, decodes a day with the bridge, maps the result
// onto its own plan and answer, and hands this module QUOTES — so this crate still names no bridge
// (docs/decisions/0094).

/// What the archive import does with ONE UTC day that has a daily file — the design's one-owner table
/// (`docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §4.2), computed by
/// [`DayOwners::classify`] and checked in VARIANT ORDER, the first that applies deciding: so a recent
/// day is never imported, and a held day is never also reported as overlapped.
///
/// It is the store half of `crates/vike-datahub-client/src/archive.rs`'s `DayClass`, one variant for
/// one, minus that type's plan-time `Refused` (a mixed-layout day or a header past a cap), which is
/// the datahub's to decide before the store is ever asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDayClass {
    /// The day ends inside `PUBLICATION_MARGIN_MS` — the HTTP lane's own margin, through the same
    /// `settled` predicate. Refused: "import again later". The archive never writes a provisional key.
    TooRecent,
    /// The archive already stored the day: its [`archive_quote_commit_key`] is spent. The file is
    /// skipped and never decoded; any requested interval still missing derives from the stored ticks.
    HeldByArchive,
    /// The HTTP lane already stored the day under its CANONICAL day key, [`quote_commit_key`] over
    /// the day's own bounds. Skipped and topped up exactly as [`Self::HeldByArchive`].
    HeldByHttp,
    /// The ONLY Dukascopy quote key meeting the day is a PROVISIONAL one whose range IS the day — a
    /// recent full-day chunk the HTTP lane fetched early. The import stores the day and supersedes
    /// `key` in the same locked publish, exactly as the HTTP lane's own settled fetch would.
    Supersede {
        /// The provisional key the import supersedes.
        key: String,
    },
    /// Some OTHER Dukascopy quote key meets the day — a ragged request edge, a chunk of an interval
    /// that does not divide a day, a provisional window cut other than to the day, a tail written
    /// before per-chunk provisional keys, or a key whose bounds do not parse. REFUSED and listed:
    /// importing the whole day would store the overlapped ticks twice, and importing the rest of it
    /// would trust a range whose ticks may have been fetched before the vendor finished publishing.
    /// The day stays the HTTP lane's to fill.
    Overlapped {
        /// Every key that meets the day, in the store's commit-log order.
        keys: Vec<String>,
    },
    /// No Dukascopy quote key meets the day: the import decodes it, stores it under the archive day
    /// key and derives the requested bars.
    Free,
}

impl ArchiveDayClass {
    /// Whether the import STORES this day — [`Self::Free`] and [`Self::Supersede`]; the number a
    /// plan's typed confirmation counts.
    pub fn is_importable(&self) -> bool {
        matches!(self, Self::Free | Self::Supersede { .. })
    }
}

/// Which of the three Dukascopy quote LANES a commit key came down — the two HTTP-lane namespaces and
/// the archive's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteLane {
    /// [`quote_commit_key`]: a SETTLED HTTP-lane chunk (or a piece of one).
    Http,
    /// [`provisional_quote_commit_key`]: a RECENT HTTP-lane chunk, or the twin every settled commit
    /// pre-spends — an import's included.
    Provisional,
    /// [`archive_quote_commit_key`]: an imported day.
    Archive,
}

/// One of the series' Dukascopy quote keys, parsed: its lane and its inclusive `[start, end]` —
/// `None` when the bounds after the symbol do not parse, which [`DayOwners::classify`] reads as
/// meeting EVERY day, the conservative direction.
#[derive(Debug, Clone)]
struct LaneKey {
    lane: QuoteLane,
    range: Option<(i64, i64)>,
    key: String,
}

impl LaneKey {
    /// Whether this key's range shares a millisecond with `[day, end]`. An inverted range is read as
    /// the span between its two bounds, so it can only meet MORE days than its writer meant.
    fn meets(&self, day: i64, end: i64) -> bool {
        self.range.is_none_or(|(a, b)| a.min(b) <= end && a.max(b) >= day)
    }
}

/// The Dukascopy quote keys of ONE series, parsed once — what every day of an archive import is
/// classified against ([`Self::classify`]).
///
/// Built from the series' commit log, which the design reads ONCE per request rather than asking
/// `series_has_commit` per day: that call re-parses the whole manifest every time, and an exact-key
/// question cannot see a key whose RANGE meets a day under other bounds — which is exactly the
/// [`ArchiveDayClass::Overlapped`] case. [`quote_series_facts`] is the one read, and
/// [`ArchiveImport::begin`] makes it under the day-owner lock.
///
/// A key belongs to the series' lanes when it opens with one of the three formatters' heads FOR THIS
/// SYMBOL — `dukascopy:EURUSD:`, `dukascopy-provisional:EURUSD:`, `dukascopy-archive:EURUSD:`, each
/// derived from its formatter rather than spelled again here. Every other key is not a Dukascopy
/// quote lane's and owns no day: another producer's, or another symbol's sharing a grouped leaf.
#[derive(Debug, Clone)]
pub struct DayOwners {
    keys: Vec<LaneKey>,
}

impl DayOwners {
    /// Parse `keys` — a quote series' commit log, as `vike_data::DataFusionHist::series_facts` or
    /// `series_commits` answers it — for `symbol`'s three Dukascopy quote lanes.
    pub fn parse(symbol: &str, keys: &[String]) -> Self {
        let heads = [
            (QuoteLane::Http, key_head(quote_commit_key, symbol)),
            (QuoteLane::Provisional, key_head(provisional_quote_commit_key, symbol)),
            (QuoteLane::Archive, key_head(archive_quote_commit_key, symbol)),
        ];
        let keys = keys
            .iter()
            .filter_map(|key| {
                heads.iter().find_map(|(lane, head)| {
                    let bounds = key.strip_prefix(head.as_str())?;
                    Some(LaneKey { lane: *lane, range: parse_bounds(bounds), key: key.clone() })
                })
            })
            .collect();
        Self { keys }
    }

    /// The design's §4.2 table for the UTC day starting at `day`, judged at `now_ms` — see
    /// [`ArchiveDayClass`] for each row and why the order is load-bearing. `day` is the start of a
    /// UTC day; [`ArchiveImport::import_day`] refuses one that is not.
    pub fn classify(&self, day: i64, now_ms: i64) -> ArchiveDayClass {
        let end = day.saturating_add(MS_PER_DAY - 1);
        if !settled(end, now_ms) {
            return ArchiveDayClass::TooRecent;
        }
        let exact = |lane| self.keys.iter().any(|k| k.lane == lane && k.range == Some((day, end)));
        if exact(QuoteLane::Archive) {
            return ArchiveDayClass::HeldByArchive;
        }
        if exact(QuoteLane::Http) {
            return ArchiveDayClass::HeldByHttp;
        }
        let meeting: Vec<&LaneKey> = self.keys.iter().filter(|k| k.meets(day, end)).collect();
        match meeting.as_slice() {
            [] => ArchiveDayClass::Free,
            [only] if only.lane == QuoteLane::Provisional && only.range == Some((day, end)) => {
                ArchiveDayClass::Supersede { key: only.key.clone() }
            }
            _ => ArchiveDayClass::Overlapped {
                keys: meeting.iter().map(|k| k.key.clone()).collect(),
            },
        }
    }

    /// Record what [`ArchiveImport::import_day`] just spent for `[day, end]` — the archive day key and
    /// the provisional twin every settled commit pre-spends — so a later question in the same
    /// request answers [`ArchiveDayClass::HeldByArchive`], as a fresh read of the store would.
    pub(super) fn record_import(
        &mut self,
        day: i64,
        end: i64,
        archive_key: String,
        provisional_key: String,
    ) {
        for (lane, key) in
            [(QuoteLane::Archive, archive_key), (QuoteLane::Provisional, provisional_key)]
        {
            self.keys.push(LaneKey { lane, range: Some((day, end)), key });
        }
    }
}

/// The head `format` writes for `symbol` before its `{start}-{end}` bounds — `dukascopy:EURUSD:` for
/// [`quote_commit_key`] — read off the formatter itself with the bounds `0-0` cut away, so the three
/// templates are spelled once, in their formatters, and nowhere in the parser.
pub(super) fn key_head(format: fn(&str, i64, i64) -> String, symbol: &str) -> String {
    let mut head = format(symbol, 0, 0);
    head.truncate(head.len() - "0-0".len());
    head
}

/// The `{start}-{end}` bounds of a Dukascopy quote key, or `None` when they do not parse. The split
/// is the first `-` after the first character with an `i64` on each side, so a pre-1970 (negative)
/// bound on either side reads correctly.
pub(super) fn parse_bounds(bounds: &str) -> Option<(i64, i64)> {
    bounds
        .char_indices()
        .skip(1)
        .filter(|&(_, c)| c == '-')
        .find_map(|(at, _)| Some((bounds[..at].parse().ok()?, bounds[at + 1..].parse().ok()?)))
}

/// `symbol`'s Dukascopy quote series' coverage and commit log, from ONE manifest parse
/// (`vike_data::DataFusionHist::series_facts`): what a plan that writes nothing — a dry run —
/// classifies its days against through [`DayOwners::parse`], without taking the day-owner lock. An
/// import that WRITES takes the same read under the lock instead, through [`ArchiveImport::begin`].
pub fn quote_series_facts(
    hist: &DataFusionHist,
    symbol: &str,
) -> Result<(SeriesCoverage, Vec<String>), CollectError> {
    Ok(hist.series_facts(&quote_series(symbol))?)
}
