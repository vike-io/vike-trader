//! The symbol each order was SUBMITTED under, so its fills carry it.
//!
//! The order path spells `EURUSD` as `EUR_USD` (`vike_catalog::to_oanda_instrument`) and every OANDA
//! transaction names the instrument that way. `vike_execution::Engine` compares symbols by exact
//! string and DROPS a bare `Fill` whose symbol it neither mounts nor registered the order under, with no
//! log and no counter: the order's lifecycle wrap advanced and the position never moved. So a fill of an
//! order this process submitted is stamped with the symbol of the ORDER, whichever spelling the
//! submitter used (`EURUSD` and `EUR_USD` both book, each under its own name). The same shape as
//! `crates/bridges/vike-ibkr/src/execution/event_mapper.rs`'s `register_symbol`.
//!
//! The table is the shared `vike_bridge_core::per_symbol::OrderClientIdSymbols`, bounded at this venue's cap
//! ([`submitted_symbols`]). One table, shared by the exec thread (records at the submit), the
//! transactions-stream thread and the confirm worker (both read it). A fill of an order client id nobody recorded
//! (another client or the web platform on the same account, an order placed before a restart that the
//! host did not teach through `ExecutionClient::adopt_restored`) keeps the venue's own spelling: there is
//! nothing to relabel it by.
//!
//! Whether an order's submit has been answered is not kept here any more: the shared fence
//! (`vike_bridge_core::exec_actor::submit_fence::SubmitFence`, owner ruling B-17) holds the stream's events for an
//! order behind the exec thread's own, and settles this table through its `enter_with` hook as it lets each go.

use std::sync::Arc;

use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_model::events::Event;

/// The most orders the table holds. Entries leave with their order ([`settle`]), so this is a leak guard:
/// past it a new order is not recorded and its fills carry the venue's spelling (loudly). Nothing is
/// evicted to make room, because a live order's entry is the only thing that books its fill.
const MAX_RECORDED: usize = 4096;

/// The order client id -> submitted-symbol table one start shares between its threads (cloning the `Arc` shares it).
pub fn submitted_symbols() -> Arc<OrderClientIdSymbols> {
    Arc::new(OrderClientIdSymbols::with_cap(MAX_RECORDED, table_full))
}

fn table_full(order_client_id: &str, cap: usize) {
    tracing::warn!(
        order_client_id,
        cap,
        "oanda: submitted-symbol table full - this order's fills will carry the venue's \
         spelling of the instrument, which an engine mounted on another spelling drops"
    );
}

/// Drop what is kept for every order `events` ends (filled, cancelled, rejected). Called where the
/// events are published, AFTER they were built: a duplicate of a fill (the stream carries a market
/// order's fill the order POST already returned) then falls back to the venue's spelling, which the
/// engine drops, and its `trade_id` dedup would have dropped it anyway.
pub(crate) fn settle(symbols: &OrderClientIdSymbols, events: &[Event]) {
    for event in events {
        let order_client_id = match event {
            Event::OrderFilled(e) => &e.order_client_id,
            Event::OrderCanceled(e) => &e.order_client_id,
            Event::OrderRejected(e) => &e.order_client_id,
            _ => continue,
        };
        symbols.forget(order_client_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::events::OrderCanceled;

    fn canceled(order_client_id: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: String::new().into(),
            timestamp: 0,
        })
    }

    /// Only an order that ended leaves (the table's own cap and lookups are bridge-core's rows).
    #[test]
    fn settle_drops_only_the_orders_the_events_end() {
        let s = submitted_symbols();
        s.record("c1", "EURUSD");
        s.record("c2", "GBPUSD");
        settle(&s, &[canceled("c1"), canceled("never-recorded")]);
        assert_eq!(s.label("c1", "EUR_USD"), "EUR_USD", "the cancelled order left");
        assert_eq!(s.label("c2", "GBP_USD"), "GBPUSD", "a live order stayed");
    }
}
