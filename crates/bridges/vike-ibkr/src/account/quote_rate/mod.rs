//! The IBKR RATE SOURCE for the engine's multi-currency money (decision 0183, phase 1): what one unit of a
//! stock's QUOTE currency is worth in the ledger's own currency, read from a LIVE forex quote of the gateway.
//!
//! IBKR states no conversion factor the way OANDA's `homeConversions` does. The account's ledger has an
//! `ExchangeRate` key (per currency, against the base): IBKR's account-value-keys page lists it ("The
//! exchange rate of the currency to your base currency", `https://www.interactivebrokers.com/docs/tws-api/doc/
//! account-portfolio-data/account-updates/account-value-keys`) but states no DIRECTION (which currency is
//! the numerator), and that page is for `reqAccountUpdates`, not for an account-summary `$LEDGER` row. So it
//! is NOT used; `tests/ibkr_smokes/ibkr_fx_smoke.rs` prints it beside the market mid so its direction can be read off a
//! gateway. The source is the market: the bid and ask of the IDEALPRO forex pair between the two
//! currencies, on the data connection every other IBKR consumer shares (`crate::feed::price::session`), averaged
//! into a mid.
//!
//! # The orientation, which is the whole risk
//!
//! Every rate here is [`AccountPerQuote`]: **ledger-currency units per ONE unit of the quote currency**. A
//! quote-currency amount is MULTIPLIED by it, never divided. A forex contract names its BASE currency first
//! and prices ONE base unit in the second (the quote): `EUR.USD` at 1.10 is 1.10 USD per EUR
//! (`Contract.symbol = EUR`, `.currency = USD`, `.exchange = IDEALPRO`, as IBKR's contract page writes a
//! forex pair: <https://interactivebrokers.github.io/tws-api/basic_contracts.html>, "FX Pairs"). ⚠ That page
//! is a code sample with no word on the price's direction, and the current TWS API page (`contracts/fx-pairs`)
//! even calls the `currency` field the "base currency forex pair"; the one statement that a price is quote
//! units per ONE base unit is IBKR's Knowledge Base article "Forex Basics" (EUR.USD 1.40 = 1.40 USD per EUR),
//! so the direction is not a document of the API: the goldens below pin it, [`check_against_previous`] refuses
//! a read that flips between two reads of one currency, and the engine's own step guard (`MAX_QUOTE_RATE_STEP`)
//! stands behind both. So on a
//! USD ledger a EUR stock reads the pair `EUR.USD` as it stands (`1.10`), a JPY stock reads `USD.JPY` and
//! INVERTS it (`1/150.10`, never `150.10`: the flipped rate is a 22,500x error on a yen), and on a EUR ledger
//! a USD stock reads `EUR.USD` and inverts it. The direction lives in the TYPE and in the golden tests'
//! names, and [`quote_rate`] is the only function that chooses it.
//!
//! Which of the two orders IBKR lists is asked, not assumed: the market convention (EUR, GBP, AUD, NZD, USD,
//! CAD, CHF, CNH, JPY, then the rest) is tried first, and a pair IB answers "no security definition" (error
//! 200, <https://interactivebrokers.github.io/tws-api/message_codes.html>) for is tried the other way round.
//! Neither listed is a refusal in words, never a guess. ⚠ The convention list is the author's recollection of
//! IDEALPRO's pair list, not a document; the fallback is what makes a wrong recollection cost a request.
//!
//! # Fail closed
//!
//! Only a REAL-TIME, two-sided, uncrossed, narrow quote is a rate. A delayed or frozen one (IB serves delayed
//! data to an account with no real-time entitlement, <https://interactivebrokers.github.io/tws-api/market_data_type.html>),
//! a missing side, a zero or negative or non-finite price, a crossed book and a spread past
//! [`MAX_SPREAD`] are each an [`IbkrPropertiesError::Unavailable`] saying which, and there is no path that answers
//! `1.0` for "I do not know". The rate is stamped with the moment the quote was ASKED for, the conservative
//! side: it is no newer than that.
//!
//! # Minor-unit currencies
//!
//! IBKR is known to quote some exchanges' stocks in the currency's MINOR unit (London in pence, Tel Aviv in
//! agorot, Johannesburg in cents) while the contract says `GBP`/`ILS`/`ZAR`. A rate for the major unit would
//! then value such a stock 100 times too high. Nothing here has measured which listings do, so
//! [`minor_unit_refusal`] refuses every major of `vike_model::MINOR_UNITS` (those three) by name.
//!
//! ⚠ DOCS-SHAPED: nothing here has run against a gateway. The tick types, the contract shape and the error
//! codes are IBKR's documentation (and `ibapi`'s), not a capture.

