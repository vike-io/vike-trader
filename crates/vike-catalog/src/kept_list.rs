//! **The venues' KEPT instrument lists: the file, its one reader, and this process's read of them.**
//!
//! A venue's list lives in ONE place, `<project>/instruments/<venue>.json`
//! (`vike_paths::state_path::PROJECT_INSTRUMENTS_DIR`), and the datahub is its only writer: its weekly refresh and its
//! refresh-now verb (`crates/vike-datahub/src/instrument_lists.rs`; the owner's ruling of 2026-11-12,
//! `docs/decisions/0062`'s amendment). [`VenueInstrumentList`] is the file's body and a row of the datahub's
//! `InstrumentLists` reply; [`read_kept_list`] is the ONE reader of it, here at the bottom of the graph so the datahub,
//! the tradehub, the gatewayhub's bridges and the CLI read it alike.
//!
//! # This process's read ([`use_kept_lists`], [`kept_list`])
//!
//! A composition root that holds a project hands its `instruments/` folder in once; a library then asks
//! [`kept_list`] for a venue's rows wherever it would otherwise have read a symbol's NAME (bybit's book, decision C4
//! of the 2026-11-13 "remove all hardcoded symbols" package). A venue's list is read at its first ask and again once
//! it is [`KEPT_LIST_REREAD`] old, so a list the datahub refreshed counts within that. A process that installed no
//! folder (a test, a tool with no project) holds no list: every lookup misses, and each reader says what it does
//! then.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vike_model::Instrument;

/// **One venue's FETCHED list as the datahub KEEPS it**: the body of `<project>/instruments/<venue>.json` and a row
/// of the datahub's `InstrumentLists` reply (`vike_datahub_client::catalog::InstrumentListsReply`). The datahub is
/// its only writer. A failed or empty answer keeps the list and says why in [`Self::last_error`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueInstrumentList {
    /// The roster slug.
    pub venue: String,
    /// When the kept list was fetched, epoch ms; `None` until the first success.
    pub refreshed_ms: Option<i64>,
    /// The kept list hit the catalog's instrument cap (`vike_datahub_client::catalog::CATALOG_MAX_INSTRUMENTS`).
    #[serde(default)]
    pub truncated: bool,
    /// Why the LAST attempt kept the old list (a failure, or an empty answer over a list); `None` when it replaced it.
    #[serde(default)]
    pub last_error: Option<String>,
    /// When the last attempt ran, epoch ms.
    #[serde(default)]
    pub last_try_ms: Option<i64>,
    /// The kept list.
    pub instruments: Vec<Instrument>,
    /// The `raw_symbol`s of [`Self::instruments`] the operator added BY HAND (the datahub's
    /// `vike_datahub_client::proto::Request::OperatorInstruments`), folded in by the datahub where the venue's own
    /// listing has no row of that name; every other row is the venue's. Absent on the wire when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_by_operator: Vec<String>,
}

/// `<dir>/<venue>.json`, a venue's kept list's file: what the datahub writes and [`read_kept_list`] reads.
#[must_use]
pub fn kept_file(dir: &Path, venue: &str) -> PathBuf {
    dir.join(format!("{venue}.json"))
}

/// **The kept list of `venue` under `dir`** (`<project>/instruments`), or `None` when there is none. The ONE reader of
/// the file the datahub writes. A file that does not parse is logged and read as none: the next refresh replaces it,
/// and it holds nothing a fetch cannot bring back.
#[must_use]
pub fn read_kept_list(dir: &Path, venue: &str) -> Option<VenueInstrumentList> {
    let path = kept_file(dir, venue);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(list) => Some(list),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "instrument list unreadable; refetching it");
            None
        }
    }
}

/// Every kept list under `dir`, in roster order.
#[must_use]
pub fn read_kept_lists(dir: &Path) -> Vec<VenueInstrumentList> {
    vike_model::VENUES.iter().filter_map(|v| read_kept_list(dir, v)).collect()
}

