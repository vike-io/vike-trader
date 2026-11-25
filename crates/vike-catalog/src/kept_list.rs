//! **The venues' KEPT instrument lists: the file, its one reader, and this process's read of them.**
//!
//! A venue's list lives in ONE place, `<project>/instruments/<venue>.json`
//! (`vike_paths::state_path::PROJECT_INSTRUMENTS_DIR`), and the datahub is its only writer, on the operator's
//! refresh (no timer, 2026-11-16) (`crates/vike-datahub/src/instrument_lists.rs`; the owner's ruling of 2026-11-12,
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
/// the file the datahub writes. A file that exists and cannot be read or does not parse is an ERROR naming it, and read
/// as none: the operator's next refresh replaces it, and it holds nothing a fetch cannot bring back (the operator's own
/// rows are the settings database's). It is NOT set aside: every process reads this file, the datahub alone writes it,
/// and a reader moving it would take a list that failed one read away from every other process until a refresh.
#[must_use]
pub fn read_kept_list(dir: &Path, venue: &str) -> Option<VenueInstrumentList> {
    let path = kept_file(dir, venue);
    let failure = match vike_utility::atomic_file::read_if_present(&path) {
        Ok(None) => return None,
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
            Ok(list) => return Some(list),
            Err(e) => e.to_string(),
        },
        Err(e) => e.to_string(),
    };
    tracing::error!(
        path = %path.display(),
        error = %failure,
        "the instrument list exists and cannot be read, so this venue has none: refresh it \
         (`vike-cli data catalog refresh --venue {venue}`)"
    );
    None
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
/// changes only when the operator refreshes it, and that refresh counts here within the minute.
pub const KEPT_LIST_REREAD: Duration = Duration::from_secs(60);

/// The folder [`use_kept_lists`] installed, and each venue's rows as last read.
struct KeptLists {
    dir: PathBuf,
    read: Mutex<HashMap<String, KeptRead>>,
}

/// One venue's file as last read: when, and its rows (`None`: no list).
struct KeptRead {
    at: Instant,
    rows: Option<Arc<KeptRows>>,
}

/// **One venue's kept rows as this process holds them, and the maps of their STORED names**, built once per read of
/// the file (or per hold): a lookup reads them and spells nothing (the owner's ruling «Б», 2026-11-16).
struct KeptRows {
    rows: Arc<Vec<Instrument>>,
    /// Each stored user name of a row of the venue, ASCII upper case, to the indexes of the rows that store it.
    by_user: HashMap<String, Vec<usize>>,
    /// Each row's own name (`raw_symbol`) to the user name it stores.
    user_of_raw: HashMap<String, String>,
    /// Every row of the venue stores its naming; a file written before names were stored does not.
    named: bool,
}

impl KeptRows {
    /// The maps of `rows`' stored names on `venue`.
    fn of(venue: &str, rows: Arc<Vec<Instrument>>) -> Self {
        let mut by_user: HashMap<String, Vec<usize>> = HashMap::new();
        let mut user_of_raw = HashMap::new();
        for (at, row) in rows.iter().enumerate().filter(|(_, row)| row.venue == venue) {
            if let Ok(user) = crate::user_name_of_row(row) {
                by_user.entry(user.to_ascii_uppercase()).or_default().push(at);
                user_of_raw.insert(row.raw_symbol.clone(), user);
            }
        }
        let named = rows.iter().all(|row| row.venue != venue || row.naming.is_some());
        Self { rows, by_user, user_of_raw, named }
    }

    /// The one row whose stored user name is `user` (ASCII case and blanks ignored), off the map; else
    /// `crate::kept_row_of_user_name`'s words for why there is none (no row, two, a list that predates names).
    fn row_of_user(&self, venue: &str, user: &str) -> Result<&Instrument, String> {
        match self.by_user.get(&user.trim().to_ascii_uppercase()).map(Vec::as_slice) {
            Some([at]) if self.named => Ok(&self.rows[*at]),
            _ => crate::kept_row_of_user_name(&self.rows, venue, user),
        }
    }
}

static KEPT: OnceLock<KeptLists> = OnceLock::new();

/// **Read this process's kept lists from `dir`** (`<project>/instruments`,
/// `vike_paths::state_path::instruments_dir_beside` over the booted settings directory). Called once by a
/// composition root that holds a project; the first call wins.
pub fn use_kept_lists(dir: PathBuf) {
    let _ = KEPT.set(KeptLists { dir, read: Mutex::default() });
}

/// Rows held for a venue in place of its file ([`hold_kept_list`]): a process that reads no folder (a test).
fn held() -> &'static Mutex<HashMap<String, Arc<KeptRows>>> {
    static HELD: OnceLock<Mutex<HashMap<String, Arc<KeptRows>>>> = OnceLock::new();
    HELD.get_or_init(Mutex::default)
}

