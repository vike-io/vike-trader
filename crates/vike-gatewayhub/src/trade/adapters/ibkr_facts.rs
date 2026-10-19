//! **IBKR's symbol facts, bounded**: what `TradeUp::SymbolFacts` answers for an IBKR stock name
//! (`docs/ops/any-symbol-remaining-venues-plan.md`, "ibkr").
//!
//! IBKR has no instrument list to keep: a name is resolved by asking the account's logged-in gateway
//! (`vike_ibkr::any_symbol_socket::IbkrFactsSource`: a symbol search, the contract details and the market rules
//! on one data connection). So [`IbkrFacts`] is [`super::keyed_facts`]'s per-key bounds (a keep time per name,
//! a single flight, [`FETCH_BUDGET`] lookups per [`FETCH_WINDOW`], [`MAX_NAMES`] names tracked) with IBKR's
//! answer type, for one account. Under the budget the gateway's gate allows six data connects an hour per
//! gateway, which is the harder floor (`crates/bridges/vike-ibkr/src/any_symbol_socket.rs`).
//!
//! The fences (a US stock by name, the account's currency, one conId, the grid) are
//! `vike_ibkr::any_symbol`'s.

use std::time::Duration;

use vike_gatewayhub_client::proto::WireSymbolFacts;
use vike_ibkr::any_symbol::{FactsError, StockFacts};

use super::keyed_facts::{KeyedBounds, KeyedError, KeyedFacts};
use super::symbol_facts::{LISTING_TTL, SESSION_FAILED_TTL};

/// Lookups allowed per [`FETCH_WINDOW`], per account.
pub const FETCH_BUDGET: usize = 6;

/// See [`FETCH_BUDGET`].
pub const FETCH_WINDOW: Duration = Duration::from_secs(60);

/// How many names are tracked at once.
pub const MAX_NAMES: usize = 64;

/// See the module doc.
#[derive(Debug)]
pub struct IbkrFacts(KeyedFacts);

impl Default for IbkrFacts {
    fn default() -> Self {
        Self::with_bounds(LISTING_TTL, SESSION_FAILED_TTL, FETCH_BUDGET, FETCH_WINDOW)
    }
}

impl IbkrFacts {
    /// [`IbkrFacts::default`] with explicit bounds (the tests').
    #[must_use]
    pub fn with_bounds(
        ttl: Duration,
        failed_ttl: Duration,
        budget: usize,
        window: Duration,
    ) -> Self {
        Self(KeyedFacts::new(KeyedBounds {
            venue: "ibkr",
            keys: "names",
            lookups: "lookups",
            max_keys: MAX_NAMES,
            ttl,
            failed_ttl,
            budget,
            window,
        }))
    }

    /// The facts of `name` (already canonical: upper case), looking it up with `fetch` when no fresh answer is
    /// kept and the budget allows. A `Refused` answer is kept for the failed keep time; an `Unavailable` one is
    /// worded as the node's "not now" and kept only as a failure of that length.
    pub fn facts(
        &self,
        name: &str,
        fetch: impl FnOnce() -> Result<StockFacts, FactsError>,
    ) -> Result<WireSymbolFacts, String> {
        self.0.facts(name, || match fetch() {
            // A share: `Linear`, 1.0 - the contract value stays absent.
            Ok(facts) => Ok(WireSymbolFacts { properties: facts.properties, contract_value: None }),
            Err(FactsError::Refused(why)) => Err(KeyedError::Refused(why)),
            Err(FactsError::Unavailable(why)) => Err(KeyedError::Unavailable(why)),
        })
    }
}

#[cfg(test)]
#[path = "ibkr_facts_tests.rs"]
mod ibkr_facts_tests;
