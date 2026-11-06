//! binance's (and aster's: a Binance fork, same wire grammar) side of the one-account-many-symbols
//! router. The router itself, its coid map and its rules are `vike_bridge_core::per_symbol` (one copy
//! for binance, aster, bybit and okx); what stays here is the line binance and aster log when a
//! submit is refused because no client could be built for its symbol, under this module's target as
//! before the router moved down.

use vike_model::OrderRequest;

/// The refusal line handed to `vike_bridge_core::per_symbol::PerSymbolRest::new` by binance and
/// aster (`vike_bridge_core::per_symbol::RefusalLog`).
pub fn refused_no_client(request: &OrderRequest, why: &str) {
    tracing::warn!(target: "vike_binance::per_symbol", symbol = %request.symbol, %why, "order refused: no client for its symbol");
}
