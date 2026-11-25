//! **The venues' KEPT instrument lists**: one `<venue>.json` per venue under `<project>/instruments`
//! (`vike_paths::state_path::PROJECT_INSTRUMENTS_DIR`), each a `vike_catalog::VenueInstrumentList`.
//! This daemon is their ONLY writer (the owner's ruling of 2026-11-12; `docs/decisions/0062`'s amendment), and it
//! writes one ONLY when the operator asks (the owner's ruling of 2026-11-16: no timer, for every venue):
//!
//! * **refresh** — `Request::RefreshInstrumentLists` ([`refresh_instrument_lists_verb`]), one venue or every venue the
//!   catalog lane serves: `vike-cli data catalog refresh [--venue V]` and the Data Manager's Refresh send it. A list a
//!   broker's login hands the gatewayhub (dukascopy's, oanda's, ctrader's) is held there in memory and reaches this
//!   folder only through a refresh; nothing is written at a login.
//! * **naming a list an older build wrote** — ONCE, at this daemon's start, before it listens ([`name_older_kept_lists`],
//!   the owner's "make it smart" of 2026-11-17): a file with a row that stores no name is copied aside and written
//!   named; a file whose rows store their names is never rewritten.
//!
//! Every row it writes carries its decision 0290 USER name (or why the grammar names it not), spelled ONCE at that
//! write and read by everyone after (the owner's ruling «Б» of 2026-11-16, simplified the same day: [`write_list`],
//! over `vike_catalog::spelled`); a naming the venue's catalog stated on a row is kept as stated. The wire name is the
//! row's own fields and the store name the store's rule over the user name, so neither is written. A refresh and an
//! operator's row (`vike-cli data instruments add`) both write through it.
//!
//! A refresh fetches through `crate::server::catalog_outcome`, the `VenueCatalog` verb's own gates, bucket and memo,
//! and so through the gatewayhub (decision 0164: this daemon opens no venue connection). Clients read the lists with
//! `Request::InstrumentLists` ([`instrument_lists_verb`]), which also carries the operator's own credentialed lists from
//! the settings database; a backtesthub reads the rows and splits of the venues a run reads with
//! `Request::KeptRowsOfVenues` ([`kept_rows_verb`]: msgpack, and a stamp per venue it answers "unchanged" to).
//!
//! # The fold, and why it never loses a list
//!
//! [`fold`] is `merge_refresh`'s rule on the server: a listing REPLACES the kept list; a failure, or an EMPTY answer
//! over a list that held instruments, KEEPS it and records why in `last_error`; a routine refusal (`NotArmed`, a
//! venue the lane does not serve) changes nothing at all. Each file is written whole through
//! `vike_utility::atomic_file::write_atomic` (a temp file beside it, then a rename that also replaces an existing file on
//! Windows), and refreshes are serialized by one lock, so two refreshes never interleave a read and a write.
//!
//! # The parsed lists ([`PARSED`])
//!
//! Every backtest run asks for every list (~14 MB of JSON on the CI box), so [`instrument_lists_verb`] answers from the lists
//! as last parsed, and its answer is still what a fresh read of the files answers: [`write_list`], the files' one writer
//! (a refresh, the operator's rows, the start's naming all write through it), drops its venue's parsed list; a file whose
//! length or modification time changed since (an operator's hand edit) is read again at the next ask; and every parsed
//! list is read again once `vike_catalog::KEPT_LIST_REREAD` old, so even a hand edit that keeps both (a same-length file
//! put back with its old time) is answered within that minute. The two readers of one venue's rows per ask
//! (`SeriesGaps`' trading calendar and `StrategySymbolFacts`' kept row) read them the same way
//! ([`parsed_kept_list`]); every other reader of the files reads them as before.
//!
//! The route answers both reads in msgpack (`FEATURE_WIRE_MSGPACK`) with [`instrument_lists_msgpack`]: each venue's ROWS
//! written as msgpack ONCE per parse, beside it (the bytes [`kept_rows_verb`] answers too: ONE spelling of the kept rows,
//! one cache), and put into the reply as those bytes, so an ask neither copies a list nor writes one again (the CI box,
//! binance: 2,211 rows, 0.95 MB of JSON, 6.6 ms of a run's ask before). The operator's lists and the share splits are
//! read and written at every ask, so a split or an operator's list is answered at once; an operator's ROW is in the kept
//! file, written through [`write_list`]. The bytes are what the value writes, byte for byte. A JSON ask (an older
//! client) is answered the value. Bounded with the parsed lists: one written copy per kept file that parses (11.5 MB of
//! msgpack in all on the CI box once every venue has been asked).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use vike_catalog::{VenueInstrumentList, kept_file, read_kept_list};
use vike_datahub_client::catalog::{CatalogOutcome, InstrumentListsReply, VenueRefresh};
use vike_datahub_client::proto::Response;
use vike_model::{Contract, ContractCfdUnderlying, Instrument, InstrumentNaming};

use crate::catalog::CatalogLane;

