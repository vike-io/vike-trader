//! The symbols a reconcile client reads BEYOND the one it was built on.
//!
//! An account's engine is admitted symbols on demand (decision 0173): the engine holds the mounted
//! symbol plus up to [`MAX_EXTRA_SYMBOLS`] admitted ones, and a restart restores positions and resting
//! orders for all of them. The core's reconcile driver, though, asks a [`super::ReconClient`] three
//! questions with no symbol in them, and the restore planner reads an order the report does not carry
//! as GONE. So a client that read only the mounted symbol made every resting order on an admitted one
//! invisible, then retired it. [`super::ReconClient::admit_symbol`] is how the client hears which
//! symbols the engine admitted, and this is the list it keeps.
//!
//! * **Append-only and bounded.** A symbol stays for the life of the client (the engine never
//!   un-admits one) and the list is capped at the engine's own [`MAX_EXTRA_SYMBOLS`], so a pass costs
//!   at most that many extra reads per report kind, and the client never reads a symbol the engine
//!   could not hold.
//! * **A symbol is added when the admission is REAL, never on a look.** The caller tells the client
//!   after `Command::AdmitSymbol` was sent, not when the node merely asked for the symbol's facts (a
//!   Preview, the Telegram look, an order the door then refused would otherwise fill the sixteen slots
//!   with symbols the engine never held, and a real 17th admission would go unread). There is no
//!   eviction: nothing is added for a symbol that was not admitted, so nothing needs removing.
//! * **The mounted symbol is not in it**: the client always reads that one, with the request it has
//!   always made.
//! * **Spelling is the caller's** (the engine's: binance and aster `.P` for the perpetual lane, bybit
//!   the bare symbol); this list only upper-cases, de-duplicates and bounds.

use std::sync::{Mutex, PoisonError};

use crate::MAX_EXTRA_SYMBOLS;

/// See the module doc.
#[derive(Debug, Default)]
pub struct AdmittedSymbols {
    symbols: Mutex<Vec<String>>,
}

impl AdmittedSymbols {
    /// Add `symbol`, unless it is empty, is the `mounted` symbol, is already held (any case) or the
    /// list is full. `true` = it was added. Both spellings are compared and stored upper-case.
    pub fn admit(&self, symbol: &str, mounted: &str) -> bool {
        let symbol = symbol.to_ascii_uppercase();
        if symbol.is_empty() || symbol.eq_ignore_ascii_case(mounted) {
            return false;
        }
        let mut held = self.symbols.lock().unwrap_or_else(PoisonError::into_inner);
        if held.len() >= MAX_EXTRA_SYMBOLS || held.contains(&symbol) {
            return false;
        }
        held.push(symbol);
        true
    }

    /// The admitted symbols, in admission order.
    pub fn symbols(&self) -> Vec<String> {
        self.symbols.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// **What a pass may claim its client read**: the symbols [`super::ReconClient::admitted_symbols`]
/// listed BEFORE the fetch and still lists AFTER it, in the first answer's order. One told only after
/// the fetch began was not in it, and one forgotten meanwhile (the daemon behind the client was
/// replaced) may not have been: claiming either lets the restore planner read the venue's silence as
/// "gone" for a symbol nobody fetched. Both `None` (a client with no list) is `None`; one answer
/// `None` and the other not is read as an empty list.
pub fn admitted_throughout(
    before: Option<Vec<String>>,
    after: Option<Vec<String>>,
) -> Option<Vec<String>> {
    if before.is_none() && after.is_none() {
        return None;
    }
    let after = after.unwrap_or_default();
    Some(before.unwrap_or_default().into_iter().filter(|s| after.contains(s)).collect())
}

#[cfg(test)]
#[path = "admitted_tests.rs"]
mod admitted_tests;