/// **The kept rows a process judging a market's closed time reads**: those of every venue whose row can name a
/// session class (`crate::session_venue`: alpaca and the FX-week brokers), from `dir` (`<project>/instruments`).
/// Empty with no `dir` (no project) or no list: every symbol's class is then unknown (`crate::session_class_of`).
/// Read whole each call: the files are small, and a list the datahub fetched after this process started is read at
/// its next use.
#[must_use]
pub fn read_session_rows(dir: Option<&Path>) -> Vec<Instrument> {
    let Some(dir) = dir else { return Vec::new() };
    vike_model::VENUES
        .iter()
        .filter(|v| crate::session_venue(v))
        .filter_map(|v| read_kept_list(dir, v))
        .flat_map(|list| list.instruments)
        .collect()
}

/// How old a venue's list this process read may get before [`kept_list`] reads its file again: a minute. A list
/// changes weekly; a list the datahub refreshed (a new contract, a refresh-now) counts within the minute.
pub const KEPT_LIST_REREAD: Duration = Duration::from_secs(60);

/// The folder [`use_kept_lists`] installed, and each venue's rows as last read.
struct KeptLists {
    dir: PathBuf,
    read: Mutex<HashMap<String, KeptRead>>,
}

/// One venue's file as last read: when, and its rows (`None`: no list).
struct KeptRead {
    at: Instant,
    rows: Option<Arc<Vec<Instrument>>>,
}

static KEPT: OnceLock<KeptLists> = OnceLock::new();

/// **Read this process's kept lists from `dir`** (`<project>/instruments`,
/// `vike_paths::state_path::instruments_dir_beside` over the booted settings directory). Called once by a
/// composition root that holds a project; the first call wins.
pub fn use_kept_lists(dir: PathBuf) {
    let _ = KEPT.set(KeptLists { dir, read: Mutex::default() });
}

/// Rows held for a venue in place of its file ([`hold_kept_list`]): a process that reads no folder (a test).
fn held() -> &'static Mutex<HashMap<String, Arc<Vec<Instrument>>>> {
    static HELD: OnceLock<Mutex<HashMap<String, Arc<Vec<Instrument>>>>> = OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

/// **Hold `rows` as `venue`'s kept list in this process**, in place of its file: what a test hands the readers of
/// [`kept_list`] (a process that reads no folder). Replaces the venue's last held rows.
pub fn hold_kept_list(venue: &str, rows: Vec<Instrument>) {
    held().lock().unwrap_or_else(PoisonError::into_inner).insert(venue.to_string(), Arc::new(rows));
}

/// **`venue`'s kept rows as this process reads them**: the rows [`hold_kept_list`] held, else the installed folder's
/// file ([`use_kept_lists`]), or `None` when neither holds the venue's list. A file is read at the first ask and again
/// once [`KEPT_LIST_REREAD`] old.
#[must_use]
pub fn kept_list(venue: &str) -> Option<Arc<Vec<Instrument>>> {
    if let Some(rows) = held().lock().unwrap_or_else(PoisonError::into_inner).get(venue) {
        return Some(Arc::clone(rows));
    }
    let kept = KEPT.get()?;
    let mut read = kept.read.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(last) = read.get(venue)
        && last.at.elapsed() < KEPT_LIST_REREAD
    {
        return last.rows.clone();
    }
    let rows = read_kept_list(&kept.dir, venue).map(|list| Arc::new(list.instruments));
    read.insert(venue.to_string(), KeptRead { at: Instant::now(), rows: rows.clone() });
    rows
}

/// **The kept row of `symbol` on `venue` as this process reads them**: [`kept_list`] through `crate::kept_row` (the
/// exact spelling, else the one row the door's grammar spells alike), or `None` when no list is held or it carries no
/// such row. What a reader that would otherwise read the NAME asks (a paper book's quote and unit, a mount's money).
/// ⚠ bybit's exec names are not its list's keys (`X.S`, a bare perpetual): its reader is `crate::bybit_kept_row`.
#[must_use]
pub fn kept_row_of(venue: &str, symbol: &str) -> Option<Instrument> {
    let rows = kept_list(venue)?;
    crate::kept_row(&rows, venue, symbol).cloned()
}

#[path = "kept_list_tests.rs"]
#[cfg(test)]
mod kept_list_tests;
