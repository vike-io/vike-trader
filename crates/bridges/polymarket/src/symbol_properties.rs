//! **What the gatewayhub asks of polymarket to admit an outcome token the engine was not started on** (decision 0173):
//! the venue's half of the list-answered symbol facts ([`SymbolPropertiesListSpecPolymarket`]): every token read ONE
//! at a time, and a market that takes no orders refused by the venue word this crate's catalog writes
//! ([`crate::catalog::VENUE_WORD_NOT_ACCEPTING_ORDERS`], named here, never restated). The listing, its bounds, the
//! checks every venue shares (a share is answered `Cash` at a margin rate of 1 there: it is bought outright) and the
//! wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`); a market NAME is resolved
//! to its token before the facts are asked (`crate::catalog::market_ref`).

use vike_bridge_core::symbol_properties::{
    SymbolPropertiesError, SymbolPropertiesListAnswer, SymbolPropertiesListSpec,
    SymbolPropertiesWords,
};
use vike_model::Instrument;

use crate::catalog::VENUE_WORD_NOT_ACCEPTING_ORDERS;
use crate::refusals::PolymarketPropertiesRefusal;

/// **polymarket's symbol facts**: the symbol is an outcome's token id, read ONE at a time out of Gamma's public
/// `clob_token_ids=` lookup (the market's tick, minimum size and neg-risk flag, and the share lot the row adds),
/// answered `Cash` with a margin rate of 1 by the shared fully-funded rule (a share is bought outright), and a market
/// that takes no orders is refused by name.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecPolymarket;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecPolymarket {
    fn venue(&self) -> &'static str {
        "polymarket"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("the outcome tokens of the open markets Gamma lists")
    }

    /// Every outcome token (Gamma's `clob_token_ids=` point lookup; the venue lists tens of thousands).
    fn one_at_a_time(&self, _key: &str) -> bool {
        true
    }

    fn row_answer(
        &self,
        symbol: &str,
        _key: &str,
        row: &Instrument,
        _now_ms: i64,
        _words: &SymbolPropertiesWords,
    ) -> Result<SymbolPropertiesListAnswer, SymbolPropertiesError> {
        polymarket_refusal(row, symbol)
            .map_err(|refusal| SymbolPropertiesError::Refused(refusal.carried()))?;
        Ok(SymbolPropertiesListAnswer::default())
    }
}

/// Why a polymarket outcome row cannot be admitted: its market is closed, inactive or paused, so an order on
/// the token only reaches the venue's own rejection. The row's description is `"{question} — {outcome}"`.
fn polymarket_refusal(row: &Instrument, symbol: &str) -> Result<(), PolymarketPropertiesRefusal> {
    match row.contract_type.as_deref() {
        Some(VENUE_WORD_NOT_ACCEPTING_ORDERS) => {
            Err(PolymarketPropertiesRefusal::NotAcceptingOrders {
                symbol: symbol.to_string(),
                description: row.description.clone(),
            })
        }
        _ => Ok(()),
    }
}