/// One refresh at a time in this process, so a read-fold-write of one file never interleaves with another's.
static REFRESHING: Mutex<()> = Mutex::new(());

/// **The kept lists as this daemon last parsed them**, by folder and venue (the module doc's "The parsed lists"). A
/// file that is absent or does not parse is never held: it is read at every ask, as before.
static PARSED: Mutex<BTreeMap<(PathBuf, String), Parsed>> = Mutex::new(BTreeMap::new());

/// One venue's file as [`PARSED`] holds it: its length and modification time when read, when, its list, and its rows
/// as msgpack, made at the first ask that writes them ([`kept_rows_verb`], [`instrument_lists_msgpack`]).
struct Parsed {
    stamp: (u64, Option<SystemTime>),
    at: Instant,
    list: Arc<VenueInstrumentList>,
    rows: Option<Arc<EncodedRows>>,
}

/// Rows as msgpack (`vike_datahub_client::proto::kept_msgpack`) and the hex SHA-256 of those bytes.
struct EncodedRows {
    msgpack: Vec<u8>,
    digest: String,
}

fn encoded_rows(rows: &[Instrument]) -> Result<EncodedRows, String> {
    let msgpack = vike_datahub_client::proto::kept_msgpack(rows)?;
    Ok(EncodedRows { digest: vike_hub_protocol::hash::sha256_hex(&msgpack), msgpack })
}

/// **Write one venue's list atomically**: the directory is created on first use, then a temp file and a rename. Each
/// row that stores no names yet is written with them, spelled here once (`vike_catalog::spelled`, decision 0290):
/// no kept file is written unnamed. The venue's parsed list ([`PARSED`]) goes with the write.
pub fn write_list(dir: &Path, list: &VenueInstrumentList) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut named = list.clone();
    named.instruments = vike_catalog::spelled(named.instruments);
    let bytes = serde_json::to_vec(&named).map_err(std::io::Error::other)?;
    let written = vike_utility::atomic_file::write_atomic(&kept_file(dir, &list.venue), &bytes);
    // After the rename: a read racing it holds the lock until it has stored what it read, so this drops that too.
    PARSED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&(dir.to_path_buf(), list.venue.clone()));
    written
}

/// **Each of `venues`' kept lists under `dir`, as a fresh read answers it** (`vike_catalog::read_kept_list`), handed to
/// `take`: the parsed list of each venue whose file has the length and modification time it was read at, less than
/// `reread` ago; every other file read now. A venue with no list is skipped. The lock is held across a read, so
/// [`write_list`]'s drop never races a store.
fn parsed_kept<'v, T>(
    dir: &Path,
    reread: Duration,
    venues: impl Iterator<Item = &'v str>,
    mut take: impl FnMut(&mut Parsed) -> T,
) -> Vec<T> {
    let mut parsed = PARSED.lock().unwrap_or_else(PoisonError::into_inner);
    venues
        .filter_map(|venue| {
            let key = (dir.to_path_buf(), venue.to_string());
            let stamp = std::fs::metadata(kept_file(dir, venue))
                .ok()
                .map(|meta| (meta.len(), meta.modified().ok()));
            if let (Some(stamp), Some(held)) = (stamp, parsed.get_mut(&key))
                && held.stamp == stamp
                && held.at.elapsed() < reread
            {
                return Some(take(held));
            }
            parsed.remove(&key);
            let list = Arc::new(read_kept_list(dir, venue)?);
            // A file with no stamp is never held, so its stamp here is never compared.
            let mut read =
                Parsed { stamp: stamp.unwrap_or_default(), at: Instant::now(), list, rows: None };
            let answer = take(&mut read);
            if stamp.is_some() {
                parsed.insert(key, read);
            }
            Some(answer)
        })
        .collect()
}

/// The roster's venues in order, kept to those `only` names (`None`: every venue).
fn roster(only: Option<&[String]>) -> impl Iterator<Item = &'static str> + '_ {
    vike_model::VENUES
        .iter()
        .copied()
        .filter(move |venue| only.is_none_or(|named| named.iter().any(|v| v == venue)))
}

/// **Every kept list under `dir`, in roster order, as a fresh read answers it** (`vike_catalog::read_kept_lists`),
/// through [`PARSED`] ([`parsed_kept`]). `only` keeps the venues it names (no other file is read or copied); `None` is
/// every venue.
fn parsed_kept_lists(
    dir: &Path,
    reread: Duration,
    only: Option<&[String]>,
) -> Vec<VenueInstrumentList> {
    let lists = parsed_kept(dir, reread, roster(only), |held| Arc::clone(&held.list));
    lists.into_iter().map(Arc::unwrap_or_clone).collect()
}

