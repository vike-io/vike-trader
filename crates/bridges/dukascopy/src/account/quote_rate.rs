//! The DUKASCOPY RATE SOURCE for the engine's multi-currency money (decision 0183): what one unit of an
//! instrument's QUOTE currency is worth in the ACCOUNT's own currency, read from the sidecar's own TICKS of the
//! conversion pair (the price lane, `crate::feed`). Unlike OANDA's `homeConversions` the venue states no factor
//! here: the rate is the MID of a pair the sidecar prices, and everything that can go wrong is orientation, age and
//! absence, each of which is a refusal in words.
//!
//! # The orientation, which is the whole risk
//!
//! Every rate here is [`AccountPerQuote`]: **account-currency units per ONE unit of the quote currency**. A
//! quote-currency amount is MULTIPLIED by it ([`AccountPerQuote::to_account`]), never divided. The pair that
//! prices it is one of two, and the tick of each points a different way:
//!
//! | account | quote | pair | what its tick is | the rate |
//! |---|---|---|---|---|
//! | USD | JPY | `USDJPY` | JPY per USD (about 150) | `1 / mid` = 0.00667 ([`AccountPerQuoteLeg::AccountIsBase`]) |
//! | USD | GBP | `GBPUSD` | USD per GBP (about 1.35) | `mid` = 1.35 ([`AccountPerQuoteLeg::QuoteIsBase`]) |
//!
//! The flipped rate is a 22,500x error (150 x 150) and is the classic bug, so the direction lives in the TYPE
//! (the only constructor refuses a non-rate) and in the goldens in `account/quote_rate_tests.rs`, whose names say which
//! way they point. The mid (bid + ask over two) is used, never one side and never a last trade: the spread of a
//! major is about 1e-4 of the rate, a hundred-thousandth of the 150x it exists to prevent, and a side would be a
//! choice between overstating and understating a loss.
//!
//! # Freshness and absence
//!
//! The rate's `as_of_ms` is the tick's OWN time (the venue's stamp), which `crate::feed::accept` has already
//! proved within `MAX_TICK_AGE_MS` of this box's clock when the tick arrived. No tick in time, a pair the account's list
//! does not carry, a sidecar that is not up or whose jar predates the price lane: each is a [`QuoteRateError`], and
//! there is no path that answers `1.0` for "I do not know". The split matters to the node: a
//! [`QuoteRateError::NoPair`] (no pair of the account's list converts the two currencies, they are one currency, or a
//! code is empty) is about the INSTRUMENT, so a refusal is remembered; a [`QuoteRateError::Unavailable`] (no sidecar, an
//! old jar, no list stated yet, a closed market, a cap) is about the MOMENT, and the next ask may find a tick. This venue
//! states no `BadQuote`.
//!
//! Only a single FOREX pair of the account's instrument list (`crate::instrument::grid`, as the sidecar stated it at its
//! login) that has the quote currency on one side and the account's on the other is
//! used; a conversion through a third currency is refused, not composed. shortcut: no triangulation, add a
//! two-leg composition (each leg read by the same orientation rule) the day an account trades a pair neither of
//! whose currencies has a direct pair to the account's.
//!
//! Docs-shaped, not measured: the vendor's description of the instruments and of the tick
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>, `getBid`/`getAsk`/`getTime`) is what
//! the sidecar forwards; no live run has captured a tick of a conversion pair.

use crate::feed::{PriceTick, QuoteBus};
use crate::instrument::grid::{InstrumentType, JforexInstrument};

use std::time::Duration;

use vike_model::{AccountPerQuote, AccountPerQuoteLeg, QuoteRate, QuoteRateError, currency_code};

/// The pair whose tick converts one currency into another, and which way it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionPair {
    /// The six-letter pair the price lane subscribes (`USDJPY`).
    pub symbol: String,
    pub leg: AccountPerQuoteLeg,
}

