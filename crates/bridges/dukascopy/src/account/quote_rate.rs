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
//! | USD | JPY | `USDJPY` | JPY per USD (about 150) | `1 / mid` = 0.00667 ([`vike_model::AccountPerQuoteLeg::AccountIsBase`]) |
//! | USD | GBP | `GBPUSD` | USD per GBP (about 1.35) | `mid` = 1.35 ([`vike_model::AccountPerQuoteLeg::QuoteIsBase`]) |
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
//! # The pair, or the two pairs through USD
//!
//! The pick is the one every converting bridge shares (`vike_model::ConversionRoute::pick`, owner 2026-11-22): a FOREX
//! pair of the account's instrument list (`crate::instrument::grid`, as the sidecar stated it at its login) that has the
//! quote currency on one side and the account's on the other; else TWO pairs, the quote currency's with USD and USD's
//! with the account's (owner, 2026-10-26: "a user must be able to trade ANY pair of every venue, a real account is not
//! always USD"), whose rates multiply; a pair the list carries BOTH ways round is refused, never chosen between. A cross
//! holds two of the bus's rate places (`crate::feed`'s `MAX_RATED_SYMBOLS`) and waits for two ticks, one after the
//! other.
//!
//! Docs-shaped, not measured: the vendor's description of the instruments and of the tick
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>, `getBid`/`getAsk`/`getTime`) is what
//! the sidecar forwards; no live run has captured a tick of a conversion pair.

use crate::feed::{FreshTickError, PriceTick, QuoteBus};
use crate::instrument::grid::{InstrumentType, JforexInstrument};
use crate::refusals::DukascopyRateRefusal;

use std::time::Duration;

use vike_model::{
    AccountPerQuote, ConversionLeg, ConversionRefusal, ConversionRoute, QuoteRate, QuoteRateError,
};

/// **The pair, or the two pairs through USD, whose ticks convert `quote_currency` into `account_currency`**, from the
/// FOREX pairs of the account's instrument list `rows` (`crate::instrument::grid`): each leg's `pair` is the six-letter
/// symbol the price lane subscribes (`USDJPY`) and its `orientation` says which way its tick points.
///
/// # Errors
/// [`QuoteRateError::NoPair`]: either code is empty, they are the same currency (nothing to convert: the caller
/// compares first), the list carries a pair both ways round, or it carries neither a pair of the two nor both legs
/// through USD.
pub fn conversion_route(
    rows: &[JforexInstrument],
    quote_ccy: &str,
    account_ccy: &str,
) -> Result<ConversionRoute<String>, QuoteRateError> {
    let find = |base: &str, counter: &str| {
        rows.iter()
            .find(|r| r.kind == InstrumentType::Forex && r.pair == format!("{base}/{counter}"))
            .map(|r| r.symbol.clone())
    };
    ConversionRoute::pick(quote_ccy, account_ccy, find).map_err(|refusal| {
        let code = |raw: &str| vike_model::currency_code(raw).unwrap_or_default();
        let (quote, account) = (code(quote_ccy), code(account_ccy));
        let refusal = match refusal {
            ConversionRefusal::NotACurrency => DukascopyRateRefusal::NotCurrencyCodes {
                quote_ccy: quote_ccy.to_string(),
                account_ccy: account_ccy.to_string(),
            },
            ConversionRefusal::SameCurrency => {
                DukascopyRateRefusal::SameCurrency { quote: quote.to_string() }
            }
            ConversionRefusal::BothOrientations { from, to } => {
                DukascopyRateRefusal::BothOrientations { from, to }
            }
            ConversionRefusal::NoPair => DukascopyRateRefusal::NoPair {
                quote: quote.to_string(),
                account: account.to_string(),
            },
        };
        QuoteRateError::NoPair(refusal.to_string())
    })
}

/// The rate one tick of `leg`'s pair states: the mid, or its inverse, per the leg's orientation.
///
/// # Errors
/// [`QuoteRateError::Unavailable`]: the tick is of another symbol, or its mid is not a finite positive number.
pub fn leg_rate(
    leg: &ConversionLeg<String>,
    tick: &PriceTick,
) -> Result<AccountPerQuote, QuoteRateError> {
    if tick.symbol != leg.pair {
        let wrong =
            DukascopyRateRefusal::WrongTick { tick: tick.symbol.clone(), pair: leg.pair.clone() };
        return Err(QuoteRateError::Unavailable(wrong.to_string()));
    }
    // shortcut: mid, venue converts at settlement rate + 0.1% fee, ~0.1% PnL gap (Dukascopy Europe fee
    // schedule, "Currency Exchange"); upgrade only if the drill's fee line shows the gap matters.
    let mid = (tick.bid + tick.ask) / 2.0;
    AccountPerQuote::of_mid(leg.orientation, mid).map_err(|_| {
        let unusable = DukascopyRateRefusal::UnusableTick {
            pair: leg.pair.clone(),
            bid: tick.bid,
            ask: tick.ask,
        };
        QuoteRateError::Unavailable(unusable.to_string())
    })
}

