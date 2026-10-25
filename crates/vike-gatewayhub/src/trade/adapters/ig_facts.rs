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
use vike_ig::{IgFactsError, IgMarketFacts, IgQuotedFacts, QuoteRate};

use super::keyed_facts::{KeyedBounds, KeyedError, KeyedFacts, RateMemory};
use super::symbol_facts::{LISTING_TTL, SESSION_FAILED_TTL};

/// Upstream fetches (each one market read, after the account's currency was read once) allowed per [`FETCH_WINDOW`], per account.
pub const FETCH_BUDGET: usize = 12;

/// See [`FETCH_BUDGET`].
pub const FETCH_WINDOW: Duration = Duration::from_secs(60);

/// How many epics are tracked at once.
pub const MAX_EPICS: usize = 64;

/// How many of [`FETCH_BUDGET`] only the RE-READ of an epic that has answered with a conversion rate may spend: the
/// node re-reads a held position's rate about once a minute (the engine calls one stale at 120 s and denies opening
/// orders), and a flood of first looks at other epics (up to 16 extra symbols per engine) must not take those.
pub const RATE_REFRESH_RESERVE: usize = 6;

/// See the module doc.
#[derive(Debug)]
pub struct IgFacts(KeyedFacts);

impl Default for IgFacts {
    fn default() -> Self {
        Self::with_bounds(LISTING_TTL, SESSION_FAILED_TTL, FETCH_BUDGET, FETCH_WINDOW)
            .with_refresh_reserve(RATE_REFRESH_RESERVE)
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

    /// These facts keeping `reserve` of the budget for the re-read of an epic that has answered with a rate
    /// ([`RATE_REFRESH_RESERVE`] in production, none in `with_bounds`).
    #[must_use]
    pub fn with_refresh_reserve(self, reserve: usize) -> Self {
        Self(self.0.with_refresh_reserve(reserve))
    }

    /// These facts remembering the rate path's failures for `memory` ([`RateMemory::default`] otherwise).
    #[must_use]
    pub fn with_rate_memory(self, memory: RateMemory) -> Self {
        Self(self.0.with_rate_memory(memory))
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
            Ok(facts) => Ok(wire(facts, None)),
            Err(IgFactsError::Refused(why)) => Err(KeyedError::Refused(why)),
            Err(IgFactsError::Unavailable(why) | IgFactsError::Account(why)) => {
                Err(KeyedError::Unavailable(why))
            }
            // The plain ask never asks for a rate, so this is only a fence worded as a failure.
            Err(IgFactsError::NoRate(why)) => Err(KeyedError::RateUnavailable(why)),
        })
    }

    /// [`Self::facts`] for a node that CONVERTS a quote currency (decision 0183): an epic quoted in another
    /// currency than the account's comes back with its quote currency and the rate, which is kept for
    /// `RATE_KEEP` only (the keep time of the listing never serves a rate). Kept under its own key, so the
    /// plain ask's answer for the same epic (a refusal, for such an epic) and this one never answer for each
    /// other. A rate IG cannot give, or that no price confirms, is worded as a failure of the RATE
    /// (`FACTS_RATE_UNAVAILABLE`) and remembered as nothing about the epic. ⚠ **A failure on this path is
    /// remembered for seconds, never `failed_ttl`** ([`RateMemory`]: a failed read 5 s, no usable rate 10 s): the
    /// node retries a rate every 10 s and the engine denies opening orders once a rate is 120 s old, so a 503
    /// served from memory for five minutes was minutes of denied orders. Only a definitive `Refused` (not an
    /// epic, a fence) keeps the long time, and so does an `Account` failure (the login was refused for good, the
    /// account's row is missing or states no currency): that one only a change cures, so a caller that keeps
    /// asking must not make a fresh login every 5 s per epic.
    pub fn facts_quoted(
        &self,
        epic: &str,
        fetch: impl FnOnce() -> Result<IgQuotedFacts, IgFactsError>,
    ) -> Result<WireSymbolFacts, String> {
        self.0.facts(&format!("{epic}\u{1f}quoted"), || match fetch() {
            Ok(IgQuotedFacts { facts, quote }) => Ok(wire(facts, quote)),
            Err(IgFactsError::Refused(why)) => Err(KeyedError::Refused(why)),
            Err(IgFactsError::Unavailable(why)) => Err(KeyedError::NotNow(why)),
            // The long failure memory, as every key held before the rate path's short one (`failed_ttl`).
            Err(IgFactsError::Account(why)) => Err(KeyedError::Unavailable(why)),
            Err(IgFactsError::NoRate(why)) => Err(KeyedError::RateUnavailable(why)),
        })
    }
}

/// The wire's facts: the grid and IG's contract value (never the default), and the rate when the epic needs one.
fn wire(facts: IgMarketFacts, quote: Option<QuoteRate>) -> WireSymbolFacts {
    WireSymbolFacts {
        properties: facts.properties,
        contract_value: Some(facts.contract_value),
        quote_ccy: quote.as_ref().map(|q| q.quote_ccy.clone()),
        quote_rate: quote.as_ref().map(|q| q.account_per_quote),
        rate_as_of_ms: quote.map(|q| q.as_of_ms),
        ..WireSymbolFacts::default()
    }
}

#[cfg(test)]
#[path = "ig_facts_tests.rs"]
mod ig_facts_tests;
