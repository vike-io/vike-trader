//! `epic` — **the IG epic of a USER name, and the user name of an epic** (decision 0290).
//!
//! The engine, the journal, the fills, the reconcile reports, the feeds' series keys and every door speak the USER
//! name (`EURUSD`; an index CFD the name its kept row states, `DE40.CFD`). IG addresses a market by its EPIC
//! (`CS.D.EURUSD.MINI.IP`), and one pair has several (the MINI, the standard CFD, a daily spread bet), so an epic is
//! never read off a name: it is the KEPT row's (`vike_catalog::kept_list`; IG's list is the operator's own rows,
//! `crate::operator_row`, and nothing here lists IG's markets), its wire name (`vike_catalog::wire_name_of_row`),
//! held to the one epic-shape rule (`crate::execution::lifecycle`'s `non_epic_refusal`, over
//! `vike_catalog::is_ig_epic`). No list, no row, or two rows answering one name is a refusal in words ([`epic_of`]):
//! the order, the feed subscription and the facts lookup that needed the epic refuse, and nothing is guessed.
//!
//! The way back, the epic a venue row names (a deal confirm, a streamed position, a working order of the operator's
//! read) to the user name it is booked under, is [`user_name_of`]: the kept row whose epic it is. Every pair
//! [`epic_of`] answered in this process is remembered, so what this process signed maps back without a list read.
//! An epic no kept row names is booked under the epic itself, said at WARN ([`user_name_or_epic`]).
//!
//! History is NOT translated here: the datahub hands the history lane the kept row's epic itself
//! (`crate::history::klines`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_model::{Instrument, InstrumentNames};

use crate::execution::lifecycle::non_epic_refusal;
use crate::reconcile::VENUE;

/// Every epic [`epic_of`] and [`user_name_of`] answered in this process, to its user name.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that `epic` is `user`.
fn remember(epic: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(epic.to_string(), user.to_string());
}

/// **The three names of the USER name `user` among `rows`** (IG's kept list): its one kept row's user, store and
/// wire names, the wire one being its epic. Pure, so a test can hand it no list (`&[]`).
///
/// # Errors
/// `rows` holds no ig row (no list is kept here: the words name the fix, `vike_catalog::no_kept_list_refusal`), none
/// answers `user`, two do, or the row's own name is not an IG epic: each in words naming `user` or the row.
pub(crate) fn names_in(rows: &[Instrument], user: &str) -> Result<InstrumentNames, String> {
    let names = vike_catalog::names_of_user_name(rows, VENUE, user).map_err(|why| {
        // An epic typed for a name (an old `account.symbol`): a row IG names not (a knock-out) says why.
        let own = rows
            .iter()
            .find(|row| row.venue == VENUE && row.raw_symbol.eq_ignore_ascii_case(user.trim()));
        format!("ig: {}", own.and_then(|row| vike_catalog::names_of_row(row).err()).unwrap_or(why))
    })?;
    match non_epic_refusal(&names.wire) {
        Some(why) => Err(why),
        None => Ok(names),
    }
}

/// **The user name of the epic `epic` among `rows`**: the one kept row whose epic it is, `None` when none is.
pub(crate) fn user_name_in(rows: &[Instrument], epic: &str) -> Option<String> {
    rows.iter()
        .filter(|row| row.venue == VENUE)
        .filter_map(|row| vike_catalog::names_of_row(row).ok())
        .find(|names| names.wire == epic)
        .map(|names| names.user.clone())
}

/// **The epic an order, a subscription or a facts lookup of the USER name `user` addresses**: [`names_in`] over this
/// process's kept ig list, the pair remembered for the way back.
///
/// # Errors
/// [`names_in`]'s.
pub(crate) fn epic_of(user: &str) -> Result<String, String> {
    let rows = vike_catalog::kept_list(VENUE);
    let names = names_in(rows.as_deref().map_or(&[][..], Vec::as_slice), user)?;
    remember(&names.wire, &names.user);
    Ok(names.wire)
}

/// **The USER name of `epic`**: one [`epic_of`] already answered, else the kept row whose epic it is. `None` when no
/// kept row names it (no list is held, or the operator never added that market).
pub(crate) fn user_name_of(epic: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(epic) {
        return Some(user.clone());
    }
    let rows = vike_catalog::kept_list(VENUE)?;
    let user = user_name_in(&rows, epic)?;
    remember(epic, &user);
    Some(user)
}

/// **The name a venue row on `epic` is booked under** (a deal confirm, a streamed position, a working order of the
/// operator's read): [`user_name_of`], else the epic itself, said at WARN: no kept row names it, so it is no name
/// this process signed (each one is remembered), and a guessed user name would book it on another instrument. A row
/// that states no epic stays blank.
pub(crate) fn user_name_or_epic(epic: &str) -> String {
    if epic.is_empty() {
        return String::new();
    }
    user_name_of(epic).unwrap_or_else(|| {
        tracing::warn!(
            target: "vike_ig::epic",
            venue = VENUE,
            epic,
            "a venue row names an epic no kept row names (the operator's rows of <project>/instruments/ig.json); \
             it is booked under the epic itself"
        );
        epic.to_string()
    })
}

/// The ONE ig kept list this crate's unit tests hold (`vike_catalog::hold_kept_list` is process-wide and the tests
/// run in parallel threads, so every test that holds ig's list holds exactly these rows): the four FX Minis of the
/// captured market details (`EURUSD`, `GBPUSD`, `EURGBP`, `USDJPY`) and EUR/USD's captured standard lot
/// (`EURUSD.CFD`), each made by the operator's own row-writing path (`crate::operator_row`'s `describe`), which names
/// it by IG's rule (`crate::catalog`'s `naming_of`).
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn kept_list_for_tests() -> Vec<Instrument> {
    let rows = [
        ("CS.D.EURUSD.MINI.IP", include_str!("../tests/fixtures/markets/CS.D.EURUSD.MINI.IP.json")),
        ("CS.D.GBPUSD.MINI.IP", include_str!("../tests/fixtures/markets/CS.D.GBPUSD.MINI.IP.json")),
        ("CS.D.EURGBP.MINI.IP", include_str!("../tests/fixtures/markets/CS.D.EURGBP.MINI.IP.json")),
        ("CS.D.USDJPY.MINI.IP", include_str!("../tests/fixtures/markets/CS.D.USDJPY.MINI.IP.json")),
        ("CS.D.EURUSD.CFD.IP", include_str!("../tests/fixtures/markets/CS.D.EURUSD.CFD.IP.json")),
    ]
    .into_iter()
    .map(|(epic, body)| {
        let pick = crate::operator_row::Pick {
            epic: epic.to_string(),
            name: String::new(),
            passed_over: Vec::new(),
        };
        let market = serde_json::from_str(body).expect("the capture parses");
        crate::operator_row::describe(&pick, &market).expect("the operator's row")
    })
    .collect();
    vike_catalog::spelled(rows)
}

/// Hold [`kept_list_for_tests`] as this process's ig list: this crate's tests, and through `test-support` the
/// cross-bridge conformance harness (`crates/vike-bridge-core/tests/bridge_conformance/venues.rs`'s `Ig`).
#[cfg(any(test, feature = "test-support"))]
pub fn hold_kept_list_for_tests() {
    vike_catalog::hold_kept_list(VENUE, kept_list_for_tests());
}

#[cfg(test)]
#[path = "epic_tests.rs"]
mod epic_tests;
