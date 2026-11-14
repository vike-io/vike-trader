//! **What the gatewayhub asks of aster to admit a symbol the engine was not started on** (decision 0173): the venue's
//! half of the list-answered symbol facts ([`SymbolPropertiesListSpecAster`]): spot read one symbol at a time, the
//! USDS-M perpetuals out of one list. The listing, its bounds, the checks every venue shares (a `.P` row that is not
//! linear is refused there) and the wire are the gatewayhub's (`crates/vike-gatewayhub/src/trade/adapters/symbol_facts.rs`).

use vike_bridge_core::symbol_properties::SymbolPropertiesListSpec;

/// **aster's symbol facts**: spot read one symbol at a time, the USDS-M perpetuals out of one list (linear only: a
/// `.P` row that is not linear is refused by the shared rule with the venue's word).
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesListSpecAster;

impl SymbolPropertiesListSpec for SymbolPropertiesListSpecAster {
    fn venue(&self) -> &'static str {
        "aster"
    }

    fn covers(&self, _list_key: &str, _account_listed: bool) -> Option<&'static str> {
        Some("spot and USDS-M perpetuals")
    }

    /// SPOT (`exchangeInfo?symbol=`); the perpetual lane stays one bulk read.
    fn one_at_a_time(&self, key: &str) -> bool {
        !key.ends_with(".P")
    }
}
