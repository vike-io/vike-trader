//! `class` — **the venue's own `(symbol, category)` of a USER name, and the user name of a venue row** (decisions
//! 0231 and 0290).
//!
//! The engine, the journal, the fills, the reconcile reports and every door speak the USER name (decision 0290's
//! grammar): a spot pair `BTCUSDT`, a linear perpetual `BTCUSDT.P`, a USDC one `BTCUSDC.P` (the venue's `BTCPERP`), a
//! coin-margined one `BTCUSD.P`, a dated contract `ETHUSDT_261106` (`ETHUSDT-06NOV26`) or `BTCUSD_261225`
//! (`BTCUSDZ26`). The venue's `symbol` and `category` exist only inside this bridge, and both are read off the name's
//! KEPT row (the datahub's `<project>/instruments/bybit.json` as this process reads it, `vike_catalog::kept_list`):
//! its wire name (`vike_catalog::names_of_row`) and its book (`crate::history::Category::of_row`). No list, no row,
//! or two rows answering one name is a refusal in words ([`venue_symbol`]): a NAME is never read for a book (the
//! owner's ruling of 2026-11-13).
//!
//! The way back, a venue row's `(category, symbol)` to the user name a fill, a reconcile report and a replay carry,
//! is [`user_symbol`]: the kept row whose book and wire name they are. Every name [`venue_symbol`] answered in this
//! process is remembered, so the names this process signs map back without a list read, even while the list cannot
//! be read.
//!
//! The category of an instrument this plane fetched a grid for is the VENUE's word on it (`contractType` of the
//! `instruments-info` row, [`category_of_contract_type`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use crate::history::Category;
use crate::refusals::BybitSymbolRefusal;

const VENUE: &str = "bybit";

/// Every `(book, wire)` [`venue_symbol`] and [`user_symbol`] answered in this process, to its user name.
fn answered() -> &'static Mutex<HashMap<(Category, String), String>> {
    static ANSWERED: OnceLock<Mutex<HashMap<(Category, String), String>>> = OnceLock::new();
    ANSWERED.get_or_init(Mutex::default)
}

/// Remember that `(book, wire)` is `user`.
fn remember(book: Category, wire: &str, user: &str) {
    answered()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((book, wire.to_string()), user.to_string());
}

/// **The venue's `(symbol, book)` of a USER name**, read off its KEPT row (decision 0290): `BTCUSDT.P` is
/// `("BTCUSDT", Linear)`, `BTCUSDT` `("BTCUSDT", Spot)`, `BTCUSDC.P` `("BTCPERP", Linear)`, `BTCUSD.P`
/// `("BTCUSD", Inverse)`, `ETHUSDT_261106` `("ETHUSDT-06NOV26", Linear)`. What every order, cancel, status query,
/// account read and socket of a name signs or dials.
///
/// # Errors
/// No bybit list is kept in this process, none of its rows is `user`, two are, or the row is of no book this bridge
/// signs or streams (an option): each in words naming `user`.
pub fn venue_symbol(user: &str) -> Result<(String, Category), String> {
    let rows = vike_catalog::kept_list(VENUE);
    let rows = rows.as_deref().map_or(&[][..], Vec::as_slice);
    let row = vike_catalog::kept_row_of_user_name(rows, VENUE, user)
        .map_err(|why| BybitSymbolRefusal::NoKeptRow { why }.to_string())?;
    let book = Category::of_row(row).ok_or_else(|| {
        BybitSymbolRefusal::NoBook { user: user.to_string(), wire: row.raw_symbol.clone() }
            .to_string()
    })?;
    let names = vike_catalog::names_of_row(row)?;
    remember(book, &names.wire, &names.user);
    Ok((names.wire, book))
}

/// **The USER name of a venue row**: the one the KEPT row of `book` named `wire` by the venue spells (decision 0290),
/// or one [`venue_symbol`] already answered. `None` when no kept row is that instrument (no list is held, or the
/// venue listed it after the list was refreshed): the caller says what it does then.
#[must_use]
pub fn user_symbol(book: Category, wire: &str) -> Option<String> {
    let key = (book, wire.to_string());
    if let Some(user) = answered().lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return Some(user.clone());
    }
    let rows = vike_catalog::kept_list(VENUE)?;
    let user = rows
        .iter()
        .filter(|row| row.venue == VENUE && Category::of_row(row) == Some(book))
        .find(|row| vike_catalog::wire_name_of_row(row) == wire)
        .and_then(|row| vike_catalog::user_name_of_row(row).ok())?;
    remember(book, wire, &user);
    Some(user)
}

/// **The name a venue row is booked under** (a fill, an order row, a funding settlement of the account's socket or
/// log): [`user_symbol`] of `wire` in `book`. With no book (a row that states no `category`, as the frozen r6 rows
/// do) or no kept row naming it, the venue's own `wire` name, the second said at WARN: such an instrument is no name
/// this process signed ([`venue_symbol`] remembered every one), so its row is an operator's hand trade on something
/// the list does not carry yet.
#[must_use]
pub fn user_symbol_or_wire(book: Option<Category>, wire: &str) -> String {
    let Some(book) = book else { return wire.to_string() };
    user_symbol(book, wire).unwrap_or_else(|| {
        tracing::warn!(
            venue = VENUE,
            wire,
            category = book.wire(),
            "a venue row names an instrument no kept row names (the datahub's <project>/instruments/bybit.json); it \
             is booked under the venue's own name"
        );
        wire.to_string()
    })
}

/// The derivative book the venue's own `contractType` word names, or `None` for a word that is not a derivative
/// this plane signs (an option, a spot row that carries none, a word the venue added).
#[must_use]
pub fn category_of_contract_type(word: &str) -> Option<Category> {
    match word {
        "LinearPerpetual" | "LinearFutures" => Some(Category::Linear),
        "InversePerpetual" | "InverseFutures" => Some(Category::Inverse),
        _ => None,
    }
}

#[cfg(test)]
#[path = "class_tests.rs"]
mod class_tests;
