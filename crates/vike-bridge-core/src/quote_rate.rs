//! The ONE memory of a converting venue's rate reads (B-7): what was read or refused per quote
//! currency, and for how long it is handed out again, so a burst of asks for one currency costs the
//! venue one read. The record and the refusal it keeps are `vike_model`'s (`vike_model::QuoteRate`,
//! [`QuoteRateError`]); each venue's timings are its parameters ([`QuoteRateMemoryConfig`]: cTrader
//! 15 s / 30 s, IBKR 30 s / 10 s), passed in by its bridge or its gatewayhub adapter, never chosen here.
//!
//! It decides nothing about WHEN to ask: single flight, a cap on asks in flight and what a kept answer
//! is worded as stay the caller's.
//!
//! [`SymbolPropertiesQuote`] is the ONE answer a converting bridge gives for a symbol (fxcm, ig): its
//! properties, the rate of its quote currency when that is not the account's, and the account's
//! currency the answer is in. How each venue derives them stays in its bridge.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_model::{ContractValue, QuoteRate, QuoteRateError, SymbolProperties};

/// **A symbol's properties for a node that CONVERTS a quote currency** (decision 0183): the grid, what one
/// unit of quantity stands for, the symbol's own margin rate, the rate of its quote currency when that is
/// not the account's, and the account's currency the answer is expressed in. The gatewayhub forwards it on
/// the wire's symbol facts. The rate record `R` is the bridge's: `vike_model::QuoteRate` alone (fxcm), or
/// with the venue's note of how it was read beside it (ig: the reading of its currencies table that held
/// against the price, which it logs).
#[derive(Clone, Debug, PartialEq)]
pub struct SymbolPropertiesQuote<R = QuoteRate> {
    /// The grid in the engine's units (tick, step, minimum and maximum, each as the venue derives it).
    pub properties: SymbolProperties,
    /// What one unit of quantity stands for. `None` = the venue states no multiplier, so one unit is one
    /// unit (`ContractValue::default()`, the wire's default): fxcm for a forex pair and a multiplier of 1.
    /// ig states it on every answer.
    pub contract_value: Option<ContractValue>,
    /// The symbol's own initial-margin RATE, a fraction of notional within `(0, 1]`, when the venue states
    /// one the bridge admits; `None` = the engine margins the symbol at its own rate.
    pub margin_rate: Option<f64>,
    /// `Some` exactly when the symbol is quoted in another currency than the account's: the quote currency
    /// and its rate. `None` = quoted in the account's own money, which needs no rate.
    pub quote: Option<R>,
    /// The account's currency (upper case, three letters): the ledger the answer is expressed in. The
    /// gatewayhub stamps it on the wire so the node can refuse an answer for an engine anchored to another
    /// currency.
    pub account_currency: String,
}

/// How long a [`QuoteRateMemory`] keeps what it was told. The numbers are each venue's own (a whole
/// authenticated session per ask on one, a socket read on another).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuoteRateMemoryConfig {
    /// How long a rate that was read is handed out again for the same quote currency.
    pub reuse: Duration,
    /// How long a failed read is answered from memory before the venue is asked again.
    pub failed: Duration,
    /// Whether a [`QuoteRateError::NoPair`] is remembered for `failed` like any other failure. `false`:
    /// a verdict is not kept at all (cheap to repeat, or remembered by the caller), and it leaves an
    /// earlier failure of the same currency as it was.
    pub remember_no_pair: bool,
}

/// **The venue's half of the gatewayhub's guard in front of a COSTLY rate ask** (ctrader's: each ask is a whole
/// authenticated session): its words, how long an answer is handed out again ([`Self::MEMORY`]) and how many asks
/// the whole hub may run at once ([`Self::HUB_ASKS`]). The gatewayhub's engine keeps one account's single ask in
/// flight and the hub's count, and words a refused ask as the node's "not now".
pub trait QuoteRateAskSpec: Send + Sync + 'static {
    /// The venue's slug, the first word of a refused ask.
    const VENUE: &'static str;
    /// The venue's name in a sentence (`cTrader`).
    const VENUE_NAME: &'static str;
    /// How long a rate and a failure are handed out again, and whether a `NoPair` verdict is.
    const MEMORY: QuoteRateMemoryConfig;
    /// The most asks running at once in the whole hub.
    const HUB_ASKS: usize;
}

