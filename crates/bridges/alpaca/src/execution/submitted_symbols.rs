//! The symbol each order was SUBMITTED under, so its fills carry it.
//!
//! The order path slashes a bare crypto pair (`BTCUSD` -> `BTC/USD`, `execution::event_mapper::to_alpaca_symbol`) and
//! every trade update names the symbol that way. `vike_execution::Engine` compares symbols by exact
//! string and DROPS a bare `Fill` whose symbol it neither mounts nor registered the order under, with no
//! log and no counter: the order's lifecycle wrap advanced and the position never moved. So a fill of an
//! order this process submitted is stamped with the symbol of the ORDER, whichever spelling the submitter
//! used (`BTCUSD` and `BTC/USD` both book, each under its own name). The same shape as
//! `crates/bridges/vike-ibkr/src/execution/event_mapper.rs`'s `register_symbol`.
//!
//! The table is the shared `vike_bridge_core::per_symbol::OrderClientIdSymbols`, bounded at this venue's cap
//! ([`submitted_symbols`]). One table, shared by the exec thread (records at the submit) and the SSE thread
//! (reads it). A fill of an order client id nobody recorded (another client on the same account, an order placed before
//! a restart that the host did not teach through `ExecutionClient::adopt_restored`) keeps the venue's own
//! spelling.

use std::sync::Arc;

use vike_bridge_core::per_symbol::OrderClientIdSymbols;

/// The most orders the table holds. Entries leave with their order (`OrderClientIdSymbols::forget`), so this is a
/// leak guard: past it a new order is not recorded and its fills carry the venue's spelling (loudly).
/// Nothing is evicted to make room, because a live order's entry is the only thing that books its fill.
const MAX_RECORDED: usize = 4096;

/// The order client id -> submitted-symbol table one start shares between its threads (cloning the `Arc` shares it).
pub fn submitted_symbols() -> Arc<OrderClientIdSymbols> {
    Arc::new(OrderClientIdSymbols::with_cap(MAX_RECORDED, table_full))
}

fn table_full(order_client_id: &str, cap: usize) {
    tracing::warn!(
        order_client_id,
        cap,
        "alpaca: submitted-symbol table full - this order's fills will carry the venue's \
         spelling of the symbol, which an engine mounted on another spelling drops"
    );
}
