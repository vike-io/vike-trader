//! **IG's symbol facts, as the gatewayhub's per-key bounds ask them** (`docs/ops/any-symbol-remaining-venues-plan.md`,
//! "ig"): the venue's half of `SymbolPropertiesCacheKeyed<S>` ([`SymbolPropertiesKeyedSpecIg`]).
//!
//! IG has no instrument list to cache: an epic's dealing rules are `GET /markets/{epic}` on a logged-in session, and the
//! account's currency is `GET /accounts`. So the gatewayhub bounds the lookups per epic (a keep time per epic, a single
//! flight, [`FETCH_BUDGET`] upstream fetches per [`FETCH_WINDOW`], [`MAX_EPICS`] epics tracked), one set per IG account,
//! and the asking account's own credentials pay for the reads ([`crate::IgPropertiesSource`], which keeps one session, so a
//! flood of epics is not a flood of logins). The fences (a measured epic type, USD, the pip grid) are
//! `market_facts`'.

use std::time::Duration;

use vike_bridge_core::quote_rate::SymbolPropertiesQuote;
use vike_bridge_core::symbol_properties::{
    SymbolPropertiesAnswer, SymbolPropertiesKeyedError, SymbolPropertiesKeyedSpec,
};
use vike_model::QuoteRate;

use crate::{IgProperties, IgPropertiesError, QuoteRateReading};

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

/// **IG's half of the per-key bounds.** The first kind of ask is the plain one (an epic of a measured type quoted in a
/// USD account's currency); the second is a node that CONVERTS a quote currency (decision 0183): an epic quoted in
/// another currency than the account's comes back with its quote currency and the rate, kept under its own key
/// ([`Self::other_key`]), so the plain ask's answer for the same epic (a refusal, for such an epic) and this one never
/// answer for each other. A rate IG cannot give, or that no price confirms, is a failure of the RATE and remembered as
/// nothing about the epic. ⚠ **A failure on the converting path is remembered for seconds, never the failed keep time**
/// (the gatewayhub's rate memory: a failed read 5 s, no usable rate 10 s): the node retries a rate every 10 s and the
/// engine denies opening orders once a rate is 120 s old, so a 503 served from memory for five minutes was minutes of
/// denied orders. Only a definitive `Refused` (not an epic, a fence) keeps the long time, and so does an `Account`
/// failure (the login was refused for good, the account's row is missing or states no currency): that one only a
/// change cures, so a caller that keeps asking must not make a fresh login every 5 s per epic.
#[derive(Debug, Clone, Copy, Default)]
pub struct SymbolPropertiesKeyedSpecIg;

impl SymbolPropertiesKeyedSpec for SymbolPropertiesKeyedSpecIg {
    const VENUE: &'static str = "ig";
    const KEYS: &'static str = "epics";
    const LOOKUPS: &'static str = "epic lookups";
    const MAX_KEYS: usize = MAX_EPICS;
    const BUDGET: usize = FETCH_BUDGET;
    const WINDOW: Duration = FETCH_WINDOW;
    const REFRESH_RESERVE: usize = RATE_REFRESH_RESERVE;

    type Fetched = Result<IgProperties, IgPropertiesError>;
    type FetchedOther =
        Result<SymbolPropertiesQuote<(QuoteRate, QuoteRateReading)>, IgPropertiesError>;
    type Note = ();

    /// The plain answer: the grid and IG's contract value (never the default), IG's own margin rate for the epic when it
    /// states one; no rate and no ledger. A refusal is a verdict; an unreachable venue (or account) is "not now", kept
    /// for the failed keep time; the plain ask never asks for a rate, so a rate failure is only a fence worded as one.
    fn answer<A: SymbolPropertiesAnswer>(
        fetched: Self::Fetched,
    ) -> Result<(A, Option<()>), SymbolPropertiesKeyedError> {
        match fetched {
            Ok(facts) => Ok((
                A::of(facts.properties, Some(facts.contract_value), facts.margin_rate, None, None),
                None,
            )),
            Err(IgPropertiesError::Refused(why)) => {
                Err(SymbolPropertiesKeyedError::Refused(why.carried()))
            }
            Err(IgPropertiesError::Unavailable(why, _) | IgPropertiesError::Account(why)) => {
                Err(SymbolPropertiesKeyedError::Unavailable(why.carried()))
            }
            Err(IgPropertiesError::NoRate(why)) => {
                Err(SymbolPropertiesKeyedError::RateUnavailable(why.carried()))
            }
        }
    }

    /// The converting answer: the grid, the contract value and margin rate as stated, the rate when the epic needs one,
    /// and the ledger currency the answer is expressed in (the node refuses it against an engine anchored to another).
    fn answer_other<A: SymbolPropertiesAnswer>(
        fetched: Self::FetchedOther,
    ) -> Result<A, SymbolPropertiesKeyedError> {
        match fetched {
            Ok(answer) => Ok(A::of(
                answer.properties,
                answer.contract_value,
                answer.margin_rate,
                answer.quote.map(|(rate, _reading)| rate),
                Some(answer.account_currency),
            )),
            Err(IgPropertiesError::Refused(why)) => {
                Err(SymbolPropertiesKeyedError::Refused(why.carried()))
            }
            Err(IgPropertiesError::Unavailable(why, _)) => {
                Err(SymbolPropertiesKeyedError::NotNow(why.carried()))
            }
            // The long failure memory, as every key held before the rate path's short one (`failed_ttl`).
            Err(IgPropertiesError::Account(why)) => {
                Err(SymbolPropertiesKeyedError::Unavailable(why.carried()))
            }
            Err(IgPropertiesError::NoRate(why)) => {
                Err(SymbolPropertiesKeyedError::RateUnavailable(why.carried()))
            }
        }
    }

    fn other_key(epic: &str) -> String {
        format!("{epic}\u{1f}quoted")
    }
}
