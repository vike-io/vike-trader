//! **IG's symbol facts, bounded**: what `TradeUp::SymbolFacts` answers for an IG epic
//! (`docs/ops/any-symbol-remaining-venues-plan.md`, "ig").
//!
//! IG has no instrument list to cache: an epic's dealing rules are `GET /markets/{epic}` on a logged-in
//! session, and the account's currency is `GET /accounts`. So [`IgFacts`] is [`super::keyed_facts`]'s per-key
//! bounds (a keep time per epic, a single flight, [`FETCH_BUDGET`] upstream fetches per [`FETCH_WINDOW`],
//! [`MAX_EPICS`] epics tracked) with IG's answer type, and the asking account's own credentials pay for the
//! reads (`vike_ig::IgFactsSource`, which keeps one session, so a flood of epics is not a flood of logins).
//!
//! One of these per IG account (`BridgeTrade` keeps the map): the epic list and the currency are the
//! account's own. The fences (CURRENCIES only, USD, the pip grid) are `vike_ig::market_facts`'.

use std::time::Duration;

use vike_gatewayhub_client::proto::WireSymbolFacts;
use vike_ig::{IgFactsError, IgMarketFacts};

use super::keyed_facts::{KeyedBounds, KeyedError, KeyedFacts};
use super::symbol_facts::{LISTING_TTL, SESSION_FAILED_TTL};

/// Upstream fetches (each one market read, after the account's currency was read once) allowed per [`FETCH_WINDOW`], per account.
pub const FETCH_BUDGET: usize = 12;

/// See [`FETCH_BUDGET`].
pub const FETCH_WINDOW: Duration = Duration::from_secs(60);

/// How many epics are tracked at once.
pub const MAX_EPICS: usize = 64;

/// See the module doc.
#[derive(Debug)]
pub struct IgFacts(KeyedFacts);

impl Default for IgFacts {
    fn default() -> Self {
        Self::with_bounds(LISTING_TTL, SESSION_FAILED_TTL, FETCH_BUDGET, FETCH_WINDOW)
    }
}

impl IgFacts {
    /// [`IgFacts::default`] with explicit bounds (the tests').
    #[must_use]
    pub fn with_bounds(
        ttl: Duration,
        failed_ttl: Duration,
        budget: usize,
        window: Duration,
    ) -> Self {
        Self(KeyedFacts::new(KeyedBounds {
            venue: "ig",
            keys: "epics",
            lookups: "epic lookups",
            max_keys: MAX_EPICS,
            ttl,
            failed_ttl,
            budget,
            window,
        }))
    }

    /// The facts of `epic` (already canonical: upper case), fetching with `fetch` when no fresh answer is
    /// kept and the budget allows. A `Refused` answer is kept for the failed keep time; an `Unavailable` one
    /// is returned worded as the node's "not now" and is not kept as a verdict, only as a failure.
    pub fn facts(
        &self,
        epic: &str,
        fetch: impl FnOnce() -> Result<IgMarketFacts, IgFactsError>,
    ) -> Result<WireSymbolFacts, String> {
        self.0.facts(epic, || match fetch() {
            Ok(facts) => Ok(WireSymbolFacts {
                properties: facts.properties,
                contract_value: Some(facts.contract_value),
            }),
            Err(IgFactsError::Refused(why)) => Err(KeyedError::Refused(why)),
            Err(IgFactsError::Unavailable(why)) => Err(KeyedError::Unavailable(why)),
        })
    }
}

#[cfg(test)]
#[path = "ig_facts_tests.rs"]
mod ig_facts_tests;