/// The socket reads behind [`crate::account::quote_rate`]: a forex pair's top of book and the account's base currency, over the
/// session's one data connection.
#[cfg(feature = "ibkr-socket")]
pub(crate) mod socket;

use std::time::Duration;

use vike_marketdata::TickQuote;
use vike_model::{
    AccountPerQuote, AccountPerQuoteLeg, QuoteRate, QuoteRateError, StreamDelay, currency_code,
};

use crate::instrument::any_symbol::IbkrPropertiesError;
use crate::refusals::IbkrSymbolRefusal;

/// The widest relative spread, `(ask - bid) / mid`, a forex quote may have and still be a rate. Majors trade
/// within a thousandth of a percent; a spread of a whole percent is a market that is not there (a weekend
/// open, a halted pair), and a mid of it would be a guess.
pub const MAX_SPREAD: f64 = 0.01;

/// The pairs IBKR lists for two currencies are written in the market's order: a currency earlier in this list
/// is the BASE (`EUR.USD`, `GBP.USD`, `USD.JPY`); one not in it comes after all of them.
const CONVENTION: &[&str] = &["EUR", "GBP", "AUD", "NZD", "USD", "CAD", "CHF", "CNH", "JPY"];

/// The one question this module asks the gateway: the top of book of the pair `base.quote` on IDEALPRO. The
/// production impl is `crate::account::quote_rate::socket::FxQuotesSocket`; every test scripts it.
pub trait FxQuotes {
    /// The top of book of the pair whose BASE currency is `base` and whose QUOTE currency is `quote` (bid and ask
    /// in quote units per base unit), and the label of the stream the two prices arrived on
    /// (`QuoteAccumulator::delay`), which [`TickQuote`] has no field for.
    ///
    /// # Errors
    /// [`QuoteRateError::NoPair`] when IB answered "no security definition" (error 200): this ORDER of the two
    /// currencies is not a listed pair. Any other error is a read that failed or timed out, in words.
    fn book(&self, base: &str, quote: &str) -> Result<(TickQuote, StreamDelay), QuoteRateError>;
}

fn rank(currency: &str) -> usize {
    CONVENTION.iter().position(|c| *c == currency).unwrap_or(CONVENTION.len())
}