/// **`venue`'s kept list under `dir`, through [`PARSED`]**: what a fresh `vike_catalog::read_kept_list` answers, by
/// [`parsed_kept`]'s rule. For a reader of one venue's rows per ask (`SeriesGaps`, `StrategySymbolFacts`), which read
/// the whole file at every call before.
pub(crate) fn parsed_kept_list(dir: &Path, venue: &str) -> Option<Arc<VenueInstrumentList>> {
    let mut one =
        parsed_kept(dir, vike_catalog::KEPT_LIST_REREAD, std::iter::once(venue), |held| {
            Arc::clone(&held.list)
        });
    one.pop()
}

/// **Fold one attempt's outcome into the kept list** at `now_ms`: `None` when nothing changes (a routine refusal),
/// else the list to write. See the module doc for the rule.
#[must_use]
pub fn fold(
    venue: &str,
    kept: Option<VenueInstrumentList>,
    outcome: &Result<CatalogOutcome, String>,
    now_ms: i64,
) -> Option<VenueInstrumentList> {
    let mut list = kept.unwrap_or_else(|| empty_list(venue));
    list.last_try_ms = Some(now_ms);
    match outcome {
        Ok(CatalogOutcome::Listed { instruments, .. })
            if instruments.is_empty() && !list.instruments.is_empty() =>
        {
            list.last_error = Some(format!(
                "the venue answered with nothing; the {} instruments already kept were kept",
                list.instruments.len()
            ));
        }
        Ok(CatalogOutcome::Listed { instruments, truncated, .. }) => {
            list.instruments.clone_from(instruments);
            list.truncated = *truncated;
            list.refreshed_ms = Some(now_ms);
            list.last_error = None;
        }
        Ok(CatalogOutcome::NotArmed | CatalogOutcome::Refused(_)) => return None,
        Err(why) => list.last_error = Some(why.clone()),
    }
    Some(list)
}

/// A list with nothing in it and no attempt behind it.
fn empty_list(venue: &str) -> VenueInstrumentList {
    VenueInstrumentList {
        venue: venue.to_string(),
        refreshed_ms: None,
        truncated: false,
        last_error: None,
        last_try_ms: None,
        instruments: Vec::new(),
        added_by_operator: Vec::new(),
    }
}

/// **A kept list split into the venue's own listing and the operator's rows it carried** (the rows named in
/// `added_by_operator`), so a fold judges and replaces the listing alone and never a row added by hand.
#[must_use]
pub fn split_operator_rows(
    kept: Option<VenueInstrumentList>,
) -> (Option<VenueInstrumentList>, Vec<Instrument>) {
    let Some(mut list) = kept else { return (None, Vec::new()) };
    let added = std::mem::take(&mut list.added_by_operator);
    let (carried, listing) =
        list.instruments.into_iter().partition(|i| added.contains(&i.raw_symbol));
    list.instruments = listing;
    (Some(list), carried)
}

/// **The venue's own listing with the operator's rows folded in** — the one rule: a row added by hand joins the
/// kept list only where the listing has no row of its `raw_symbol` (the venue's own row wins, and the hand row is
/// kept for when the listing drops it), and the joined rows are named in `added_by_operator`. A venue with no
/// listing and no hand row keeps no list (`None`).
///
/// ⚠ A listing row with no price step (zero, or absent and read as zero: a list kept before the venue stated steps)
/// is not a fact, so it never beats a hand row that states one: the hand row takes its place. That listing row is
/// then gone from the kept file until the next listing brings it back (a refresh), and a listing that states a step
/// wins again.
///
/// ⚠ **A hand row NAMES a listing row the grammar cannot name** ([`lent_naming`]): the venue's row stays, with every
/// fact it states, its own base included, and takes the name spelled from the hand row's base. It is not an added row,
/// so it stays in the listing a split gives back. Every fold first takes back every name a hand row lent
/// ([`is_lent`]) and then lends again from the rows it is given, so the operator's rows are the only source of such a
/// name: a hand row added again under another base renames the venue's row at once, and a removed one leaves it
/// unnamed at once (the owner's "fix" of 2026-11-20: one `add`, no `rm`, refresh and `add`). A store that will not read
/// (`operator_rows`'s `None`) keeps the rows the file carried, the names they lend included.
#[must_use]
pub fn with_operator_rows(
    listing: Option<VenueInstrumentList>,
    venue: &str,
    rows: &[Instrument],
) -> Option<VenueInstrumentList> {
    let rows: Vec<&Instrument> = rows.iter().filter(|r| r.venue == venue).collect();
    if listing.is_none() && rows.is_empty() {
        return None;
    }
    let mut list = listing.unwrap_or_else(|| empty_list(venue));
    for listed in list.instruments.iter_mut().filter(|i| is_lent(i)) {
        listed.naming = Some(vike_catalog::spell_naming_of_row(listed));
    }
    let states_step =
        |i: &Instrument| i.properties.tick_size.is_finite() && i.properties.tick_size > 0.0;
    for row in rows {
        if states_step(row) {
            list.instruments.retain(|i| i.raw_symbol != row.raw_symbol || states_step(i));
        }
        match list.instruments.iter_mut().find(|i| i.raw_symbol == row.raw_symbol) {
            Some(listed) => {
                if let Some(naming) = lent_naming(listed, row) {
                    listed.naming = Some(naming);
                }
            }
            None => {
                list.added_by_operator.push(row.raw_symbol.clone());
                list.instruments.push(row.clone());
            }
        }
    }
    Some(list)
}

