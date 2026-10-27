//! The IBKR RATE SOURCE for the engine's multi-currency money (decision 0183, phase 1): what one unit of a
//! stock's QUOTE currency is worth in the ledger's own currency, read from a LIVE forex quote of the gateway.
//!
//! IBKR states no conversion factor the way OANDA's `homeConversions` does. The account's ledger has an
//! `ExchangeRate` key (per currency, against the base): IBKR's account-value-keys page lists it ("The
//! exchange rate of the currency to your base currency", `https://www.interactivebrokers.com/docs/tws-api/doc/
//! account-portfolio-data/account-updates/account-value-keys`) but states no DIRECTION (which currency is
//! the numerator), and that page is for `reqAccountUpdates`, not for an account-summary `$LEDGER` row. So it
//! is NOT used; `tests/ibkr_fx_smoke.rs` prints it beside the market mid so its direction can be read off a
//! gateway. The source is the market: the bid and ask of the IDEALPRO forex pair between the two
//! currencies, on the data connection every other IBKR consumer shares (`crate::data_session`), averaged
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
//! [`MAX_SPREAD`] are each an [`FactsError::Unavailable`] saying which, and there is no path that answers
//! `1.0` for "I do not know". The rate is stamped with the moment the quote was ASKED for, the conservative
//! side: it is no newer than that.
//!
//! # Minor-unit currencies
//!
//! IBKR is known to quote some exchanges' stocks in the currency's MINOR unit (London in pence, Tel Aviv in
//! agorot, Johannesburg in cents) while the contract says `GBP`/`ILS`/`ZAR`. A rate for the major unit would
//! then value such a stock 100 times too high. Nothing here has measured which listings do, so
//! [`minor_unit_refusal`] refuses those three currencies by name.
//!
//! ⚠ DOCS-SHAPED: nothing here has run against a gateway. The tick types, the contract shape and the error
//! codes are IBKR's documentation (and `ibapi`'s), not a capture.

use std::time::Duration;

use vike_model::DataDelay;

use crate::any_symbol::FactsError;

/// The widest relative spread, `(ask - bid) / mid`, a forex quote may have and still be a rate. Majors trade
/// within a thousandth of a percent; a spread of a whole percent is a market that is not there (a weekend
/// open, a halted pair), and a mid of it would be a guess.
pub const MAX_SPREAD: f64 = 0.01;

/// The pairs IBKR lists for two currencies are written in the market's order: a currency earlier in this list
/// is the BASE (`EUR.USD`, `GBP.USD`, `USD.JPY`); one not in it comes after all of them.
const CONVENTION: &[&str] = &["EUR", "GBP", "AUD", "NZD", "USD", "CAD", "CHF", "CNH", "JPY"];

/// **Ledger-currency units per ONE unit of a quote currency.** Multiply a quote-currency amount by it
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

    /// The factor: ledger-currency units per one quote-currency unit.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }

    /// A quote-currency amount in the ledger's currency (`quote_amount * factor`).
    #[must_use]
    pub fn to_account(self, quote_amount: f64) -> f64 {
        quote_amount * self.0
    }
}

/// What the node is handed for ONE quote currency: the rate, when it was asked and from which pair.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRate {
    /// The quote currency (upper case, three letters).
    pub quote_ccy: String,
    /// Ledger-currency units per one `quote_ccy` unit.
    pub account_per_quote: AccountPerQuote,
    /// When the quote was ASKED for, epoch ms: the rate is no newer than this.
    pub as_of_ms: i64,
    /// The IBKR pair the mid was read from, `BASE.QUOTE` as listed (`USD.JPY`): for a log line and a test.
    pub pair: String,
}

/// One read of a forex pair's top of book.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FxBook {
    /// The best bid, in quote units per base unit.
    pub bid: f64,
    /// The best ask, in quote units per base unit.
    pub ask: f64,
    /// The label of the stream the two prices arrived on (`QuoteAccumulator::delay`).
    pub delay: DataDelay,
}

/// Why a pair could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FxFail {
    /// IB answered "no security definition" (error 200): this ORDER of the two currencies is not a listed pair.
    NoSuchPair,
    /// The read failed or timed out; the reason, in words.
    Unavailable(String),
}