/// The `(base, quote)` order the market writes two currencies in, which is the order IBKR is asked first.
#[must_use]
pub fn conventional_pair(a: &str, b: &str) -> (String, String) {
    if (rank(a), a) <= (rank(b), b) {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Why a stock quoted in `currency` is not admitted because its price may be in the currency's MINOR unit, or
/// `None`. Which currencies have a minor unit is `vike_model::MINOR_UNITS`; the listing words are IBKR's. See the
/// module doc: refused, not corrected, because which listings are quoted so is not measured.
#[must_use]
pub fn minor_unit_refusal(currency: &str) -> Option<IbkrSymbolRefusal> {
    let unit = vike_model::MinorUnit::of_major(currency)?;
    let currency = unit.major;
    let (listing, minor, major) = match currency {
        "GBP" => ("London", "pence (GBX)", "pounds"),
        "ILS" => ("Tel Aviv", "agorot", "shekels"),
        "ZAR" => ("Johannesburg", "cents (ZAc)", "rand"),
        // A minor unit the table gains is refused too, named by its codes.
        _ => ("some exchanges'", unit.minor, unit.major),
    };
    Some(IbkrSymbolRefusal::MinorUnitCurrency {
        listing: listing.to_string(),
        minor: minor.to_string(),
        currency: currency.to_string(),
        major: major.to_string(),
    })
}

fn refused(why: IbkrSymbolRefusal) -> IbkrPropertiesError {
    IbkrPropertiesError::Refused(why)
}

fn unavailable(why: IbkrSymbolRefusal) -> IbkrPropertiesError {
    IbkrPropertiesError::Unavailable(why)
}

/// The mid of `quote` (the pair `pair` as listed, on a stream labelled `delay`), or why it is not a rate.
fn mid_of(quote: &TickQuote, delay: StreamDelay, pair: &str) -> Result<f64, IbkrPropertiesError> {
    let pair_of = || pair.to_string();
    if !delay.is_real_time() {
        return Err(unavailable(IbkrSymbolRefusal::QuoteNotRealTime {
            pair: pair_of(),
            delay: delay.word().to_string(),
        }));
    }
    let (bid, ask) = (quote.bid, quote.ask);
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0) {
        return Err(unavailable(IbkrSymbolRefusal::QuoteNotTwoSided { pair: pair_of(), bid, ask }));
    }
    if ask < bid {
        return Err(unavailable(IbkrSymbolRefusal::BookCrossed { pair: pair_of(), bid, ask }));
    }
    let mid = quote.mid();
    if (ask - bid) / mid > MAX_SPREAD {
        return Err(unavailable(IbkrSymbolRefusal::SpreadTooWide {
            pair: pair_of(),
            bid,
            ask,
            percent: MAX_SPREAD * 100.0,
        }));
    }
    Ok(mid)
}

/// **The rate for a stock quoted in `quote_ccy` on a ledger in `account_ccy`**, read through `fx`, stamped
/// `asked_at_ms`. Ledger units per ONE quote unit (module doc): the pair `QUOTE.ACCOUNT` is read as it stands,
/// the pair `ACCOUNT.QUOTE` is inverted. Beside the rate (its `as_of_ms` is the moment the quote was ASKED for:
/// the rate is no newer than that), the IBKR pair its mid was read from, `BASE.QUOTE` as listed (`USD.JPY`): for
/// a log line, the sanity gate's words and a test.
///
/// # Errors
/// [`IbkrPropertiesError::Refused`] for an empty code (`vike_model::currency_code`), two currencies that are one, and a pair IBKR
/// lists in neither order; [`IbkrPropertiesError::Unavailable`] for a read that failed and a quote that is not a rate.
pub fn quote_rate(
    fx: &dyn FxQuotes,
    account_ccy: &str,
    quote_ccy: &str,
    asked_at_ms: i64,
) -> Result<(QuoteRate, String), IbkrPropertiesError> {
    let not_a_code =
        |code: &str| refused(IbkrSymbolRefusal::RateCodeNotACurrency { code: code.to_string() });
    let account = currency_code(account_ccy).ok_or_else(|| not_a_code(account_ccy))?;
    let quote = currency_code(quote_ccy).ok_or_else(|| not_a_code(quote_ccy))?;
    if account == quote {
        return Err(refused(IbkrSymbolRefusal::RateOfTheLedgersOwn { currency: quote }));
    }
    let (base, counter) = conventional_pair(&quote, &account);
    let read = |base: &str, counter: &str| {
        fx.book(base, counter).map_err(|fail| match fail {
            QuoteRateError::NoPair(_) => None,
            QuoteRateError::Unavailable(why) | QuoteRateError::BadQuote(why) => {
                Some(IbkrSymbolRefusal::PairReadFailed {
                    base: base.to_string(),
                    counter: counter.to_string(),
                    why,
                })
            }
        })
    };
    let (base, counter, (tick, delay)) = match read(&base, &counter) {
        Ok(book) => (base, counter, book),
        Err(Some(failed)) => return Err(unavailable(failed)),
        Err(None) => match read(&counter, &base) {
            Ok(book) => (counter, base, book),
            Err(Some(failed)) => return Err(unavailable(failed)),
            Err(None) => {
                return Err(refused(IbkrSymbolRefusal::NoForexPair {
                    quote,
                    account,
                    base,
                    counter,
                }));
            }
        },
    };
    let pair = format!("{base}.{counter}");
    let mid = mid_of(&tick, delay, &pair)?;
    // The pair prices ONE `base` in `counter` units, so which of the two is the quote fixes the reading.
    let leg = if base == quote {
        AccountPerQuoteLeg::QuoteIsBase
    } else {
        AccountPerQuoteLeg::AccountIsBase
    };
    let account_per_quote = AccountPerQuote::of_mid(leg, mid).map_err(|per_quote| {
        unavailable(IbkrSymbolRefusal::MidGivesNoRate { pair: pair.clone(), mid, per_quote })
    })?;
    Ok((QuoteRate { quote_currency: quote, account_per_quote, as_of_ms: asked_at_ms }, pair))
}

/// **The sanity gate on a fresh rate** (vendor-doc finding V1: IBKR's TWS API pages never state which way a forex
/// pair is priced, and a flip is a 22,500x error on a yen). A fresh rate of a currency is compared with the
/// rate of the SAME currency, on the SAME base, this source accepted before (`previous`, accepted `previous_age`
/// ago; each rate with the pair it was read from, as [`quote_rate`] answers it): it is refused when it moved by more than the ENGINE'S OWN step ([`vike_execution::MAX_QUOTE_RATE_STEP`], not a
/// band invented here) in one read, which a real market does not do between two reads minutes apart and a
/// flipped pair, a stale frozen tick or an inverted request always does (`1/150` against `150` is 22,500x).
///
/// A reference older than the engine's staleness window ([`vike_execution::DEFAULT_QUOTE_RATE_TTL_MS`]) is not compared:
/// after that long a real move is possible, nothing locks the source out, and the engine's step guard (which
/// stays authoritative, with its own confirm-by-a-second-offer rule) judges the rate against what it holds.
///
/// ⚠ What it cannot catch: a pair flipped from the FIRST read on. Nothing in this process knows the direction
/// before the first read; the goldens pin it, and the drill (`tests/ibkr_smokes/ibkr_fx_smoke.rs`) prints every pair as
/// listed beside the rate for a person to compare with TWS.
///
/// # Errors
/// [`IbkrPropertiesError::Unavailable`] saying the two rates and the step.
pub fn check_against_previous(
    previous: &(QuoteRate, String),
    previous_age: Duration,
    fresh: &(QuoteRate, String),
) -> Result<(), IbkrPropertiesError> {
    let ((previous, previous_pair), (fresh, fresh_pair)) = (previous, fresh);
    let window = Duration::from_millis(
        u64::try_from(vike_execution::DEFAULT_QUOTE_RATE_TTL_MS).unwrap_or(0),
    );
    if previous_age > window || previous.quote_currency != fresh.quote_currency {
        return Ok(());
    }
    let (was, now) = (previous.account_per_quote.get(), fresh.account_per_quote.get());
    let step = if now >= was { now / was } else { was / now };
    if step <= vike_execution::MAX_QUOTE_RATE_STEP {
        return Ok(());
    }
    Err(unavailable(IbkrSymbolRefusal::RateMovedTooFar {
        pair: fresh_pair.clone(),
        now,
        currency: fresh.quote_currency.clone(),
        age_secs: previous_age.as_secs(),
        was,
        previous_pair: previous_pair.clone(),
        step,
    }))
}

#[path = "quote_rate_tests.rs"]
#[cfg(test)]
mod quote_rate_tests;