/// **Why [`quote_rate`] has no rate**: the engine's [`QuoteRateError`], its words byte for byte, and whether it is
/// because a conversion pair could not be SUBSCRIBED ([`FreshTickError::NotSubscribed`]: every place taken, or the
/// sidecar gone) rather than subscribed and silent, which the caller says once an hour (`crate::ledger`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoRate {
    /// The refusal the engine is handed.
    pub error: QuoteRateError,
    /// A pair the rate needs could not be subscribed.
    pub pair_not_subscribed: bool,
}

impl From<QuoteRateError> for NoRate {
    fn from(error: QuoteRateError) -> Self {
        NoRate { error, pair_not_subscribed: false }
    }
}

/// The rate of one leg from the freshest tick of its pair on `bus` (asked of the sidecar if nobody has, waiting up to
/// `wait`), with the tick's own time.
fn leg_reading(
    bus: &QuoteBus,
    leg: &ConversionLeg<String>,
    wait: Duration,
) -> Result<(AccountPerQuote, i64), NoRate> {
    let tick = bus.fresh_tick(&leg.pair, wait).map_err(|why| NoRate {
        pair_not_subscribed: matches!(why, FreshTickError::NotSubscribed(_)),
        error: QuoteRateError::Unavailable(why.to_string()),
    })?;
    let ahead_ms = tick.timestamp.saturating_sub(vike_model::now_ms());
    if ahead_ms > vike_execution::QUOTE_RATE_FUTURE_SKEW_MS {
        let ahead = DukascopyRateRefusal::TickAhead {
            pair: leg.pair.clone(),
            ahead_secs: ahead_ms as f64 / 1000.0,
            max_secs: vike_execution::QUOTE_RATE_FUTURE_SKEW_MS / 1000,
        };
        return Err(NoRate::from(QuoteRateError::Unavailable(ahead.to_string())));
    }
    Ok((leg_rate(leg, &tick)?, tick.timestamp))
}

/// **The rate that converts `quote_currency` into the account's `account_currency`**, from the freshest tick of the
/// conversion pair on `bus` (asked of the sidecar if nobody has, waiting up to `wait`), or, through USD, the product of
/// its two legs' rates stamped with the OLDER tick's time; the pairs are read off the account's instrument list the bus
/// holds ([`conversion_route`]).
///
/// # Errors
/// A [`NoRate`] whose error is [`QuoteRateError::NoPair`] when no route converts them, [`QuoteRateError::Unavailable`]
/// when no list has been stated yet or no usable tick could be had (a leg of a cross is named; a pair that could not be
/// subscribed is [`NoRate::pair_not_subscribed`] too), including a tick stamped further AHEAD of this box's
/// clock than the engine accepts a rate to be ([`vike_execution::QUOTE_RATE_FUTURE_SKEW_MS`]): the bus lets a tick
/// through up to [`crate::feed::MAX_TICK_AGE_MS`] ahead, and the engine would refuse such a rate as `quote-rate-future`
/// in a word the operator cannot read.
pub fn quote_rate(
    bus: &QuoteBus,
    quote_ccy: &str,
    account_ccy: &str,
    wait: Duration,
) -> Result<QuoteRate, NoRate> {
    let rows = bus.instrument_list().map_err(QuoteRateError::Unavailable)?;
    let route = conversion_route(&rows, quote_ccy, account_ccy)?;
    let (account_per_quote, as_of_ms) = match &route {
        ConversionRoute::Direct(leg) => leg_reading(bus, leg, wait)?,
        ConversionRoute::ThroughHub([first, second]) => {
            let named = |leg: &ConversionLeg<String>, e: NoRate| NoRate {
                error: match e.error {
                    QuoteRateError::Unavailable(why) => {
                        let leg_words = DukascopyRateRefusal::LegHasNoRate {
                            from: first.from.to_string(),
                            to: second.to.to_string(),
                            leg_from: leg.from.to_string(),
                            leg_to: leg.to.to_string(),
                            why,
                        };
                        QuoteRateError::Unavailable(leg_words.to_string())
                    }
                    other => other,
                },
                pair_not_subscribed: e.pair_not_subscribed,
            };
            let (r1, t1) = leg_reading(bus, first, wait).map_err(|e| named(first, e))?;
            let (r2, t2) = leg_reading(bus, second, wait).map_err(|e| named(second, e))?;
            let rate = AccountPerQuote::new(r1.get() * r2.get()).ok_or_else(|| {
                let composed = DukascopyRateRefusal::ComposedNotFinite {
                    from: first.from.to_string(),
                    to: second.to.to_string(),
                };
                QuoteRateError::Unavailable(composed.to_string())
            })?;
            (rate, t1.min(t2))
        }
    };
    QuoteRate::new(quote_ccy, account_per_quote, as_of_ms).ok_or_else(|| {
        let not_a_code =
            DukascopyRateRefusal::NotACurrencyCode { quote_ccy: quote_ccy.to_string() };
        NoRate::from(QuoteRateError::NoPair(not_a_code.to_string()))
    })
}

#[cfg(test)]
#[path = "quote_rate_tests.rs"]
mod quote_rate_tests;
