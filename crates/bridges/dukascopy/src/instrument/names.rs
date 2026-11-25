//! `names` — **dukascopy's USER name of an instrument and its WIRE name, read off the KEPT row (decision 0290), and the
//! ONE Rust copy of the spelling the JForex sidecar answers in (audit flag DUK-7).**
//!
//! The engine, the journal, the fills, the reconcile reports, the price stream's keys and every door speak the USER name
//! (decision 0290's grammar): a pair `EURUSD`, a metal `XAUUSD`, a share CFD `AAPL.CFD`. The venue's own spelling (the
//! WIRE: `EURUSD`, `XAUUSD`, `AAPL.US/USD`) exists only inside this bridge, and it is read off the name's KEPT row (the
//! datahub's `<project>/instruments/dukascopy.json` as this process reads it, `vike_catalog::kept_list`; the datahub
//! writes it from the list the sidecar's login states, `crate::catalog`). No list, no row, or two rows answering one name
//! is a refusal in words ([`wire_symbol_in`]): a NAME is never read for an instrument (the owner's ruling of 2026-11-13).
//!
//! **The sidecar's spelling.** The sidecar takes a currency or metal pair as six letters or as `AAA/BBB`, and any other
//! instrument as the SDK's slashed name (`OrderMap.instrument`), but it ANSWERS (a fill, a `position` line, a tick, a
//! `terms` line, a recon row) in ONE spelling, `OrderMap.canonicalSymbol`: the SDK's name with its slash dropped exactly
//! when it is two three-letter codes (`EUR/USD` is `EURUSD`, `AAPL.US/USD` stays). [`sidecar_spelling`] is that rule's
//! one Rust copy, held to the Java by `names_tests.rs`'s `the_rust_rule_is_the_java_rule` (it reads `OrderMap.java`).
//! Every name this bridge sends is a kept row's wire in that spelling ([`wire_of_row`]), so the name an answer comes back
//! under is the name the command went out under, whichever way the row spells it (`EURUSD` as the sidecar lists it, or
//! `EUR/USD` as an operator may write it).
//!
//! **The way back**, a sidecar line's name to the user name a fill, a position, a reconcile row and a tick are booked
//! under, is [`user_symbol`]: the kept row whose wire it is. Every pair [`wire_symbol`] answered in this process is
//! remembered, so the names this process sent map back without a list read. A name no kept row names (an instrument
//! opened in the JForex platform that the list does not carry) is kept under the venue's own name with a WARN
//! ([`user_symbol_or_wire`]): never dropped, never guessed.
//!
//! **History is not translated here**: the `.bi5` datafeed is asked by its dataset name (`vike_catalog::bi5_dataset_name`,
//! `crate::instrument::grid::history_point_value`), which the datahub sends as it is.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_bridge_core::symbol_properties::SymbolPropertiesError;
use vike_model::Instrument;

/// The venue key of the kept list.
const VENUE: &str = "dukascopy";

/// **The spelling the sidecar answers in for the name `name`** (`OrderMap.canonicalSymbol`, the ONE rule): `EUR/USD` is
/// `EURUSD`, `XAU/USD` is `XAUUSD`, and any other name is itself (`EURUSD`, `AAPL.US/USD`, `DOGE/USD`, `COPPER.CMD/USD`).
#[must_use]
pub fn sidecar_spelling(name: &str) -> String {
    let code = |part: &str| part.len() == 3 && part.bytes().all(|b| b.is_ascii_uppercase());
    match name.split_once('/') {
        Some((base, quote)) if code(base) && code(quote) => format!("{base}{quote}"),
        _ => name.to_string(),
    }
}

/// **The wire name of a KEPT row as the sidecar is sent it and answers it**: [`sidecar_spelling`] of
/// `vike_catalog::wire_name_of_row`.
#[must_use]
pub fn wire_of_row(row: &Instrument) -> String {
    sidecar_spelling(&vike_catalog::wire_name_of_row(row))
}