/// **The name a hand row lends the venue's own row of its `raw_symbol`, or `None`** (decision 0290's O-6; the owner's
/// "SO ADD IT" of 2026-11-18). A CFD on an index, a commodity, a bond or anything else is named from the base its row
/// states, and a venue whose base is no word gives its row no name (dukascopy's `USA500.IDX/USD`, base `USA500.IDX`);
/// the operator's row of it, `vike-cli data instruments add --venue dukascopy USA500.IDX/USD --tick 0.001 --quote USD
/// --asset-class index --point-scale 1000 --base SPX500`, then names it `SPX500.CFD`. Only when both rows are such CFDs, the
/// venue's row has no name of its own (none, or one a hand row lent: [`is_lent`]) and the hand row spells one: a venue
/// row that has a name of its own keeps it, and no other class is named by hand. The `.bi5` dataset and the wire name
/// read `raw_symbol`, never the base, and the venue's row keeps its own base, so all three stay the venue's.
#[must_use]
pub(crate) fn lent_naming(listed: &Instrument, row: &Instrument) -> Option<InstrumentNaming> {
    let naming_of =
        |i: &Instrument| i.naming.clone().unwrap_or_else(|| vike_catalog::spell_naming_of_row(i));
    if !named_by_its_base(listed)
        || !named_by_its_base(row)
        || !(matches!(naming_of(listed), InstrumentNaming::Unnamed(_)) || is_lent(listed))
    {
        return None;
    }
    match naming_of(row) {
        naming @ InstrumentNaming::User(_) => Some(naming),
        InstrumentNaming::Unnamed(_) => None,
    }
}

/// Whether the grammar names `i` by the base its row states (decision 0290's O-6): a CFD on an index, a commodity, a
/// bond or anything else.
fn named_by_its_base(i: &Instrument) -> bool {
    matches!(&i.contract, Contract::Cfd(cfd) if matches!(
        cfd.underlying_kind,
        ContractCfdUnderlying::Index
            | ContractCfdUnderlying::Commodity
            | ContractCfdUnderlying::Bond
            | ContractCfdUnderlying::Other
    ))
}

/// **Whether the user name `listed` stores is one a hand row LENT it** ([`lent_naming`]): a CFD named by its base whose
/// own base names nothing (dukascopy's `VOL.IDX`) and which stores a user name all the same, on a venue whose catalog
/// does not name its own rows ([`NAMED_BY_THEIR_CATALOG`]: IG's stated names are its own). Only a fold of an operator's
/// row writes such a name, so [`with_operator_rows`] takes it back before it lends again.
pub(crate) fn is_lent(listed: &Instrument) -> bool {
    !NAMED_BY_THEIR_CATALOG.contains(&listed.venue.as_str())
        && named_by_its_base(listed)
        && matches!(listed.naming, Some(InstrumentNaming::User(_)))
        && matches!(vike_catalog::spell_naming_of_row(listed), InstrumentNaming::Unnamed(_))
}

/// **The operator's rows of `venue`**, read from the settings database, or `None` when they cannot be known (no
/// store, or one that will not read: logged): the caller then keeps the rows the file carried, never drops them.
fn operator_rows(settings_dir: Option<&Path>, venue: &str) -> Option<Vec<Instrument>> {
    match vike_secrets::operator_instrument::read_operator_instruments(settings_dir?) {
        Ok(rows) => {
            Some(rows.into_iter().map(|r| r.instrument).filter(|i| i.venue == venue).collect())
        }
        Err(e) => {
            tracing::warn!(venue, error = %e, "the operator's own instruments could not be read; the kept ones stay");
            None
        }
    }
}

/// What an older kept file is kept as beside the one this daemon names (`<venue>.json.before-names`): the file as the
/// older build wrote it, byte for byte, so a person can put it back. No reader reads it.
pub const OLDER_FILE_SUFFIX: &str = ".before-names";

/// **The venues whose kept rows only their own catalog names** (decision 0290 item 6: IG names itself,
/// `crates/bridges/ig/src/catalog.rs`'s `naming_of`, which this daemon cannot run: it links no bridge). The shared
/// grammar would misname their rows (an IG standard lot `CS.D.EURUSD.CFD.IP` states no base and quote, a share epic's
/// ticker is its third word), so [`name_older_kept_lists`] leaves such a file to the refresh after the start, which
/// reaches the venue's catalog through the gatewayhub; until then its symbols stay refused in words.
const NAMED_BY_THEIR_CATALOG: &[&str] = &["ig"];