/// **Hold `rows` as `venue`'s kept list in this process**, in place of its file: what a test hands the readers of
/// [`kept_list`] (a process that reads no folder). Replaces the venue's last held rows. A test is a writer too: each
/// row that stores no naming is spelled as the datahub's write spells it (`crate::spelled`).
pub fn hold_kept_list(venue: &str, rows: Vec<Instrument>) {
    let rows = KeptRows::of(venue, Arc::new(crate::spelled(rows)));
    held().lock().unwrap_or_else(PoisonError::into_inner).insert(venue.to_string(), Arc::new(rows));
}

/// **`venue`'s kept rows as this process reads them**: the rows [`hold_kept_list`] held, else the installed folder's
/// file ([`use_kept_lists`]), or `None` when neither holds the venue's list. A file is read at the first ask and again
/// once [`KEPT_LIST_REREAD`] old.
#[must_use]
pub fn kept_list(venue: &str) -> Option<Arc<Vec<Instrument>>> {
    read_kept(venue, KEPT_LIST_REREAD).map(|kept| Arc::clone(&kept.rows))
}

/// **[`kept_list`] read from its file NOW**, whatever the age of this process's last read (which it replaces): for a
/// COLD path that must see a list the datahub has just refreshed (an account's start, asked again because its venue's
/// list was refreshed: `vike-tradehub`'s account watcher), never a per-order one.
#[must_use]
pub fn kept_list_now(venue: &str) -> Option<Arc<Vec<Instrument>>> {
    read_kept(venue, Duration::ZERO).map(|kept| Arc::clone(&kept.rows))
}

/// [`kept_list`]'s body: the held rows, else the installed folder's file read again once `reread` old, with the maps
/// of its stored names.
fn read_kept(venue: &str, reread: Duration) -> Option<Arc<KeptRows>> {
    if let Some(rows) = held().lock().unwrap_or_else(PoisonError::into_inner).get(venue) {
        return Some(Arc::clone(rows));
    }
    let kept = KEPT.get()?;
    let mut read = kept.read.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(last) = read.get(venue)
        && last.at.elapsed() < reread
    {
        return last.rows.clone();
    }
    let rows = read_kept_list(&kept.dir, venue)
        .map(|list| Arc::new(KeptRows::of(venue, Arc::new(list.instruments))));
    read.insert(venue.to_string(), KeptRead { at: Instant::now(), rows: rows.clone() });
    rows
}

/// **The kept row of the USER name `symbol` on `venue` as this process reads them** (decision 0290): the one row of
/// [`kept_list`] whose STORED user name it is, ignoring ASCII case, read off the map built when the list was read; or
/// `None` when no list is held, it carries no such row, or two, or it predates stored names. What a reader of an
/// engine's, an order's or a fill's symbol asks instead of reading the NAME (a paper book's quote and unit, a mount's
/// money, a fee lane): bybit's `BTCUSDT.P` is the perpetual's row and `BTCUSDT` the spot pair's (audit flag BYB-7).
#[must_use]
pub fn kept_row_of(venue: &str, symbol: &str) -> Option<Instrument> {
    read_kept(venue, KEPT_LIST_REREAD)?.row_of_user(venue, symbol).ok().cloned()
}

/// **The KEPT row's own name (`raw_symbol`) of the USER name `user` on `venue`** (decision 0290), as this process reads
/// the list ([`kept_list`]): the name a bridge whose code speaks its venue's listing signs, streams and reads history
/// by (okx's instId `BTC-USDT-SWAP` for `BTCUSDT.P`, deribit's `BTC-PERPETUAL` for `BTCUSD.P`, binance's laned
/// `BTCUSD_PERP.P` for `BTCUSD.P`). Its way back is [`kept_user_name_of_raw`].
///
/// # Errors
/// No list of `venue` is held, it predates stored names, no row answers `user`, or two do:
/// `crate::kept_row_of_user_name`'s words.
pub fn kept_raw_of_user_name(venue: &str, user: &str) -> Result<String, String> {
    match read_kept(venue, KEPT_LIST_REREAD) {
        Some(kept) => kept.row_of_user(venue, user).map(|row| row.raw_symbol.clone()),
        None => crate::kept_row_of_user_name(&[], venue, user).map(|row| row.raw_symbol.clone()),
    }
}

/// **The USER name the kept row named `raw` on `venue` stores** (`BTCUSDT.P` for okx's `BTC-USDT-SWAP`), or `None` when
/// no kept row of this process's list is named so (no list, a row the venue listed after the last refresh) or its row
/// stores no user name: the caller says what it books then. Read off the map built when the list was read (a fill asks
/// on every row the venue sends).
#[must_use]
pub fn kept_user_name_of_raw(venue: &str, raw: &str) -> Option<String> {
    read_kept(venue, KEPT_LIST_REREAD)?.user_of_raw.get(raw).cloned()
}

#[path = "kept_list_tests.rs"]
#[cfg(test)]
mod kept_list_tests;
