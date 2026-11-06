//! The symbol each order was SUBMITTED under, so its fills carry it.
//!
//! The order path slashes a bare crypto pair (`BTCUSD` -> `BTC/USD`, `event_mapper::to_alpaca_symbol`) and
//! every trade update names the symbol that way. `vike_exec::ExecutionEngine` compares symbols by exact
//! string and DROPS a bare `Fill` whose symbol it neither mounts nor registered the order under, with no
//! log and no counter: the order's lifecycle wrap advanced and the position never moved. So a fill of an
//! order this process submitted is stamped with the symbol of the ORDER, whichever spelling the submitter
//! used (`BTCUSD` and `BTC/USD` both book, each under its own name). The same shape as
//! `crates/bridges/vike-ibkr/src/exec/event_mapper.rs`'s `register_symbol`.
//!
//! One table, shared by the exec thread (records at the submit) and the SSE thread (reads it). A fill of a
//! coid nobody recorded (another client on the same account, an order placed before a restart that the
//! host did not teach through `ExecutionClient::adopt_restored`) keeps the venue's own spelling.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The most orders [`SubmittedSymbols`] holds. Entries leave with their order ([`SubmittedSymbols::forget`]),
/// so this is a leak guard: past it a new order is not recorded and its fills carry the venue's spelling
/// (loudly). Nothing is evicted to make room, because a live order's entry is the only thing that books
/// its fill.
const MAX_RECORDED: usize = 4096;

/// coid -> the symbol the order was submitted under. Cloning shares the one table.
#[derive(Clone)]
pub struct SubmittedSymbols {
    table: Arc<Mutex<HashMap<String, String>>>,
    cap: usize,
}

impl Default for SubmittedSymbols {
    fn default() -> Self {
        Self::with_capacity(MAX_RECORDED)
    }
}

impl SubmittedSymbols {
    fn with_capacity(cap: usize) -> Self {
        Self { table: Arc::default(), cap }
    }

    fn table(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Remember that `coid` was submitted (or, for a restored order, taught) under `symbol`.
    pub(crate) fn record(&self, coid: &str, symbol: &str) {
        if coid.is_empty() || symbol.is_empty() {
            return;
        }
        let mut table = self.table();
        if table.len() >= self.cap && !table.contains_key(coid) {
            tracing::warn!(
                coid,
                cap = self.cap,
                "alpaca: submitted-symbol table full - this order's fills will carry the venue's \
                 spelling of the symbol, which an engine mounted on another spelling drops"
            );
            return;
        }
        table.insert(coid.to_string(), symbol.to_string());
    }

    /// The symbol a fill of `coid` carries: the one the order was submitted under, else the venue's own
    /// spelling `venue_symbol` (an order nobody recorded).
    pub(crate) fn label(&self, coid: &str, venue_symbol: &str) -> String {
        self.table().get(coid).map_or_else(|| venue_symbol.to_string(), Clone::clone)
    }

    /// Drop what is kept for `coid`: its order ended (the final `fill`, a cancel, an expiry or a
    /// rejection). A `partial_fill` does NOT end it: the pieces that follow still need the symbol.
    pub(crate) fn forget(&self, coid: &str) {
        self.table().remove(coid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_coid_keeps_its_symbol_and_an_unknown_one_the_venue_spelling() {
        let s = SubmittedSymbols::default();
        s.record("c1", "BTCUSD");
        s.record("c2", "BTC/USD");
        assert_eq!(s.label("c1", "BTC/USD"), "BTCUSD");
        assert_eq!(s.label("c2", "BTC/USD"), "BTC/USD");
        assert_eq!(s.label("foreign", "BTC/USD"), "BTC/USD");
        assert_eq!(s.label("", "BTC/USD"), "BTC/USD");
    }

    #[test]
    fn a_clone_shares_the_table() {
        let s = SubmittedSymbols::default();
        s.clone().record("c1", "BTCUSD");
        assert_eq!(s.label("c1", "BTC/USD"), "BTCUSD");
    }

    /// Only an order that ended leaves; a full table is never cleared to make room.
    #[test]
    fn the_table_is_bounded_and_only_a_finished_order_leaves_it() {
        let s = SubmittedSymbols::with_capacity(2);
        s.record("c1", "BTCUSD");
        s.record("c2", "ETHUSD");
        s.record("c3", "SOLUSD");
        assert_eq!(s.label("c3", "SOL/USD"), "SOL/USD", "past the cap: not recorded");
        assert_eq!(s.label("c1", "BTC/USD"), "BTCUSD", "and nothing was evicted for it");
        s.record("c1", "BTCUSD"); // re-recording a known coid is not growth
        s.forget("c1");
        s.forget("never-recorded");
        assert_eq!(s.label("c1", "BTC/USD"), "BTC/USD", "the ended order left");
        assert_eq!(s.label("c2", "ETH/USD"), "ETHUSD", "a live order stayed");
        s.record("c3", "SOLUSD");
        assert_eq!(s.label("c3", "SOL/USD"), "SOLUSD", "the freed slot took the new order");
    }
}