/// **Name, ONCE, every kept list an older build wrote** (decision 0290; the owner's ruling «Б» of 2026-11-16 and his
/// "make it smart" of 2026-11-17): at this daemon's start, BEFORE it listens, so every hub that waits for that listen
/// (the tradehub: the entrypoint starts it once the gatewayhub AND this daemon listen) reads names. A venue's file
/// with a row that stores no naming is copied aside first (`<venue>.json` + [`OLDER_FILE_SUFFIX`]; a copy that fails
/// leaves the file as it is), then folded with the settings database's operator rows as a refresh folds them (a row
/// the operator deleted leaves it; a store that will not read keeps the rows the file carried) and written ONCE
/// through [`keep`], which names each row by the refresh's own naming (`vike_catalog::spelled`). A file whose every row
/// stores its naming is only read: nothing is spelled at a later start. A venue in [`NAMED_BY_THEIR_CATALOG`] is left
/// to its refresh. The answer is the venues it named; a failure is logged and leaves that file as it was.
pub fn name_older_kept_lists(dir: &Path, settings_dir: Option<&Path>) -> Vec<&'static str> {
    let _one_at_a_time = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let mut named = Vec::new();
    for venue in vike_model::VENUES {
        let Some(on_disk) = read_kept_list(dir, venue) else { continue };
        if on_disk.instruments.iter().all(|row| row.naming.is_some()) {
            continue;
        }
        if NAMED_BY_THEIR_CATALOG.contains(venue) {
            tracing::warn!(
                venue,
                "the kept list an older build wrote stores no names, and only the venue's own \
                catalog names its rows: `vike-cli data catalog refresh --venue {venue}` once the hubs are up names it"
            );
            continue;
        }
        let file = kept_file(dir, venue);
        let mut older = file.clone().into_os_string();
        older.push(OLDER_FILE_SUFFIX);
        if let Err(e) = std::fs::copy(&file, &older) {
            tracing::error!(venue, error = %e, "the kept list an older build wrote could not be copied aside, so it is \
                left unnamed and its symbols stay refused; refresh it once this daemon listens");
            continue;
        }
        let (listing, carried) = split_operator_rows(Some(on_disk.clone()));
        let rows = operator_rows(settings_dir, venue).unwrap_or(carried);
        match keep(dir, venue, Some(&on_disk), with_operator_rows(listing, venue, &rows), true) {
            Ok(()) => {
                let older = Path::new(&older).display().to_string();
                tracing::info!(venue, rows = on_disk.instruments.len(), %older, "the kept list an older build wrote \
                    is named, once (decision 0290); the older file is kept beside it");
                named.push(*venue);
            }
            Err(why) => {
                tracing::error!(venue, %why, "the kept list an older build wrote was not named")
            }
        }
    }
    named
}

/// Write `list` when it differs from `on_disk` (or `always`), the error as the answer's words. Its rows are named
/// first (decision 0290, the owner's ruling «Б» of 2026-11-16): each row that stores no naming yet (the venue's fresh
/// listing, an operator's row, a row of a file written before names were stored) is spelled once here
/// (`vike_catalog::spelled`), so the list compared with `on_disk` is the one [`write_list`] writes, and no reader
/// spells it again. An old file is therefore named by any refresh asked for it, a refused one included, its listing
/// untouched. A list two of whose user names would be stored under one series ([`store_collision`]) is not written at
/// all: the old file stands and the answer says which rows.
fn keep(
    dir: &Path,
    venue: &str,
    on_disk: Option<&VenueInstrumentList>,
    list: Option<VenueInstrumentList>,
    always: bool,
) -> Result<(), String> {
    let Some(mut list) = list else { return Ok(()) };
    list.instruments = vike_catalog::spelled(list.instruments);
    if let Some(why) = store_collision(venue, &list.instruments) {
        tracing::warn!(venue, %why, "instrument list refused; the kept file is unchanged");
        return Err(why);
    }
    if !always && on_disk == Some(&list) {
        return Ok(());
    }
    write_list(dir, &list).map_err(|e| {
        tracing::warn!(venue, error = %e, "instrument list could not be written; the kept file is unchanged");
        format!("the list of `{venue}` could not be kept: {e}")
    })
}

/// **Two different user names of `venue` that one store path would hold** (the owner's ruling «Б», 2026-11-16): the
/// store files a series under `vike_paths::store_path::store_name_of_user_name` of its user name, so `XYZ:TSLAUSDC.P`
/// and `XYZ-TSLAUSDC.P` would write one series as two instruments. `Some` is the refusal naming both rows and both
/// user names. One user name on two rows is not this (it is one store path for one name, and a lookup refuses the
/// name as naming two rows).
fn store_collision(venue: &str, rows: &[Instrument]) -> Option<String> {
    let mut seen: std::collections::HashMap<String, (&str, &str)> =
        std::collections::HashMap::new();
    for row in rows.iter().filter(|row| row.venue == venue) {
        let Some(vike_model::InstrumentNaming::User(user)) = &row.naming else { continue };
        let store = vike_paths::store_path::store_name_of_user_name(user);
        match seen.get(&store) {
            Some((first_raw, first_user)) if *first_user != user.as_str() => {
                return Some(format!(
                    "the list of `{venue}` was not kept: `{first_raw}` (user name `{first_user}`) and `{}` (user name \
                     `{user}`) would both be stored as `{store}` (the store writes a user name's `:` as `-`, decision \
                     0290), so one series would hold two instruments; the kept list stands as it was",
                    row.raw_symbol
                ));
            }
            Some(_) => {}
            None => {
                seen.insert(store, (row.raw_symbol.as_str(), user.as_str()));
            }
        }
    }
    None
}

