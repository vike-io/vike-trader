//! `names` — **an OANDA instrument's USER name and its WIRE name, both read off its KEPT row** (decision 0290).
//!
//! The engine, the journal, the fills, the financing, the reconcile reports and the feeds' series speak the USER name
//! (`EURUSD`, `XAUUSD`, an index CFD `SPX500.CFD`): `vike_catalog::user_name_of_row` of the instrument's kept row. The
//! venue's own name (`EUR_USD`, `XAU_USD`, `SPX500_USD`) is the WIRE, and it exists only inside this bridge:
//! `vike_catalog::wire_name_of_row` of the same row. The row is the KEPT list's (`<project>/instruments/oanda.json` as
//! this process reads it, `vike_catalog::kept_list`), never a name: no list, no row, or two rows answering one name is
//! a refusal in the translator's words ([`wire_name`]), and with no list those words name the fix
//! (`vike_catalog::no_kept_list_refusal`).
//!
//! The way back, a venue instrument (a fill, a position, a financing row, a pending order) to the name it is booked
//! under, is [`user_name`]: the kept row whose wire name it is. Every pair [`wire_name`] answered in this process is
//! remembered, so the names this process sent map back without a list read. A venue instrument no kept row names (a
//! hand trade on something the list does not carry yet) keeps the venue's own name, said at WARN
//! ([`user_name_or_wire`]): never dropped, never guessed.
//!
//! ⚠ OANDA is a HELD-SESSION venue: its list exists only once a running account's own session has read it (the
//! gatewayhub's `hold_listing`, then the datahub keeps it). So nothing the start, the listing or the account reads
//! needs a row; what does (an order, a replace, a feed subscription, a reconcile pass, the symbol-facts key) refuses
//! in words until the list exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use vike_model::Instrument;

const VENUE: &str = "oanda";

/// Every wire name [`wire_name`] and [`user_name`] answered in this process, to its user name.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that the venue's `wire` is the user's `user`.
fn remember(wire: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(wire.to_string(), user.to_string());
}

/// This process's kept oanda rows (`vike_catalog::kept_list`), empty when none is held.
fn kept_rows() -> Arc<Vec<Instrument>> {
    vike_catalog::kept_list(VENUE).unwrap_or_default()
}

/// Whether this process holds no oanda kept list at all (the venue's first account has not been listed yet).
pub(crate) fn no_kept_list() -> bool {
    !kept_rows().iter().any(|row| row.venue == VENUE)
}

/// **The WIRE name of the USER name `user` among `rows`** (the venue's kept list): `EURUSD` is `EUR_USD`, `XAUUSD`
/// `XAU_USD`, `SPX500.CFD` `SPX500_USD`. The core of [`wire_name`], over the rows it is given (a test passes `&[]`);
/// the pair it answers is remembered for the way back.
///
/// # Errors
/// `rows` holds no oanda row, none answers `user`, or two do: the translator's words, naming `user`.
pub(crate) fn wire_name_in(rows: &[Instrument], user: &str) -> Result<String, String> {
    let names = vike_catalog::names_of_user_name(rows, VENUE, user)?;
    remember(&names.wire, &names.user);
    Ok(names.wire)
}

/// **The WIRE name of the USER name `user`**, read off this process's kept row (decision 0290): what every order,
/// replace, position read, reconcile read and price subscription of `user` sends.
///
/// # Errors
/// [`wire_name_in`]'s, over this process's kept list.
pub(crate) fn wire_name(user: &str) -> Result<String, String> {
    wire_name_in(&kept_rows(), user)
}

/// **The USER name of the venue instrument `wire` among `rows`**: the user name of the one row whose wire name it
/// is, `None` when no row is. The pure core of [`user_name`].
pub(crate) fn user_name_in(rows: &[Instrument], wire: &str) -> Option<String> {
    rows.iter()
        .filter(|row| row.venue == VENUE)
        .find(|row| vike_catalog::wire_name_of_row(row) == wire)
        .and_then(|row| vike_catalog::user_name_of_row(row).ok())
}

/// **The USER name of the venue instrument `wire`**: one [`wire_name`] answered in this process, else the kept row's.
/// `None` when no kept row is that instrument: the caller says what it does then.
pub(crate) fn user_name(wire: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(wire) {
        return Some(user.clone());
    }
    let user = user_name_in(&kept_rows(), wire)?;
    remember(wire, &user);
    Some(user)
}

/// **The name a venue row is booked under** (a fill, a financing row, a pending order): [`user_name`] of `wire`, else
/// the venue's own `wire` name, said at WARN: no name this process sent ([`wire_name`] remembered each), so a hand
/// trade on an instrument the kept list does not carry (yet).
pub(crate) fn user_name_or_wire(wire: &str) -> String {
    user_name(wire).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            wire,
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/oanda.json); it \
             is booked under the venue's own name"
        );
        wire.to_string()
    })
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;
