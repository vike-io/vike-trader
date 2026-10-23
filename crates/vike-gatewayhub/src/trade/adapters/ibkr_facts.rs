//! **IBKR's symbol facts, bounded**: what `TradeUp::SymbolFacts` answers for an IBKR stock name
//! (`docs/ops/any-symbol-remaining-venues-plan.md`, "ibkr").
//!
//! IBKR has no instrument list to keep: a name is resolved by asking the account's logged-in gateway
//! (`vike_ibkr::any_symbol_socket::IbkrFactsSource`: a symbol search, the contract details and the market rules
//! on one data connection). So [`IbkrFacts`] is [`super::keyed_facts`]'s per-key bounds (a keep time per name,
//! a single flight, [`FETCH_BUDGET`] lookups per [`FETCH_WINDOW`], [`MAX_NAMES`] names tracked) with IBKR's
//! answer type, for one account. Under the budget the gateway's gate allows six data connects an hour per
//! gateway and the lookup lane may spend four of them, which is the harder floor
//! (`crates/bridges/vike-ibkr/src/any_symbol_socket.rs`).
//!
//! The fences (a US stock by name, the account's currency, one conId, the grid) are
//! `vike_ibkr::any_symbol`'s.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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
pub struct IbkrFacts {
    keyed: KeyedFacts,
    /// How long a kept answer lives (the keyed bounds' `ttl`): a recorded magnifier older than that belongs to an
    /// answer that is gone.
    ttl: Duration,
    /// The `priceMagnifier` each looked-up name's contract carried, and when it was looked up. The kept answer is
    /// a [`WireSymbolFacts`] with no room for it, and the converting path judges it AFTER the rate read says the
    /// stock is foreign ([`Self::price_magnifier`]), possibly against a grid served from the keep.
    magnifiers: Mutex<HashMap<String, (Instant, i32)>>,
}

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
        Self {
            keyed: KeyedFacts::new(KeyedBounds {
                venue: "ibkr",
                keys: "names",
                lookups: "lookups",
                max_keys: MAX_NAMES,
                ttl,
                failed_ttl,
                budget,
                window,
            }),
            ttl,
            magnifiers: Mutex::default(),
        }
    }

    /// The facts of `name` (already canonical: upper case), looking it up with `fetch` when no fresh answer is
    /// kept and the budget allows. A `Refused` answer is kept for the failed keep time; an `Unavailable` one is
    /// worded as the node's "not now" and kept only as a failure of that length. The contract's price magnifier is
    /// recorded ([`Self::price_magnifier`]) and NOT judged here: this is the plain path's answer too, and it
    /// admits what it always did.
    pub fn facts(
        &self,
        name: &str,
        fetch: impl FnOnce() -> Result<StockFacts, FactsError>,
    ) -> Result<WireSymbolFacts, String> {
        self.keyed.facts(name, || match fetch() {
            // A share: `Linear`, 1.0 - the contract value stays absent.
            Ok(facts) => {
                self.record_magnifier(name, facts.price_magnifier);
                Ok(WireSymbolFacts { properties: facts.properties, ..WireSymbolFacts::default() })
            }
            Err(FactsError::Refused(why)) => Err(KeyedError::Refused(why)),
            Err(FactsError::Unavailable(why)) => Err(KeyedError::Unavailable(why)),
        })
    }

    /// The `priceMagnifier` of the contract `name`'s kept answer was looked up with, or `None` when it is not
    /// known (never looked up, or older than the keep). Looked up AFTER [`Self::facts`] answered, so a kept answer's
    /// record is no older than the keep unless something is wrong, and `None` is then a refusal, not a guess.
    #[must_use]
    pub fn price_magnifier(&self, name: &str) -> Option<i32> {
        let kept = self.magnifiers.lock().unwrap_or_else(PoisonError::into_inner);
        kept.get(name).filter(|(at, _)| at.elapsed() < self.ttl).map(|(_, magnifier)| *magnifier)
    }

    /// Remember `magnifier` for `name`, dropping the records of answers that have expired (so the table is as
    /// small as the keep and the lookup budget allow).
    fn record_magnifier(&self, name: &str, magnifier: i32) {
        let mut kept = self.magnifiers.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|_, (at, _)| at.elapsed() < self.ttl);
        kept.insert(name.to_string(), (Instant::now(), magnifier));
    }
}

#[cfg(test)]
#[path = "ibkr_facts_tests.rs"]
mod ibkr_facts_tests;