/// **Refresh one venue now**: fetch through the lane's gates, fold the venue's own listing, fold the operator's rows
/// back in ([`with_operator_rows`]: a refresh never erases a row added by hand), write. The answer is what the attempt
/// did, with an empty answer over a list reported as the error it kept the list for.
pub fn refresh_venue(
    dir: &Path,
    settings_dir: Option<&Path>,
    lane: Option<&CatalogLane>,
    venue: &str,
    now_ms: i64,
) -> VenueRefresh {
    let _one_at_a_time = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let outcome = crate::server::catalog_outcome(venue, lane);
    let mut answer =
        VenueRefresh { venue: venue.to_string(), outcome: outcome.clone(), splits: None };
    let on_disk = read_kept_list(dir, venue);
    let (listing, carried) = split_operator_rows(on_disk.clone());
    let folded = fold(venue, listing.clone(), &outcome, now_ms);
    if let (Ok(CatalogOutcome::Listed { .. }), Some(Some(kept))) =
        (&outcome, folded.as_ref().map(|l| &l.last_error))
    {
        answer.outcome = Err(kept.clone());
    }
    let changed = folded.is_some();
    let rows = operator_rows(settings_dir, venue).unwrap_or(carried);
    let merged = with_operator_rows(folded.or(listing), venue, &rows);
    if let Err(why) = keep(dir, venue, on_disk.as_ref(), merged, changed) {
        answer.outcome = Err(why);
    }
    answer
}

/// **Fold the operator's rows of `venue` into its kept list now** (`Request::OperatorInstruments`, after a row was
/// added or removed): the venue's own listing as it is kept, asked nothing. A list that would not change is not
/// written.
///
/// # Errors
/// The operator's rows could not be read (the file is left as it is), or the file could not be written.
pub(crate) fn keep_operator_rows(
    dir: &Path,
    settings_dir: &Path,
    venue: &str,
) -> Result<(), String> {
    let _one_at_a_time = REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
    let rows = operator_rows(Some(settings_dir), venue).ok_or_else(|| {
        format!("the operator's own instruments could not be read, so the list of `{venue}` was left as it is")
    })?;
    let on_disk = read_kept_list(dir, venue);
    let (listing, _) = split_operator_rows(on_disk.clone());
    keep(dir, venue, on_disk.as_ref(), with_operator_rows(listing, venue, &rows), false)
}

/// `Request::InstrumentLists`: the kept lists, the operator's own lists and the operator's share splits
/// (`vike_secrets::stock_split`, read at each ask, so a split `vike-cli data splits add` wrote reaches the next backtest
/// with no restart). No `dir` (a datahub with no project) keeps no list and refuses in words, never an empty answer that
/// reads as "every venue keeps an empty list" (it does not advertise the verb either: `crate::server`'s
/// `served_features`); no settings store answers none of the other two. `venues` is
/// `Request::InstrumentListsOfVenues`: the same answer with only those venues' parts (`InstrumentListsReply::of_venues`),
/// and no other venue's kept list is copied.
pub(crate) fn instrument_lists_verb(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
    venues: Option<&[String]>,
) -> Response {
    let Some(dir) = dir else {
        return Response::Error(NO_PROJECT.to_string());
    };
    match settings_rows(settings) {
        Ok(reply) => Response::InstrumentLists(with_kept_lists(reply, dir, venues)),
        Err(why) => Response::Error(why),
    }
}

