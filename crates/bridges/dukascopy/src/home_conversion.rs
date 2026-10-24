//! The DUKASCOPY RATE SOURCE for the engine's multi-currency money (decision 0183): what one unit of an
//! instrument's QUOTE currency is worth in the ACCOUNT's own currency, read from the sidecar's own TICKS of the
//! conversion pair (the price lane, `crate::quotes`). Unlike OANDA's `homeConversions` the venue states no factor
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
//! | USD | JPY | `USDJPY` | JPY per USD (about 150) | `1 / mid` = 0.00667 ([`Leg::AccountIsBase`]) |
//! | USD | GBP | `GBPUSD` | USD per GBP (about 1.35) | `mid` = 1.35 ([`Leg::QuoteIsBase`]) |
//!
//! The flipped rate is a 22,500x error (150 x 150) and is the classic bug, so the direction lives in the TYPE
//! (the only constructor refuses a non-rate) and in the goldens in `home_conversion_tests.rs`, whose names say which
//! way they point. The mid (bid + ask over two) is used, never one side and never a last trade: the spread of a
//! major is about 1e-4 of the rate, a hundred-thousandth of the 150x it exists to prevent, and a side would be a
//! choice between overstating and understating a loss.
//!
//! # Freshness and absence
//!
//! The rate's `as_of_ms` is the tick's OWN time (the venue's stamp), which `crate::quotes::accept` has already
//! proved within `MAX_TICK_AGE_MS` of this box's clock when the tick arrived. No tick in time, a pair the SDK table
//! does not carry, a sidecar that is not up or whose jar predates the price lane: each is a [`RateError`], and there
//! is no path that answers `1.0` for "I do not know".
//!
//! Only a single pair of the SDK table that has the quote currency on one side and the account's on the other is
//! used; a conversion through a third currency is refused, not composed. shortcut: no triangulation, add a
//! two-leg composition (as `vike_exec`'s `RateBook::rate_to` does) the day an account trades a pair neither of
//! whose currencies has a direct pair to the account's.
//!
//! Docs-shaped, not measured: the vendor's description of the instruments and of the tick
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>, `getBid`/`getAsk`/`getTime`) is what
//! the sidecar forwards; no live run has captured a tick of a conversion pair.

use crate::jforex_grid::{JforexKind, jforex_instruments};
use crate::quotes::{PriceTick, QuoteBus};

use std::time::Duration;

/// **Account-currency units per ONE unit of a quote currency.** Multiply a quote-currency amount by it
/// ([`Self::to_account`]); never divide. Finite and strictly positive, or it does not exist.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountPerQuote(f64);

impl AccountPerQuote {
    /// `None` unless `account_units_per_quote_unit` is finite and greater than zero.
    #[must_use]
    pub fn new(account_units_per_quote_unit: f64) -> Option<Self> {
        (account_units_per_quote_unit.is_finite() && account_units_per_quote_unit > 0.0)
            .then_some(Self(account_units_per_quote_unit))
    }

    /// The factor: account-currency units per one quote-currency unit.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// A quote-currency amount in account currency (`quote_amount * factor`).
    #[must_use]
    pub fn to_account(self, quote_amount: f64) -> f64 {
        quote_amount * self.0
    }
}

/// Which way the conversion pair points, so one tick becomes [`AccountPerQuote`] one way only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leg {
    /// The pair is `QUOTE`/`ACCOUNT` (`GBPUSD` for a GBP quote on a USD account): its tick is already account
    /// units per one quote unit, and the rate is the mid.
    QuoteIsBase,
    /// The pair is `ACCOUNT`/`QUOTE` (`USDJPY` for a JPY quote on a USD account): its tick is quote units per one
    /// account unit, and the rate is the INVERSE of the mid.
    AccountIsBase,
}

/// The pair whose tick converts one currency into another, and which way it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionPair {
    /// The six-letter pair the price lane subscribes (`USDJPY`).
    pub symbol: String,
    pub leg: Leg,
}

/// What one quote currency is worth in the account's, at one moment (the tick's own time).
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRate {
    /// The quote currency (upper-case).
    pub quote_ccy: String,
    /// Account-currency units per one `quote_ccy` unit.
    pub account_per_quote: AccountPerQuote,
    /// The tick's own time, epoch ms.
    pub as_of_ms: i64,
}

/// Why no rate. The split matters to the node: a [`Self::NoPair`] is about the INSTRUMENT (it can never be
/// converted by this venue, so a refusal is remembered), an [`Self::Unavailable`] is about the MOMENT (the next
/// ask may find a tick).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateError {
    /// No pair of the SDK table converts the two currencies, or they are the same currency, or a code is not one.
    NoPair(String),
    /// A pair exists but no usable tick of it could be had now (no sidecar, an old jar, a closed market, a cap).
    Unavailable(String),
}

impl std::fmt::Display for RateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPair(why) | Self::Unavailable(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for RateError {}

fn code(ccy: &str) -> Option<String> {
    let c = ccy.trim();
    (c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())).then(|| c.to_ascii_uppercase())
}