/// **The wire name of the USER name `user` among `rows`** (the venue's kept list; `&[]` is no list), with the row's
/// canonical user name: `(wire, user)`.
///
/// # Errors
/// No row of the venue in `rows` (`vike_catalog::no_kept_list_refusal` names the fix), none answers `user`, or two do:
/// the translator's words, naming `user`.
pub fn wire_symbol_in(rows: &[Instrument], user: &str) -> Result<(String, String), String> {
    let row = vike_catalog::kept_row_of_user_name(rows, VENUE, user)
        .map_err(|why| format!("dukascopy: {why}"))?;
    let canonical =
        vike_catalog::user_name_of_row(row).map_err(|why| format!("dukascopy: {why}"))?;
    Ok((wire_of_row(row), canonical))
}

/// **The USER name of the sidecar's name `wire` among `rows`**: the one row of the venue whose [`wire_of_row`] it is.
/// `None` for no such row, or two (an operator's `EUR/USD` beside the venue's `EURUSD`): no answer is a guess.
#[must_use]
pub fn user_symbol_in(rows: &[Instrument], wire: &str) -> Option<String> {
    let mut named = rows.iter().filter(|row| row.venue == VENUE && wire_of_row(row) == wire);
    let row = named.next()?;
    if named.next().is_some() {
        return None;
    }
    vike_catalog::user_name_of_row(row).ok()
}

/// Every `wire -> user` pair answered in this process.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that `wire` is `user`.
fn remember(wire: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(wire.to_string(), user.to_string());
}

/// The rows of the venue's kept list as this process reads it (empty: no list).
fn kept_rows() -> std::sync::Arc<Vec<Instrument>> {
    vike_catalog::kept_list(VENUE).unwrap_or_default()
}

/// **The wire name the sidecar is sent for the USER name `user`** ([`wire_symbol_in`] over this process's kept list),
/// remembered for the way back. What an order, a protection sent again and a price subscription carry.
///
/// # Errors
/// [`wire_symbol_in`]'s.
pub fn wire_symbol(user: &str) -> Result<String, String> {
    let (wire, canonical) = wire_symbol_in(&kept_rows(), user)?;
    remember(&wire, &canonical);
    Ok(wire)
}

/// **[`wire_symbol`] as the facts door words it** (the gatewayhub's dukascopy facts, `crate::ledger::quoted_facts`'s
/// caller): no list kept yet is "not now" (`SymbolPropertiesError::ListUnavailable`, which the node asks again once the
/// sidecar's login had its list kept), and a name the kept list carries no row of, or two, is a verdict on the name.
///
/// # Errors
/// As above.
pub fn facts_wire_symbol(user: &str) -> Result<String, SymbolPropertiesError> {
    let rows = kept_rows();
    if !rows.iter().any(|row| row.venue == VENUE) {
        return Err(SymbolPropertiesError::ListUnavailable(
            format!("`{}`: {}", user.trim(), vike_catalog::no_kept_list_refusal(VENUE)).into(),
        ));
    }
    let (wire, canonical) =
        wire_symbol_in(&rows, user).map_err(|why| SymbolPropertiesError::Refused(why.into()))?;
    remember(&wire, &canonical);
    Ok(wire)
}

/// **The USER name of the sidecar's name `wire`**: one [`wire_symbol`] already answered, else the kept row whose wire
/// it is ([`user_symbol_in`]). `None` when no kept row is that instrument (no list is held, or the venue lists it and the
/// kept list does not): the caller says what it does then.
#[must_use]
pub fn user_symbol(wire: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(wire) {
        return Some(user.clone());
    }
    let user = user_symbol_in(&kept_rows(), wire)?;
    remember(wire, &user);
    Some(user)
}

/// **The name a sidecar line is booked under** (a fill, a `position` line, a recon row, a tick): [`user_symbol`] of
/// `wire`, else the venue's own `wire` name, said at WARN: such an instrument is no name this process sent (every one
/// is remembered), so it is an order opened in the JForex platform on something the kept list does not carry.
#[must_use]
pub fn user_symbol_or_wire(wire: &str) -> String {
    user_symbol(wire).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            wire,
            "a sidecar line names an instrument no kept row names (the datahub's <project>/instruments/dukascopy.json); \
             it is booked under the venue's own name"
        );
        wire.to_string()
    })
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;