/// [`instrument_lists_verb`]'s answer as its plain msgpack (`rmp_serde`'s named encoding, the route's; `Err` is a reply
/// to write as a value): each venue's ROWS written ONCE per parse of its file ([`Parsed`]'s `rows`, [`kept_rows_verb`]'s
/// bytes), its list written around them with none, the operator's lists and the share splits written at each ask, as
/// they are read at each ask. Byte for byte what msgpack writes of [`instrument_lists_verb`]'s value: that value written
/// with no kept list, its empty `venues` replaced by the lists, each list written with no row and its empty
/// `instruments` replaced by its rows. A value or a list that does not write, or one without that empty array, is
/// answered as the value.
pub(crate) fn instrument_lists_msgpack(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
    venues: Option<&[String]>,
) -> Result<Vec<u8>, Box<Response>> {
    let Some(dir) = dir else {
        return Err(Box::new(Response::Error(NO_PROJECT.to_string())));
    };
    let reply = settings_rows(settings).map_err(|why| Box::new(Response::Error(why)))?;
    let lists = parsed_kept(dir, vike_catalog::KEPT_LIST_REREAD, roster(venues), |held| {
        if held.rows.is_none() {
            held.rows = encoded_rows(&held.list.instruments).ok().map(Arc::new);
        }
        let list = &held.list;
        // Every field but the rows, so the rows are never copied to be written.
        let rowless = VenueInstrumentList {
            venue: list.venue.clone(),
            refreshed_ms: list.refreshed_ms,
            truncated: list.truncated,
            last_error: list.last_error.clone(),
            last_try_ms: list.last_try_ms,
            instruments: Vec::new(),
            added_by_operator: list.added_by_operator.clone(),
        };
        let rows = held.rows.clone()?;
        splice_empty_array(&msgpack_of(&rowless)?, INSTRUMENTS_KEY, &rows.msgpack)
    });
    let response = Response::InstrumentLists(match venues {
        Some(named) => reply.of_venues(named),
        None => reply,
    });
    let spliced = lists.into_iter().collect::<Option<Vec<_>>>().and_then(|lists| {
        let mut array = msgpack_array_head(lists.len());
        lists.iter().for_each(|list| array.extend_from_slice(list));
        splice_empty_array(&msgpack_of(&response)?, VENUES_KEY, &array)
    });
    spliced.ok_or_else(|| Box::new(as_value(response, dir, venues)))
}

/// A list's `instruments` and a reply's `venues` as `rmp_serde` names them: a fixstr head (`0xa0` and the length), the
/// name. Each is followed by `0x90`, the empty array, in the value written with none.
const INSTRUMENTS_KEY: &[u8] = b"\xabinstruments";
const VENUES_KEY: &[u8] = b"\xa6venues";

/// `value`'s plain msgpack: its reply frame less its four reserved bytes and its magic.
fn msgpack_of(value: &impl serde::Serialize) -> Option<Vec<u8>> {
    use vike_hub_protocol::frame::{BodyCodec, encode_reply};
    encode_reply(BodyCodec::Msgpack, value).ok().map(|frame| frame[8..].to_vec())
}

/// `msgpack` with the empty array after its first `key` replaced by `array` (one whole msgpack array). `None` when the
/// key is not followed by an empty array. The pair cannot sit inside a string: `0x90` after an ASCII letter is no UTF-8.
fn splice_empty_array(msgpack: &[u8], key: &[u8], array: &[u8]) -> Option<Vec<u8>> {
    let at = msgpack
        .windows(key.len() + 1)
        .position(|w| w[..key.len()] == *key && w[key.len()] == 0x90)?;
    let value = at + key.len();
    Some([&msgpack[..value], array, &msgpack[value + 1..]].concat())
}

/// The head of a msgpack array of `n` elements, as `rmp` writes it (the shortest form).
fn msgpack_array_head(n: usize) -> Vec<u8> {
    match n {
        0..=15 => vec![0x90 | n as u8],
        16..=0xffff => [&[0xdc][..], &(n as u16).to_be_bytes()].concat(),
        _ => [&[0xdd][..], &(n as u32).to_be_bytes()].concat(),
    }
}

/// [`instrument_lists_msgpack`]'s `response`, which holds no kept list, with them: [`instrument_lists_verb`]'s value.
fn as_value(response: Response, dir: &Path, venues: Option<&[String]>) -> Response {
    match response {
        Response::InstrumentLists(reply) => {
            Response::InstrumentLists(with_kept_lists(reply, dir, venues))
        }
        other => other,
    }
}

/// `reply` with `dir`'s kept lists, kept to `venues` when it names them (`InstrumentListsReply::of_venues`).
fn with_kept_lists(
    mut reply: InstrumentListsReply,
    dir: &Path,
    venues: Option<&[String]>,
) -> InstrumentListsReply {
    reply.venues = parsed_kept_lists(dir, vike_catalog::KEPT_LIST_REREAD, venues);
    match venues {
        Some(named) => reply.of_venues(named),
        None => reply,
    }
}

/// The two parts of the answer the settings database holds, read at each ask: the share splits, then the operator's
/// own lists, in a reply with no kept list. No store answers none of either; one that will not read is the refusal, in
/// words.
fn settings_rows(
    settings: Option<&vike_secrets::CredentialStore>,
) -> Result<InstrumentListsReply, String> {
    let splits = stock_splits(settings)?;
    let operator = match settings.and_then(vike_secrets::CredentialStore::location) {
        None => Vec::new(),
        Some(at) => vike_secrets::operator_instrument_list::read_operator_instrument_lists(
            at.settings_dir(),
        )
        .map_err(|e| format!("the operator's own lists could not be read: {e}"))?
        .into_iter()
        .map(|r| vike_catalog::InstrumentList {
            venue: r.venue,
            fetched: r.fetched,
            qualifier: r.qualifier,
            instruments: r.instruments,
        })
        .collect(),
    };
    Ok(InstrumentListsReply { venues: Vec::new(), operator, splits })
}