/// The pair that converts `quote_ccy` into `account_ccy`, from the SDK's own table of predefined FOREX pairs
/// (`crate::jforex_grid`): `QUOTEACCOUNT` is [`Leg::QuoteIsBase`], `ACCOUNTQUOTE` is [`Leg::AccountIsBase`].
///
/// # Errors
/// [`RateError::NoPair`]: either code is not a three-letter currency, they are the same currency (nothing to
/// convert: the caller compares first), or neither pair is a FOREX row of the table.
pub fn conversion_pair(quote_ccy: &str, account_ccy: &str) -> Result<ConversionPair, RateError> {
    let (Some(quote), Some(account)) = (code(quote_ccy), code(account_ccy)) else {
        return Err(RateError::NoPair(format!(
            "dukascopy: `{quote_ccy}` / `{account_ccy}` are not both currency codes, so no pair converts them"
        )));
    };
    if quote == account {
        return Err(RateError::NoPair(format!(
            "dukascopy: {quote} is the account's own currency; there is nothing to convert"
        )));
    }
    let find = |base: &str, counter: &str| {
        jforex_instruments()
            .iter()
            .find(|r| r.kind == JforexKind::Forex && r.pair == format!("{base}/{counter}"))
            .map(|r| r.symbol.clone())
    };
    if let Some(symbol) = find(&quote, &account) {
        return Ok(ConversionPair { symbol, leg: Leg::QuoteIsBase });
    }
    if let Some(symbol) = find(&account, &quote) {
        return Ok(ConversionPair { symbol, leg: Leg::AccountIsBase });
    }
    Err(RateError::NoPair(format!(
        "dukascopy: the SDK table carries neither {quote}/{account} nor {account}/{quote}, so no single pair \
         converts {quote} into the account's {account} (a conversion through a third currency is not composed)"
    )))
}

impl ConversionPair {
    /// The rate one tick of this pair states: the mid, or its inverse, per [`Leg`].
    ///
    /// # Errors
    /// [`RateError::Unavailable`]: the tick is of another symbol, or its mid is not a finite positive number.
    pub fn account_per_quote(&self, tick: &PriceTick) -> Result<AccountPerQuote, RateError> {
        if tick.symbol != self.symbol {
            return Err(RateError::Unavailable(format!(
                "dukascopy: a tick of {} cannot price the conversion pair {}",
                tick.symbol, self.symbol
            )));
        }
        // shortcut: mid, venue converts at settlement rate + 0.1% fee, ~0.1% PnL gap (Dukascopy Europe fee
        // schedule, "Currency Exchange"); upgrade only if the drill's fee line shows the gap matters.
        let mid = (tick.bid + tick.ask) / 2.0;
        let rate = match self.leg {
            Leg::QuoteIsBase => mid,
            Leg::AccountIsBase => 1.0 / mid,
        };
        AccountPerQuote::new(rate).ok_or_else(|| {
            RateError::Unavailable(format!(
                "dukascopy: the tick of {} (bid {}, ask {}) states no usable rate",
                self.symbol, tick.bid, tick.ask
            ))
        })
    }
}

/// **The rate that converts `quote_ccy` into the account's `account_ccy`**, from the freshest tick of the
/// conversion pair on `bus` (asked of the sidecar if nobody has, waiting up to `wait`).
///
/// # Errors
/// [`RateError::NoPair`] when no pair converts them, [`RateError::Unavailable`] when no usable tick could be had,
/// including a tick stamped further AHEAD of this box's clock than the engine accepts a rate to be
/// ([`vike_exec::QUOTE_RATE_FUTURE_SKEW_MS`]): the bus lets a tick through up to [`crate::quotes::MAX_TICK_AGE_MS`]
/// ahead, and the engine would refuse such a rate as `quote-rate-future` in a word the operator cannot read.
pub fn quote_rate(
    bus: &QuoteBus,
    quote_ccy: &str,
    account_ccy: &str,
    wait: Duration,
) -> Result<QuoteRate, RateError> {
    let pair = conversion_pair(quote_ccy, account_ccy)?;
    let tick = bus.fresh_tick(&pair.symbol, wait).map_err(RateError::Unavailable)?;
    let ahead_ms = tick.ts.saturating_sub(vike_model::now_ms());
    if ahead_ms > vike_exec::QUOTE_RATE_FUTURE_SKEW_MS {
        return Err(RateError::Unavailable(format!(
            "dukascopy: the tick of {} is stamped {:.1} s AHEAD of this box's clock (a skewed or drifting \
             clock); a rate may be stamped at most {} s ahead of it, so this tick is not used as a rate",
            pair.symbol,
            ahead_ms as f64 / 1000.0,
            vike_exec::QUOTE_RATE_FUTURE_SKEW_MS / 1000
        )));
    }
    let account_per_quote = pair.account_per_quote(&tick)?;
    Ok(QuoteRate {
        quote_ccy: quote_ccy.trim().to_ascii_uppercase(),
        account_per_quote,
        as_of_ms: tick.ts,
    })
}

#[cfg(test)]
#[path = "home_conversion_tests.rs"]
mod home_conversion_tests;