/// The one question this module asks the gateway: the top of book of the pair `base.quote` on IDEALPRO. The
/// production impl is `crate::fx_socket::SocketFx`; every test scripts it.
pub trait FxQuotes {
    /// The top of book of the pair whose BASE currency is `base` and whose QUOTE currency is `quote`.
    fn book(&self, base: &str, quote: &str) -> Result<FxBook, FxFail>;
}

fn rank(ccy: &str) -> usize {
    CONVENTION.iter().position(|c| *c == ccy).unwrap_or(CONVENTION.len())
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

/// `ccy` as an upper-case ISO code, or `None` unless it is exactly three ASCII letters.
fn code(ccy: &str) -> Option<String> {
    let ccy = ccy.trim();
    (ccy.len() == 3 && ccy.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| ccy.to_ascii_uppercase())
}

/// Why a stock quoted in `ccy` is not admitted because its price may be in the currency's MINOR unit, or
/// `None`. See the module doc: refused, not corrected, because which listings are quoted so is not measured.
#[must_use]
pub fn minor_unit_refusal(ccy: &str) -> Option<String> {
    let ccy = ccy.trim().to_ascii_uppercase();
    let (listing, minor, major) = match ccy.as_str() {
        "GBP" => ("London", "pence (GBX)", "pounds"),
        "ILS" => ("Tel Aviv", "agorot", "shekels"),
        "ZAR" => ("Johannesburg", "cents (ZAc)", "rand"),
        _ => return None,
    };
    Some(format!(
        "IBKR is known to quote {listing} stocks in {minor} while the contract says {ccy}, and which \
         listings and whether this adapter's prices are in that minor unit is not measured: a rate for \
         {major} would value such a stock 100 times too high, so a {ccy} stock is not admitted"
    ))
}

fn refused(why: String) -> FactsError {
    FactsError::Refused(why)
}

fn unavailable(why: String) -> FactsError {
    FactsError::Unavailable(why)
}

/// The mid of `book` (the pair `pair` as listed), or why it is not a rate.
fn mid_of(book: &FxBook, pair: &str) -> Result<f64, FactsError> {
    if !book.delay.is_real_time() {
        return Err(unavailable(format!(
            "the {pair} quote is {} data, not real-time, so it is not a rate",
            book.delay.word()
        )));
    }
    let (bid, ask) = (book.bid, book.ask);
    if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0) {
        return Err(unavailable(format!(
            "the {pair} quote has no usable two-sided price (bid {bid}, ask {ask})"
        )));
    }
    if ask < bid {
        return Err(unavailable(format!("the {pair} book is crossed (bid {bid} above ask {ask})")));
    }
    let mid = (bid + ask) / 2.0;
    if (ask - bid) / mid > MAX_SPREAD {
        return Err(unavailable(format!(
            "the {pair} spread ({bid} / {ask}) is wider than {}% of the mid, which is not a market",
            MAX_SPREAD * 100.0
        )));
    }
    Ok(mid)
}

