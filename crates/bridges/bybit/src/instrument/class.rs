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
//!   symbol (`crate::execution::event_mapper` stamps it from the row's `category`) and the reconcile read's symbol. It is
//!   NOT the data plane's bare spot name (`DOGEUSDT`, `crates/bridges/bybit/src/history.rs`), which is another
//!   instrument on the exec side: the suffix is the exec plane's only spelling of a second book.
//!
//! The category of a symbol is the VENUE's word wherever a grid is fetched anyway (`contractType` of the
//! `instruments-info` row, [`category_of_contract_type`]); [`Category::of_kept`] (the symbol's row of the KEPT list,
//! `vike_catalog::bybit_kept_row` over `vike_catalog::kept_list`, its `wire.group`) is the no-network reading a cancel
//! or a status query takes when no row was fetched (a client built after a restart), and it answers `None` rather
//! than guess: a NAME is never read for a book (the owner's ruling of 2026-11-13). The row lookup lives in
//! vike-catalog so `vike-cli secrets account set-symbol` checks a name exactly as this bridge reads it: ONE rule.

use crate::history::Category;

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

#[cfg(test)]
#[path = "class_tests.rs"]
mod class_tests;
