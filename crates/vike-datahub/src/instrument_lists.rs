//! **The venues' KEPT instrument lists**: one `<venue>.json` per venue under `<project>/instruments`
//! (`vike_paths::state_path::PROJECT_INSTRUMENTS_DIR`), each a `vike_catalog::VenueInstrumentList`.
//! This daemon is their ONLY writer (the owner's ruling of 2026-11-12; `docs/decisions/0062`'s amendment):
//!
//! * **the weekly refresh** — [`spawn_ticker`] looks every [`CATALOG_REFRESH_TICK`] and refreshes a venue whose list
//!   is [`CATALOG_REFRESH_PERIOD_MS`] old or older ([`is_due`]);
//! * **refresh now** — `Request::RefreshInstrumentLists`, one venue or every venue the catalog lane serves.
//!
//! Both fetch through `crate::server::catalog_outcome`, the `VenueCatalog` verb's own gates, bucket and memo, and so
//! through the gatewayhub (decision 0164: this daemon opens no venue connection). Clients read the lists with
//! `Request::InstrumentLists` ([`instrument_lists_verb`]), which also carries the operator's own credentialed lists from
//! the settings database.
//!
//! # The fold, and why it never loses a list
//!
//! [`fold`] is `merge_refresh`'s rule on the server: a listing REPLACES the kept list; a failure, or an EMPTY answer
//! over a list that held instruments, KEEPS it and records why in `last_error`; a routine refusal (`NotArmed`, a
//! venue the lane does not serve) changes nothing at all. Each file is written whole through
//! `vike_utility::atomic_file::write_atomic` (a temp file beside it, then a rename that also replaces an existing file on
//! Windows), and refreshes are serialized by one lock, so the ticker and a press never interleave a read and a write.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_catalog::{VenueInstrumentList, kept_file, read_kept_list, read_kept_lists};
use vike_datahub_client::catalog::{CatalogOutcome, InstrumentListsReply, VenueRefresh};
use vike_datahub_client::proto::Response;
use vike_model::Instrument;

use crate::catalog::CatalogLane;

/// **How old a kept list may get before the weekly refresh fetches it again: seven days**, the default and the only
/// value (no settings row, by the owner's ruling). A venue whose list has never been fetched is due at once.
pub const CATALOG_REFRESH_PERIOD_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// How often the ticker looks for a due venue. A failed refresh is therefore retried an hour later, under the lane's
/// own bucket.
pub const CATALOG_REFRESH_TICK: Duration = Duration::from_secs(60 * 60);

/// One refresh at a time in this process, so a read-fold-write of one file never interleaves with another's.
static REFRESHING: Mutex<()> = Mutex::new(());

/// **Write one venue's list atomically**: the directory is created on first use, then a temp file and a rename.
pub fn write_list(dir: &Path, list: &VenueInstrumentList) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let bytes = serde_json::to_vec(list).map_err(std::io::Error::other)?;
    vike_utility::atomic_file::write_atomic(&kept_file(dir, &list.venue), &bytes)
}

/// **Is `list` due a refresh at `now_ms`?** Never fetched (no list, or no success yet) is due; otherwise due once it is
/// [`CATALOG_REFRESH_PERIOD_MS`] old, and not a millisecond before.
#[must_use]
pub fn is_due(now_ms: i64, list: Option<&VenueInstrumentList>) -> bool {
    match list.and_then(|l| l.refreshed_ms) {
        None => true,
        Some(at) => now_ms.saturating_sub(at) >= CATALOG_REFRESH_PERIOD_MS,
    }
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
    for row in rows {
        if !list.instruments.iter().any(|i| i.raw_symbol == row.raw_symbol) {
            list.added_by_operator.push(row.raw_symbol.clone());
            list.instruments.push(row.clone());
        }
    }
    Some(list)
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

/// Write `list` when it differs from `on_disk` (or `always`), the error as the answer's words.
fn keep(
    dir: &Path,
    venue: &str,
    on_disk: Option<&VenueInstrumentList>,
    list: Option<VenueInstrumentList>,
    always: bool,
) -> Result<(), String> {
    let Some(list) = list else { return Ok(()) };
    if !always && on_disk == Some(&list) {
        return Ok(());
    }
    write_list(dir, &list).map_err(|e| {
        tracing::warn!(venue, error = %e, "instrument list could not be written; the kept file is unchanged");
        format!("the list of `{venue}` could not be kept: {e}")
    })
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
    let mut answer = VenueRefresh { venue: venue.to_string(), outcome: outcome.clone() };
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

/// **The weekly refresh's one pass at `now_ms`**: every venue the lane serves whose kept list [`is_due`].
pub fn refresh_due(
    dir: &Path,
    settings_dir: Option<&Path>,
    lane: &CatalogLane,
    now_ms: i64,
) -> Vec<VenueRefresh> {
    lane.table()
        .supported()
        .into_iter()
        .filter(|venue| is_due(now_ms, read_kept_list(dir, venue).as_ref()))
        .map(|venue| refresh_venue(dir, settings_dir, Some(lane), venue, now_ms))
        .collect()
}

/// **Start the weekly refresh**: one thread that runs [`refresh_due`] at once and then every [`CATALOG_REFRESH_TICK`],
/// for the life of the process. `settings_dir` is where the operator's own rows are read, folded back into each list.
pub fn spawn_ticker(dir: PathBuf, settings_dir: Option<PathBuf>, lane: Arc<CatalogLane>) {
    let spawned = std::thread::Builder::new().name("dh-instrument-lists".into()).spawn(move || {
        loop {
            for done in refresh_due(&dir, settings_dir.as_deref(), &lane, vike_model::now_ms()) {
                match done.outcome {
                    Ok(_) => tracing::info!(venue = %done.venue, "instrument list refreshed"),
                    Err(why) => tracing::warn!(venue = %done.venue, %why, "instrument list kept"),
                }
            }
            std::thread::sleep(CATALOG_REFRESH_TICK);
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "the weekly instrument-list refresh could not start");
    }
}

/// `Request::InstrumentLists`: the kept lists and the operator's own. No `dir` (a datahub with no project) answers
/// none of the first; no settings store, none of the second.
pub(crate) fn instrument_lists_verb(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
) -> Response {
    let operator = match settings.and_then(vike_secrets::CredentialStore::location) {
        None => Vec::new(),
        Some(at) => match vike_secrets::operator_instrument_list::read_operator_instrument_lists(
            at.settings_dir(),
        ) {
            Ok(rows) => rows
                .into_iter()
                .map(|r| vike_catalog::InstrumentList {
                    venue: r.venue,
                    fetched: r.fetched,
                    qualifier: r.qualifier,
                    instruments: r.instruments,
                })
                .collect(),
            Err(e) => {
                return Response::Error(format!("the operator's own lists could not be read: {e}"));
            }
        },
    };
    let venues = dir.map(read_kept_lists).unwrap_or_default();
    Response::InstrumentLists(InstrumentListsReply { venues, operator })
}

/// `Request::RefreshInstrumentLists`: one venue, or every venue the lane serves (`None`).
pub(crate) fn refresh_instrument_lists_verb(
    dir: Option<&Path>,
    settings: Option<&vike_secrets::CredentialStore>,
    lane: Option<&CatalogLane>,
    venue: Option<&str>,
) -> Response {
    let Some(dir) = dir else {
        return Response::Error(
            "this datahub resolved no project folder, so it keeps no instrument lists".to_string(),
        );
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
