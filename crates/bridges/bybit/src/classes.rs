//! `classes` — **which V5 book an ENGINE symbol of this venue trades in** (decision 0231), and the one spelling
//! that tells the books apart where the venue's own name cannot.
//!
//! The venue's `category` is per ORDER (`linear`, `inverse`, `spot`, `option`), the engine's symbol is one
//! string, and the same string means two books for exactly one pair of classes: a SPOT pair and a LINEAR
//! perpetual are both `BTCUSDT` (290 such names measured on 2026-09-16, and the engine's bare symbol has always
//! been the PERPETUAL). Every other class has a name of its own: a dated linear future is `ETHUSDT-27NOV26`, an
//! inverse perpetual `BTCUSD`, a dated inverse future `BTCUSDZ26`, an option `BTC-31OCT26-75500-C-USDT`. So:
//!
//! * a bare name is a DERIVATIVE, as it always was (the venue's `linear ∩ inverse` is empty, so the name says
//!   which of the two derivative books it is);
//! * a SPOT pair is written `BASEQUOTE.S` ([`SPOT_SUFFIX`]): the engine's symbol, the order's symbol, the fill's
//!   symbol (`crate::event_mapper` stamps it from the row's `category`) and the reconcile read's symbol. It is
//!   NOT the data plane's bare spot name (`DOGEUSDT`, `crates/bridges/bybit/src/data.rs`), which is another
//!   instrument on the exec side: the suffix is the exec plane's only spelling of a second book.
//!
//! The category of a symbol is the VENUE's word wherever a grid is fetched anyway (`contractType` of the
//! `instruments-info` row, [`category_of_contract_type`]); [`category_from_name`] is the no-network reading a
//! cancel or a status query takes when no row was fetched (a client built after a restart), and it answers
//! `None` rather than guess.

use crate::data::Category;

/// The suffix of a SPOT symbol on the exec plane. `DOGEUSDT.S` is the spot pair, `DOGEUSDT` the perpetual.
pub const SPOT_SUFFIX: &str = ".S";

/// `(venue symbol, is spot)` of an engine symbol: `DOGEUSDT.S` is `("DOGEUSDT", true)`.
#[must_use]
pub fn split_spot(symbol: &str) -> (&str, bool) {
    match symbol.strip_suffix(SPOT_SUFFIX) {
        Some(bare) => (bare, true),
        None => (symbol, false),
    }
}

/// The engine's symbol for a row of `category` the venue named `venue_symbol`: the venue's name, plus
/// [`SPOT_SUFFIX`] for a spot pair. The one place a user-data row's `category` becomes a spelling.
#[must_use]
pub fn engine_symbol(category: Option<&str>, venue_symbol: &str) -> String {
    if category == Some("spot") && !venue_symbol.is_empty() {
        format!("{venue_symbol}{SPOT_SUFFIX}")
    } else {
        venue_symbol.to_string()
    }
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

/// The book an engine symbol names, read off the NAME alone (no network): the answer a client built without a
/// grid fetch (a cancel, a status query) takes. `None` is "cannot tell from the name": the caller asks the venue's
/// listings ([`crate::instruments::perp_book_for`]) or refuses, and never defaults to linear.
///
/// * `X.S` is spot. A dated linear future (`ETHUSDT-27NOV26`) and every name ending `USDT`, `USDC` or `PERP` is
///   LINEAR: the three shapes of every one of the venue's 923 linear symbols on 2026-10-30, and the ones every
///   symbol this plane traded before decision 0231 had.
/// * A name ending `USD`, or `USD` + a month letter + two digits (`BTCUSD`, `BTCUSDZ26`), is INVERSE. The two
///   shapes are disjoint from the linear ones, and a name this misreads would be refused by the venue as an
///   unknown symbol of the wrong category, never filled.
/// * An option (`BTC-31OCT26-75500-C-USDT`: more than one hyphen) is `None`: this plane signs none.
#[must_use]
pub fn category_from_name(symbol: &str) -> Option<Category> {
    let (name, spot) = split_spot(symbol);
    if spot {
        return (!name.is_empty()).then_some(Category::Spot);
    }
    if name.matches('-').count() > 1 {
        return None;
    }
    if name.contains('-') {
        return Some(Category::Linear);
    }
    if ["USDT", "USDC", "PERP"].iter().any(|q| name.ends_with(q)) {
        return Some(Category::Linear);
    }
    let b = name.as_bytes();
    let dated_inverse = b.len() > 6
        && name[..b.len() - 3].ends_with("USD")
        && b[b.len() - 3].is_ascii_uppercase()
        && b[b.len() - 2..].iter().all(u8::is_ascii_digit);
    (name.ends_with("USD") || dated_inverse).then_some(Category::Inverse)
}

#[cfg(test)]
#[path = "classes_tests.rs"]
mod classes_tests;