/// The pair that converts `quote_currency` into `account_currency`, from the FOREX pairs of the account's instrument list `rows`
/// (`crate::instrument::grid`): `QUOTEACCOUNT` is [`AccountPerQuoteLeg::QuoteIsBase`], `ACCOUNTQUOTE` is [`AccountPerQuoteLeg::AccountIsBase`].
///
/// # Errors
/// [`QuoteRateError::NoPair`]: either code is empty (`vike_model::currency_code`), they are the same currency (nothing to
/// convert: the caller compares first), or neither pair is a FOREX row of the list.
pub fn conversion_pair(
    rows: &[JforexInstrument],
    quote_ccy: &str,
    account_ccy: &str,
) -> Result<ConversionPair, QuoteRateError> {
    let (Some(quote), Some(account)) = (currency_code(quote_ccy), currency_code(account_ccy))
    else {
        return Err(QuoteRateError::NoPair(format!(
            "dukascopy: `{quote_ccy}` / `{account_ccy}` are not both currency codes, so no pair converts them"
        )));
    };
    if quote == account {
        return Err(QuoteRateError::NoPair(format!(
            "dukascopy: {quote} is the account's own currency; there is nothing to convert"
        )));
    }
    let find = |base: &str, counter: &str| {
        rows.iter()
            .find(|r| r.kind == InstrumentType::Forex && r.pair == format!("{base}/{counter}"))
            .map(|r| r.symbol.clone())
    };
    if let Some(symbol) = find(&quote, &account) {
        return Ok(ConversionPair { symbol, leg: AccountPerQuoteLeg::QuoteIsBase });
    }
    if let Some(symbol) = find(&account, &quote) {
        return Ok(ConversionPair { symbol, leg: AccountPerQuoteLeg::AccountIsBase });
    }
    Err(QuoteRateError::NoPair(format!(
        "dukascopy: the account's instrument list carries neither {quote}/{account} nor {account}/{quote}, so no \
         single pair converts {quote} into the account's {account} (a conversion through a third currency is not \
         composed)"
    )))
}

impl ConversionPair {
    /// The rate one tick of this pair states: the mid, or its inverse, per [`AccountPerQuoteLeg`].
    ///
    /// # Errors
    /// [`QuoteRateError::Unavailable`]: the tick is of another symbol, or its mid is not a finite positive number.
    pub fn account_per_quote(&self, tick: &PriceTick) -> Result<AccountPerQuote, QuoteRateError> {
        if tick.symbol != self.symbol {
            return Err(QuoteRateError::Unavailable(format!(
                "dukascopy: a tick of {} cannot price the conversion pair {}",
                tick.symbol, self.symbol
            )));
        }
        // shortcut: mid, venue converts at settlement rate + 0.1% fee, ~0.1% PnL gap (Dukascopy Europe fee
        // schedule, "Currency Exchange"); upgrade only if the drill's fee line shows the gap matters.
        let mid = (tick.bid + tick.ask) / 2.0;
        AccountPerQuote::of_mid(self.leg, mid).map_err(|_| {
            QuoteRateError::Unavailable(format!(
                "dukascopy: the tick of {} (bid {}, ask {}) states no usable rate",
                self.symbol, tick.bid, tick.ask
            ))
        })
    }
}

/// **The rate that converts `quote_currency` into the account's `account_currency`**, from the freshest tick of the
/// conversion pair on `bus` (asked of the sidecar if nobody has, waiting up to `wait`); the pair is read off the
/// account's instrument list the bus holds.
///
/// # Errors
/// [`QuoteRateError::NoPair`] when no pair converts them, [`QuoteRateError::Unavailable`] when no list has been stated yet
/// or no usable tick could be had,
/// including a tick stamped further AHEAD of this box's clock than the engine accepts a rate to be
/// ([`vike_execution::QUOTE_RATE_FUTURE_SKEW_MS`]): the bus lets a tick through up to [`crate::feed::MAX_TICK_AGE_MS`]
/// ahead, and the engine would refuse such a rate as `quote-rate-future` in a word the operator cannot read.
pub fn quote_rate(
    bus: &QuoteBus,
    quote_ccy: &str,
    account_ccy: &str,
    wait: Duration,
) -> Result<QuoteRate, QuoteRateError> {
    let rows = bus.instrument_list().map_err(QuoteRateError::Unavailable)?;
    let pair = conversion_pair(&rows, quote_ccy, account_ccy)?;
    let tick = bus.fresh_tick(&pair.symbol, wait).map_err(QuoteRateError::Unavailable)?;
    let ahead_ms = tick.timestamp.saturating_sub(vike_model::now_ms());
    if ahead_ms > vike_execution::QUOTE_RATE_FUTURE_SKEW_MS {
        return Err(QuoteRateError::Unavailable(format!(
            "dukascopy: the tick of {} is stamped {:.1} s AHEAD of this box's clock (a skewed or drifting \
             clock); a rate may be stamped at most {} s ahead of it, so this tick is not used as a rate",
            pair.symbol,
            ahead_ms as f64 / 1000.0,
            vike_execution::QUOTE_RATE_FUTURE_SKEW_MS / 1000
        )));
    }
    let account_per_quote = pair.account_per_quote(&tick)?;
    Ok(QuoteRate {
        quote_currency: quote_ccy.trim().to_ascii_uppercase(),
        account_per_quote,
        as_of_ms: tick.timestamp,
    })
}

#[cfg(test)]
#[path = "quote_rate_tests.rs"]
mod quote_rate_tests;