/// **The memory of one account's rate reads, per quote currency**: the last rate read and the last
/// failure kept, each with when it was told. A rate is handed out for
/// [`QuoteRateMemoryConfig::reuse`], a failure for [`QuoteRateMemoryConfig::failed`]; a rate read
/// clears the currency's failure. The rate type `R` is the caller's (the venue's record, or
/// `vike_model::QuoteRate`). Keys are trimmed and upper-cased, so `" jpy"` and `"JPY"` are one
/// currency.
#[derive(Debug)]
pub struct QuoteRateMemory<R> {
    config: QuoteRateMemoryConfig,
    rates: Mutex<HashMap<String, (Instant, R)>>,
    failures: Mutex<HashMap<String, (Instant, QuoteRateError)>>,
}

impl<R: Clone> QuoteRateMemory<R> {
    /// An empty memory keeping for `config`'s windows.
    #[must_use]
    pub fn new(config: QuoteRateMemoryConfig) -> Self {
        Self { config, rates: Mutex::default(), failures: Mutex::default() }
    }

    /// The kept answer for `quote` while it is inside its window: a rate read less than `reuse` ago,
    /// else a failure told less than `failed` ago, else `None` (ask the venue).
    #[must_use]
    pub fn answer(&self, quote: &str) -> Option<Result<R, QuoteRateError>> {
        self.fresh_rate(quote)
            .map(Ok)
            .or_else(|| self.recent_failure(quote).map(|(_, why)| Err(why)))
    }

    /// The rate of `quote` read less than `reuse` ago.
    #[must_use]
    pub fn fresh_rate(&self, quote: &str) -> Option<R> {
        self.last_rate(quote).filter(|(age, _)| *age < self.config.reuse).map(|(_, rate)| rate)
    }

    /// The last rate read for `quote` WHATEVER its age, with that age: what a caller compares a new
    /// read with before it believes it.
    #[must_use]
    pub fn last_rate(&self, quote: &str) -> Option<(Duration, R)> {
        let rates = self.rates.lock().unwrap_or_else(PoisonError::into_inner);
        rates.get(&key(quote)).map(|(at, rate)| (at.elapsed(), rate.clone()))
    }

    /// The failure of `quote` told less than `failed` ago, with its age (for the caller's words).
    #[must_use]
    pub fn recent_failure(&self, quote: &str) -> Option<(Duration, QuoteRateError)> {
        let failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
        failures
            .get(&key(quote))
            .map(|(at, why)| (at.elapsed(), why.clone()))
            .filter(|(age, _)| *age < self.config.failed)
    }

    /// Tell the memory how a read of `quote` ended: a rate is kept and the currency's failure
    /// forgotten; a failure is kept, except a [`QuoteRateError::NoPair`] when `remember_no_pair` is
    /// off (then nothing changes).
    pub fn note(&self, quote: &str, answer: &Result<R, QuoteRateError>) {
        let key = key(quote);
        match answer {
            Ok(rate) => {
                self.rates
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key.clone(), (Instant::now(), rate.clone()));
                self.forget_failure(&key);
            }
            Err(QuoteRateError::NoPair(_)) if !self.config.remember_no_pair => {}
            Err(why) => {
                self.failures
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key, (Instant::now(), why.clone()));
            }
        }
    }

    /// Forget the failure of `quote`: a read answered without a rate to keep (the quote turned out to
    /// be the account's own currency).
    pub fn forget_failure(&self, quote: &str) {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner).remove(&key(quote));
    }
}

/// A quote currency as the memory's key: the engine's spelling of a code (`vike_model::currency_code`),
/// or the empty key for a blank one.
fn key(quote: &str) -> String {
    vike_model::currency_code(quote).unwrap_or_default()
}

#[path = "quote_rate_tests.rs"]
#[cfg(test)]
mod quote_rate_tests;