/// **The rate for a stock quoted in `quote_ccy` on a ledger in `account_ccy`**, read through `fx`, stamped
/// `asked_at_ms`. Ledger units per ONE quote unit (module doc): the pair `QUOTE.ACCOUNT` is read as it stands,
/// the pair `ACCOUNT.QUOTE` is inverted.
///
/// # Errors
/// [`FactsError::Refused`] for a code that is not three letters, two currencies that are one, and a pair IBKR
/// lists in neither order; [`FactsError::Unavailable`] for a read that failed and a quote that is not a rate.
pub fn quote_rate(
    fx: &dyn FxQuotes,
    account_ccy: &str,
    quote_ccy: &str,
    asked_at_ms: i64,
) -> Result<QuoteRate, FactsError> {
    let account = code(account_ccy).ok_or_else(|| {
        refused(format!("`{account_ccy}` is not a currency code (three letters)"))
    })?;
    let quote = code(quote_ccy)
        .ok_or_else(|| refused(format!("`{quote_ccy}` is not a currency code (three letters)")))?;
    if account == quote {
        return Err(refused(format!("{quote} is the ledger's own currency: nothing converts")));
    }
    let (base, counter) = conventional_pair(&quote, &account);
    let read = |base: &str, counter: &str| {
        fx.book(base, counter).map_err(|fail| match fail {
            FxFail::NoSuchPair => None,
            FxFail::Unavailable(why) => Some(unavailable(format!("{base}.{counter}: {why}"))),
        })
    };
    let (base, counter, book) = match read(&base, &counter) {
        Ok(book) => (base, counter, book),
        Err(Some(failed)) => return Err(failed),
        Err(None) => match read(&counter, &base) {
            Ok(book) => (counter, base, book),
            Err(Some(failed)) => return Err(failed),
            Err(None) => {
                return Err(refused(format!(
                    "IBKR lists no forex pair between {quote} and {account} (neither {base}.{counter} \
                     nor {counter}.{base}), so there is no rate to convert {quote} into {account}"
                )));
            }
        },
    };
    let pair = format!("{base}.{counter}");
    let mid = mid_of(&book, &pair)?;
    // The pair prices ONE `base` in `counter` units. Quote as the base: that IS the ledger per quote unit.
    // Ledger as the base: the pair is quote units per ledger unit, and the rate is its inverse.
    let per_quote = if base == quote { mid } else { 1.0 / mid };
    let account_per_quote = AccountPerQuote::new(per_quote).ok_or_else(|| {
        unavailable(format!("the {pair} mid {mid} gives no usable rate ({per_quote})"))
    })?;
    Ok(QuoteRate { quote_ccy: quote, account_per_quote, as_of_ms: asked_at_ms, pair })
}

/// **The sanity gate on a fresh rate** (vendor-doc finding V1: IBKR's TWS API pages never state which way a forex
/// pair is priced, and a flip is a 22,500x error on a yen). A fresh rate of a currency is compared with the
/// rate of the SAME currency, on the SAME base, this source accepted before (`previous`, accepted `previous_age`
/// ago): it is refused when it moved by more than the ENGINE'S OWN step ([`vike_exec::MAX_QUOTE_RATE_STEP`], not a
/// band invented here) in one read, which a real market does not do between two reads minutes apart and a
/// flipped pair, a stale frozen tick or an inverted request always does (`1/150` against `150` is 22,500x).
///
/// A reference older than the engine's staleness window ([`vike_exec::DEFAULT_QUOTE_RATE_TTL_MS`]) is not compared:
/// after that long a real move is possible, nothing locks the source out, and the engine's step guard (which
/// stays authoritative, with its own confirm-by-a-second-offer rule) judges the rate against what it holds.
///
/// ⚠ What it cannot catch: a pair flipped from the FIRST read on. Nothing in this process knows the direction
/// before the first read; the goldens pin it, and the drill (`tests/ibkr_fx_smoke.rs`) prints every pair as
/// listed beside the rate for a person to compare with TWS.
///
/// # Errors
/// [`FactsError::Unavailable`] saying the two rates and the step.
pub fn check_against_previous(
    previous: &QuoteRate,
    previous_age: Duration,
    fresh: &QuoteRate,
) -> Result<(), FactsError> {
    let window =
        Duration::from_millis(u64::try_from(vike_exec::DEFAULT_QUOTE_RATE_TTL_MS).unwrap_or(0));
    if previous_age > window || previous.quote_ccy != fresh.quote_ccy {
        return Ok(());
    }
    let (was, now) = (previous.account_per_quote.get(), fresh.account_per_quote.get());
    let step = if now >= was { now / was } else { was / now };
    if step <= vike_exec::MAX_QUOTE_RATE_STEP {
        return Ok(());
    }
    Err(unavailable(format!(
        "the {} read gives {now} ledger units per {} where the read {} s ago gave {was} ({}): a move of {step:.1} \
         times in one read is past the engine's {}x step, so it is a flipped or frozen quote and not a rate",
        fresh.pair,
        fresh.quote_ccy,
        previous_age.as_secs(),
        previous.pair,
        vike_exec::MAX_QUOTE_RATE_STEP
    )))
}

#[path = "fx_rate_tests.rs"]
#[cfg(test)]
mod fx_rate_tests;
