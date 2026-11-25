//! `names` — **a USER name's broker name (the WIRE) and back, read off the instrument's KEPT row** (decision 0290).
//!
//! The engine, the journal, the fills, the reconcile reports, the feeds' series keys and every door speak the USER
//! name: a pair `EURUSD`, a metal `XAUUSD`, an index CFD `US500.CFD`, a coin CFD `BTCUSD.CFD`. The broker's own name of
//! the instrument (`EURUSD`, or `EURUSDm` on a broker that suffixes its symbols; its case is the broker's) exists only
//! inside this bridge, read off the instrument's KEPT row (`<project>/instruments/ctrader.json`, the account's own
//! listing as the datahub keeps it: `vike_catalog::kept_list`) by `vike_catalog::wire_name_of_row`, and the wire speaks
//! that name's `symbolId` (`crate::instrument::symbols::SymbolMap::id_of`). No list, no row, or two rows is a refusal
//! in words ([`names_in`], the translator's own, which name the fix when no list is kept): a name is never read as the
//! broker's.
//!
//! The way back ([`user_name`]): a `symbolId`'s broker name (`SymbolMap::name_of`) to the user name of the kept row
//! whose wire name it is. Every pair [`wire_name`] answered in this process is remembered, so a name this process
//! signed maps back without a list read. A broker name no kept row names (a position the operator opened in the
//! platform on a symbol the list does not carry yet) is booked under the broker's own name, said once at WARN
//! ([`user_name_or_wire`]): never dropped, never guessed.
//!
//! The handshake, the catalog's listing, the account reads and the candle history never ask here: ctrader's list is
//! written by the account's own running session (the adapter's start and the gatewayhub's symbol facts hand it to the
//! held list the datahub keeps), so the bridge starts and lists with no kept list. A reconcile pass reads each name
//! through [`names_in`] too: a name with no row fails the pass in words, never read as the broker's.
//!
//! Each lookup has a pure core over `rows` (a test passes `&[]` for "no list" without touching process-wide state)
//! and a thin wrapper over `vike_catalog::kept_list`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_model::{Instrument, InstrumentNames};

use crate::refusals::CtraderNameRefusal;

const VENUE: &str = "ctrader";

/// Every broker name [`wire_name`] or [`user_name`] answered in this process, to its user name.
fn answered() -> &'static Mutex<HashMap<String, String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that the broker's `wire` is `user`.
fn remember(wire: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(wire.to_string(), user.to_string());
}

/// **The three names of the USER name `user` among `rows`** (the venue's kept list), the pure core of [`wire_name`]:
/// the one ctrader row whose user name it is (ignoring ASCII case), its canonical user name and its broker name.
///
/// # Errors
/// No ctrader row in `rows` (no list is kept: the words name `vike_catalog::no_kept_list_refusal`'s fix), none
/// answering `user`, or two: each in the translator's words, naming `user`.
pub fn names_in(rows: &[Instrument], user: &str) -> Result<InstrumentNames, CtraderNameRefusal> {
    vike_catalog::names_of_user_name(rows, VENUE, user)
        .map_err(|why| CtraderNameRefusal::Untranslated { why })
}

/// **The user name of the broker's `wire` among `rows`**, the pure core of [`user_name`]: the user name of the ctrader
/// row whose wire name is `wire`, exactly (the broker's names are case-significant), or `None` when no row is.
#[must_use]
pub fn user_name_in(rows: &[Instrument], wire: &str) -> Option<String> {
    rows.iter()
        .filter(|row| row.venue == VENUE)
        .find(|row| vike_catalog::wire_name_of_row(row) == wire)
        .and_then(|row| vike_catalog::user_name_of_row(row).ok())
}

/// **The broker's name of the USER name `user`**, read off its kept row ([`names_in`] over this process's kept list):
/// what an order, an amend and a feed subscription resolve their `symbolId` by. The pair is remembered for the way
/// back.
///
/// # Errors
/// [`names_in`]'s.
pub fn wire_name(user: &str) -> Result<String, CtraderNameRefusal> {
    let rows = vike_catalog::kept_list(VENUE);
    let names = names_in(rows.as_deref().map_or(&[][..], Vec::as_slice), user)?;
    remember(&names.wire, &names.user);
    Ok(names.wire)
}

/// **The USER name of the broker's `wire`**: one [`wire_name`] already answered, else the kept row whose wire name it
/// is ([`user_name_in`]). `None` when no kept row is that instrument: the caller says what it does then.
#[must_use]
pub fn user_name(wire: &str) -> Option<String> {
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(wire) {
        return Some(user.clone());
    }
    let rows = vike_catalog::kept_list(VENUE)?;
    let user = user_name_in(&rows, wire)?;
    remember(wire, &user);
    Some(user)
}

/// **The name a venue row is booked under** (a fill, a funding settlement, a quote, a reconcile report): its
/// [`user_name`], or the broker's own `wire` name when no kept row names it, said once per name at WARN. Such an
/// instrument is no name this process signed ([`wire_name`] remembered every one), so its row is an operator's hand
/// trade on something the list does not carry yet, or the account's list is not kept yet.
#[must_use]
pub fn user_name_or_wire(wire: &str) -> String {
    user_name(wire).unwrap_or_else(|| {
        let first = warned().lock().unwrap_or_else(PoisonError::into_inner).insert(wire.to_string());
        if first {
            tracing::warn!(
                venue = VENUE,
                wire,
                fix = %vike_catalog::no_kept_list_refusal(VENUE),
                "a venue row names an instrument no kept row names; it is booked under the broker's own name"
            );
        }
        wire.to_string()
    })
}

/// The broker names [`user_name_or_wire`] has said its WARN for.
fn warned() -> &'static Mutex<HashSet<String>> {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WARNED.get_or_init(Mutex::default)
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod names_tests;