/// The operator's share splits (`vike_secrets::stock_split`), read now; none with no settings store.
fn stock_splits(
    settings: Option<&vike_secrets::CredentialStore>,
) -> Result<Vec<vike_model::StockSplit>, String> {
    let Some(at) = settings.and_then(vike_secrets::CredentialStore::location) else {
        return Ok(Vec::new());
    };
    vike_secrets::stock_split::read_stock_splits(at.settings_dir())
        .map_err(|e| format!("the operator's share splits could not be read: {e}"))
}

/// `Request::KeptRowsOfVenues` (the owner's "yes both" of 2026-11-19): for each asked venue, once and in the order
/// asked, its STAMP and, unless `known` holds that stamp, the rows and splits [`instrument_lists_verb`] answers it (its
/// kept list's instruments, its share splits) as msgpack, in one binary body
/// (`vike_datahub_client::proto::encode_kept_rows`). Refused in [`instrument_lists_verb`]'s words.
///
/// ⚠ **The stamp is DERIVED from the bytes it would answer**: the hex SHA-256 of the rows' own SHA-256 and the
/// splits' msgpack. So it moves with every writer and no writer has to move it: a refresh, the operator's rows and the
/// start's naming all write the file through [`write_list`], which drops the parsed list; a hand edit is read again by
/// [`parsed_kept`]'s rules (at the next ask, or within `vike_catalog::KEPT_LIST_REREAD`); a split, whoever writes it
/// (`vike-cli data splits`, a refresh's Yahoo pass), is read at each ask. Equal bytes answer equal stamps, across a
/// restart too, so a reader's held rows are exactly what a fresh answer would carry. A list's rows are encoded and
/// hashed once per parse of its file, beside it in [`PARSED`], the bytes [`instrument_lists_msgpack`] answers too; each
/// ask encodes and hashes only its venues' splits.
pub(crate) fn kept_rows_verb(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
    venues: &[String],
    known: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    use vike_datahub_client::proto::{KeptRowsOfVenue, encode_kept_rows, kept_msgpack};
    let dir = dir.ok_or_else(|| NO_PROJECT.to_string())?;
    let splits = stock_splits(settings)?;
    let held = parsed_kept(dir, vike_catalog::KEPT_LIST_REREAD, roster(Some(venues)), |held| {
        if held.rows.is_none() {
            held.rows = encoded_rows(&held.list.instruments).ok().map(Arc::new);
        }
        (held.list.venue.clone(), held.rows.clone())
    });
    let none = Arc::new(encoded_rows(&[])?);
    let mut answer: Vec<KeptRowsOfVenue> = Vec::with_capacity(venues.len());
    for venue in venues {
        if answer.iter().any(|a| a.venue == *venue) {
            continue;
        }
        // shortcut: a file whose `venue` names another venue (only a hand edit writes one) is answered under the venue
        // it names, the first such file alone, where the JSON verb keeps every one; a hand edit that does it is wrong
        // anyway.
        let rows = match held.iter().find(|(of, _)| of == venue) {
            Some((_, Some(rows))) => Arc::clone(rows),
            Some((_, None)) => {
                return Err(format!("the kept rows of `{venue}` could not be encoded"));
            }
            None => Arc::clone(&none),
        };
        let of_venue: Vec<&vike_model::StockSplit> =
            splits.iter().filter(|s| s.venue == *venue).collect();
        let split_bytes = kept_msgpack(&of_venue)?;
        let stamp = vike_hub_protocol::hash::sha256_hex(
            &[rows.digest.as_bytes(), split_bytes.as_slice()].concat(),
        );
        let changed = (known.get(venue) != Some(&stamp))
            .then(|| vike_data::EncodedKept { rows: rows.msgpack.clone(), splits: split_bytes });
        answer.push(KeptRowsOfVenue { venue: venue.clone(), stamp, changed });
    }
    Ok(encode_kept_rows(&answer))
}

/// What both verbs answer a datahub with no project folder.
const NO_PROJECT: &str = "this datahub resolved no project folder, so it keeps no instrument lists";

/// `Request::RefreshInstrumentLists`: one venue, or every venue the lane serves (`None`).
pub(crate) fn refresh_instrument_lists_verb(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
    lane: Option<&CatalogLane>,
    venue: Option<&str>,
) -> Response {
    let Some(dir) = dir else {
        return Response::Error(NO_PROJECT.to_string());
    };
    let venues: Vec<String> = match (venue, lane) {
        (Some(v), _) => vec![v.to_string()],
        (None, Some(lane)) => lane.table().supported().iter().map(|v| (*v).to_string()).collect(),
        (None, None) => Vec::new(),
    };
    let now_ms = vike_model::now_ms();
    let settings_dir = settings.and_then(vike_secrets::CredentialStore::location);
    let settings_dir = settings_dir.as_ref().map(|at| at.settings_dir());
    Response::InstrumentListsRefreshed(
        venues.iter().map(|v| refresh_venue(dir, settings_dir, lane, v, now_ms)).collect(),
    )
}

#[path = "instrument_lists_tests.rs"]
#[cfg(test)]
mod instrument_lists_tests;
