//! The symbol each order was SUBMITTED under, so its fills carry it.
//!
//! The order path spells `EURUSD` as `EUR_USD` (`vike_catalog::to_oanda_instrument`) and every OANDA
//! transaction names the instrument that way. `vike_exec::ExecutionEngine` compares symbols by exact
//! string and DROPS a bare `Fill` whose symbol it neither mounts nor registered the order under, with no
//! log and no counter: the order's lifecycle wrap advanced and the position never moved. So a fill of an
//! order this process submitted is stamped with the symbol of the ORDER, whichever spelling the
//! submitter used (`EURUSD` and `EUR_USD` both book, each under its own name). The same shape as
//! `crates/bridges/vike-ibkr/src/exec/event_mapper.rs`'s `register_symbol`.
//!
//! One table, shared by the exec thread (records at the submit), the transactions-stream thread and the
//! confirm worker (both read it). A fill of a coid nobody recorded (another client or the web platform
//! on the same account, an order placed before a restart that the host did not teach through
//! `ExecutionClient::adopt_restored`) keeps the venue's own spelling: there is nothing to relabel it by.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vike_model::events::Event;

/// The most orders [`SubmittedSymbols`] holds. Entries leave with their order ([`SubmittedSymbols::settle`]),
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
                "oanda: submitted-symbol table full - this order's fills will carry the venue's \
                 spelling of the instrument, which an engine mounted on another spelling drops"
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

    /// Drop what is kept for every order `events` ends (filled, cancelled, rejected). Called where the
    /// events are published, AFTER they were built: a duplicate of a fill (the stream carries a market
    /// order's fill the order POST already returned) then falls back to the venue's spelling, which the
    /// engine drops, and its `trade_id` dedup would have dropped it anyway.
    pub(crate) fn settle(&self, events: &[Event]) {
        let mut table = self.table();
        for ev in events {
            let coid = match ev {
                Event::OrderFilled(e) => &e.client_order_id,
                Event::OrderCanceled(e) => &e.client_order_id,
                Event::OrderRejected(e) => &e.client_order_id,
                _ => continue,
            };
            table.remove(coid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::OrderCanceled;

    fn canceled(coid: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: String::new().into(),
            ts: 0,
        })
    }

    #[test]
    fn a_recorded_coid_keeps_its_symbol_and_an_unknown_one_the_venue_spelling() {
        let s = SubmittedSymbols::default();
        s.record("c1", "EURUSD");
        s.record("c2", "EUR_USD");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD");
        assert_eq!(s.label("c2", "EUR_USD"), "EUR_USD");
        assert_eq!(s.label("foreign", "EUR_USD"), "EUR_USD");
        assert_eq!(s.label("", "EUR_USD"), "EUR_USD");
    }

    #[test]
    fn a_clone_shares_the_table() {
        let s = SubmittedSymbols::default();
        s.clone().record("c1", "EURUSD");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD");
    }

    /// Only an order that ended leaves; a full table is never cleared to make room.
    #[test]
    fn the_table_is_bounded_and_only_a_finished_order_leaves_it() {
        let s = SubmittedSymbols::with_capacity(2);
        s.record("c1", "EURUSD");
        s.record("c2", "GBPUSD");
        s.record("c3", "USDJPY");
        assert_eq!(s.label("c3", "USD_JPY"), "USD_JPY", "past the cap: not recorded");
        assert_eq!(s.label("c1", "EUR_USD"), "EURUSD", "and nothing was evicted for it");
        s.record("c1", "EURUSD"); // re-recording a known coid is not growth
        s.settle(&[canceled("c1"), canceled("never-recorded")]);
        assert_eq!(s.label("c1", "EUR_USD"), "EUR_USD", "the cancelled order left");
        assert_eq!(s.label("c2", "GBP_USD"), "GBPUSD", "a live order stayed");
        s.record("c3", "USDJPY");
        assert_eq!(s.label("c3", "USD_JPY"), "USDJPY", "the freed slot took the new order");
    }
}
