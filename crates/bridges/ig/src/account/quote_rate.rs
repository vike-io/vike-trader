//! The IG RATE SOURCE for the engine's multi-currency money (decision 0183): what ONE unit of an epic's QUOTE
//! currency is worth in the ACCOUNT's currency, taken from the market details the facts already read, and
//! CHECKED against a price before it is believed. Pure: the venue is a closure, the clock a parameter.
//!
//! # The orientation, which is the whole risk
//!
//! The rate is **account-currency units per ONE unit of the quote currency** (`account_per_quote`): a quote
//! amount is MULTIPLIED by it. On a USD account USDJPY at 150 gives JPY `0.00667` (a fraction of a dollar per
//! yen), never `150`; EURGBP gives GBP `1.35`. The flipped rate is a 22,500x error on USDJPY.
//!
//! # What IG states, and why that is not trusted alone
//!
//! `GET /markets/{epic}` (v3) carries `instrument.currencies[]`: `code`, `symbol`, `baseExchangeRate`,
//! `exchangeRate`, `isDefault`. IG's reference (<https://labs.ig.com/reference/markets-epic.html>, read
//! 2026-10-19; the `rest-trading-api-reference` pages answered HTTP 500) describes them in four words:
//! `baseExchangeRate` "Base exchange rate", `exchangeRate` "Exchange rate.", `code` "Code, to be used when
//! placing orders", `isDefault` "True if this is the default currency". **It states no orientation, no
//! timestamp, no difference between the two rates and no row that is "the account's"**, and no body was ever
//! captured from the demo gateway (decision 0183: "IG `currencies` rates ... not verified"). So WHICH row and
//! WHICH direction carry the account-per-quote rate is a hypothesis, and the hypothesis is enforced on every
//! read instead of trusted. The `exchangeRate` column is read four ways ([`QuoteRateReading`]), each a coherent table:
//!
//! | reading | the rate | the table must also say |
//! |---|---|---|
//! | [`QuoteRateReading::QuoteRow`] | the QUOTE currency's row | the account currency's row says 1 |
//! | [`QuoteRateReading::QuoteRowInverse`] | 1 / the quote currency's row | the account currency's row says 1 |
//! | [`QuoteRateReading::AccountRow`] | the ACCOUNT currency's row | the quote currency's row says 1 (or is absent) |
//! | [`QuoteRateReading::AccountRowInverse`] | 1 / the account currency's row | the quote currency's row says 1 (or is absent) |
//!
//! and a reading is believed only if the rate it gives agrees, within [`RATE_TOLERANCE`], with a PRICE-derived
//! rate of the same currency: the mid of the epic's own `snapshot` when the pair's BASE is the account
//! currency (USDJPY on USD: `1/mid`), else the mid of a conversion pair read from the same gateway (EURGBP on
//! USD: GBPUSD, `mid`), its orientation taken from ITS `onePipMeans`, never from its name. A flipped rate is
//! off by far more than 2%, so **a rate that passes is within 2% of a market price whatever IG meant by the
//! word**; when none of the four does, the epic is refused and the refusal prints every candidate beside the
//! price, which is how the first live read settles the reading (and how this table is narrowed to the one
//! proven: the [`QuoteRateReading`] [`derive`] answers beside the rate names the one that held).
//!
//! Two more refusals close the cases where the price cannot tell the readings apart. **Par is never a rate**: a
//! row that would BE the rate and itself says 1 is the identity row, which states nothing about the quote
//! currency (a table naming only the account's own row at 1, or both rows at 1, is refused, not valued at 1.0
//! because the price happens to sit near 1). **Readings that agree with the price but not with each other are a
//! guess** (a rate near 1.0 agrees with its own reciprocal to within 2%: a CHF row at 1.015 and its inverse 0.985
//! both pass a price of 1.000): the epic is refused unless they differ by less than [`AMBIGUITY_MARGIN`], and the
//! order they are tried in decides nothing.
//!
//! # The `baseExchangeRate` column (vendor-doc review, 2026-10-19)
//!
//! Three public captures of this table (a 2020 `GET /markets?epics=` of ~50 epics, an R package's 2025 mock, a
//! Python library's test data) agree on ONE inference the reference does not state: `baseExchangeRate` is units
//! of the ROW's currency per ONE unit of the account (default) currency, the account's own row says 1.0 with
//! `isDefault` true, and **that column is market-consistent** (JPY 108.036 beside a USDJPY mid of 108.09) where
//! the `exchangeRate` column of the 2020 capture is coarse (JPY 0.007 against a true 0.00925, the account's own
//! row 0.66 instead of 1). So the base column is read too, both ways ([`QuoteRateReading::BaseRowInverse`], the inferred
//! one, and [`QuoteRateReading::BaseRow`]), and held to the same price and the same ambiguity refusal as every other
//! reading: a coarse `exchangeRate` that is 24% off the price is refused, never accepted within 2%.
//!
//! **An epic that does not list the account's currency at all** (the same captures: USDJPY lists only JPY) has no
//! `exchangeRate` readings, and the base column may be absent too. Then the rate is the PRICE itself: the mid of
//! the epic's own snapshot when its base is the account currency, else the conversion pair's
//! ([`QuoteRateReading::PriceOnly`]). With no table to check the price against, the price must itself be believable as a
//! price: its snapshot must say its levels are price units (`scalingFactor` x the pip = 1), see [`price_units`].
//!
//! The rate is stamped with the moment the market details were ASKED (IG states none): the conservative side,
//! the rate is no newer than the request. The node re-reads it about once a minute while a foreign-quoted
//! position is held (`TradeLeaseRegistry::spawn_rate_refresher`).
//!
//! # Fail closed
//!
//! No usable rate is [`IgPropertiesError::NoRate`] (the symbol is not admitted this time, nothing is remembered about
//! the symbol itself) and a read that failed is [`IgPropertiesError::Unavailable`]. There is no path that answers `1.0`
//! for "I do not know".

use serde_json::Value;
use vike_bridge_core::json::json_number;
use vike_model::{AccountPerQuote, AccountPerQuoteLeg, QuoteRate};

use crate::instrument::{IgPropertiesError, Pair, pip_and_pair};
use crate::refusals::{IgRateRefusal, IgSymbolRefusal};

/// `1 / x` by the ONE orientation rule (`AccountPerQuote::of_mid`, an `ACCOUNT/QUOTE` figure), handed back as the
/// figure even where it is no rate: every reading is judged later, in this module's own words.
fn inverse(x: f64) -> f64 {
    AccountPerQuote::of_mid(AccountPerQuoteLeg::AccountIsBase, x)
        .map_or_else(|figure| figure, AccountPerQuote::get)
}

/// How far a stated rate may sit from the price-derived one (relative to the price-derived one): wide enough
/// for a conversion margin of the size brokers charge (a fraction of a percent), tight enough that a flipped
/// rate (off by `r*r - 1`: 22,500x for USDJPY, still 1.8x for a pound) or another column is refused.
pub const RATE_TOLERANCE: f64 = 0.02;

/// How far the identity row of a table may sit from 1.
const IDENTITY_TOLERANCE: f64 = 1e-4;

/// How far two readings that BOTH agree with the price may sit from each other before the choice between them
/// is a guess (relative): about the size of a conversion margin. A rate near 1.0 agrees with its own reciprocal
/// to within 2% (a CHF row at 1.015 and its inverse 0.985 both sit within 2% of a price of 1.000), and which of
/// the two IG meant is exactly what the table does not say; two readings that differ by less than this are the
/// same number for money's sake and the first is taken.
pub(crate) const AMBIGUITY_MARGIN: f64 = 0.005;

/// Which row and direction of `currencies[].exchangeRate` gave the rate (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteRateReading {
    /// The quote currency's row IS account-per-quote; the account currency's row says 1.
    QuoteRow,
    /// The quote currency's row is quote-per-account (the rate is its reciprocal); the account's row says 1.
    QuoteRowInverse,
    /// The account currency's row IS account-per-quote; the quote currency's row says 1 (or is not listed).
    AccountRow,
    /// The account currency's row is quote-per-account (the rate is its reciprocal); the quote's says 1.
    AccountRowInverse,
    /// `baseExchangeRate` of the QUOTE currency's row IS account-per-quote (the opposite of what three public
    /// captures fit); the account's row, when listed, says 1.
    BaseRow,
    /// `baseExchangeRate` of the QUOTE currency's row is quote-per-account (the rate is its reciprocal): the
    /// reading three public captures fit; the account's row, when listed, says 1.
    BaseRowInverse,
    /// The table states nothing about the account currency (its row is not listed and no base column reads), so
    /// the rate is the market price itself, which must say its levels are price units (`scalingFactor` x pip = 1).
    PriceOnly,
}

impl QuoteRateReading {
    /// The words a log line and a refusal name it by.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            QuoteRateReading::QuoteRow => "the quote currency's row, as stated",
            QuoteRateReading::QuoteRowInverse => "the quote currency's row, inverted",
            QuoteRateReading::AccountRow => "the account currency's row, as stated",
            QuoteRateReading::AccountRowInverse => "the account currency's row, inverted",
            QuoteRateReading::BaseRow => "the quote currency's baseExchangeRate, as stated",
            QuoteRateReading::BaseRowInverse => "the quote currency's baseExchangeRate, inverted",
            QuoteRateReading::PriceOnly => {
                "the market price alone (the table lists no row for the account currency)"
            }
        }
    }
}

fn no_rate(refusal: IgRateRefusal) -> IgPropertiesError {
    IgPropertiesError::NoRate(Box::new(refusal))
}

/// A row of `instrument.currencies[]` by `code`: `None` = no such row, `Some(None)` = a row with no usable
/// `exchangeRate` (missing, zero, negative, non-finite or not a number), `Some(Some(x))` = its rate.
fn row_rate(instrument: &Value, code: &str) -> Option<Option<f64>> {
    row_column(instrument, code, "exchangeRate")
}

/// [`row_rate`] for the `baseExchangeRate` column.
fn row_base(instrument: &Value, code: &str) -> Option<Option<f64>> {
    row_column(instrument, code, "baseExchangeRate")
}

fn row_column(instrument: &Value, code: &str, key: &str) -> Option<Option<f64>> {
    let rows = instrument.get("currencies").and_then(Value::as_array)?;
    let row = rows.iter().find(|c| c.get("code").and_then(Value::as_str) == Some(code))?;
    Some(row.get(key).and_then(json_number).filter(|x| x.is_finite() && *x > 0.0))
}

fn is_one(x: f64) -> bool {
    (x - 1.0).abs() <= IDENTITY_TOLERANCE
}

/// Every coherent reading of IG's table for `quote` in `account` and the rate each gives (module doc table),
/// in the order they are tried: the `exchangeRate` column's four, then the `baseExchangeRate` column's two.
///
/// **Empty only when the table lists no row for the account currency and the base column reads nothing**: the
/// table then says nothing about the account currency and the caller derives the rate from the price alone
/// ([`QuoteRateReading::PriceOnly`]). An account row that is listed beside a table nothing can be read from is `NoRate`.
fn candidates(
    epic: &str,
    instrument: &Value,
    quote: &str,
    account: &str,
) -> Result<Vec<(QuoteRateReading, f64)>, IgPropertiesError> {
    let own_row = row_rate(instrument, account);
    let theirs = row_rate(instrument, quote);
    let mut out = Vec::new();
    // ⚠ A row that is the RATE must not itself say 1: a row at 1 is the identity, and read as a rate it is PAR,
    // which a price within 2% of 1.0 would then "confirm" (a table naming only the account's own row at 1, or
    // both rows at 1, states nothing about the quote currency). Par is never a rate here.
    if let Some(own) = own_row {
        // The account's own row says 1: the table is "rates into the account currency", the quote's row is the
        // rate.
        if own.is_some_and(is_one)
            && let Some(Some(x)) = theirs
            && !is_one(x)
        {
            out.push((QuoteRateReading::QuoteRow, x));
            out.push((QuoteRateReading::QuoteRowInverse, inverse(x)));
        }
        // The quote's own row says 1 (or is not listed): the table is "rates out of the quote currency", the
        // account's row is the rate.
        if theirs.is_none_or(|row| row.is_some_and(is_one))
            && let Some(x) = own
            && !is_one(x)
        {
            out.push((QuoteRateReading::AccountRow, x));
            out.push((QuoteRateReading::AccountRowInverse, inverse(x)));
        }
    }
    // The base column: the account's row, when it is listed, says 1 (three captures: it does, with `isDefault`
    // true), and the quote's row is the rate or its reciprocal. Par is no rate here either.
    let (base_own, base_theirs) = (row_base(instrument, account), row_base(instrument, quote));
    if base_own.is_none_or(|row| row.is_some_and(is_one))
        && let Some(Some(x)) = base_theirs
        && !is_one(x)
    {
        out.push((QuoteRateReading::BaseRowInverse, inverse(x)));
        out.push((QuoteRateReading::BaseRow, x));
    }
    if out.is_empty() && own_row.is_some() {
        return Err(no_rate(IgRateRefusal::NoCoherentTable {
            epic: epic.to_string(),
            quote: quote.to_string(),
            account: account.to_string(),
            own: own_row.flatten(),
            theirs,
            base_own,
            base_theirs,
        }));
    }
    Ok(out)
}

/// The mid of a market body's own two-sided price: `TRADEABLE`, finite, positive, not crossed. IG's levels are
/// read as pair units; a body whose levels are scaled (`snapshot.scalingFactor`) fails the agreement check
/// against the stated rate and is refused there, never rescaled here on a guess.
fn mid(market: &Value) -> Result<f64, String> {
    let status = market.pointer("/snapshot/marketStatus").and_then(Value::as_str).unwrap_or("");
    if status != "TRADEABLE" {
        return Err(format!("its market status is `{status}`, not TRADEABLE"));
    }
    let side = |key: &str| {
        market
            .pointer(&format!("/snapshot/{key}"))
            .and_then(json_number)
            .filter(|x| x.is_finite() && *x > 0.0)
    };
    match (side("bid"), side("offer")) {
        (Some(bid), Some(offer)) if bid <= offer => Ok((bid + offer) / 2.0),
        (Some(_), Some(_)) => Err("its bid is above its offer".to_string()),
        _ => Err("it carries no two-sided price".to_string()),
    }
}

/// **Whether a body's snapshot levels are PRICE UNITS** (the rate is the price itself only when they are): its
/// `snapshot.scalingFactor` times its pip (`onePipMeans`) must be 1. IG documents `scalingFactor` as a "multiplying
/// factor to determine actual pip value for the levels used by the instrument" and ties it to nothing else; the
/// captures that exist fit `price = level x scalingFactor x pip` (a 2020 USDJPY level of 10806.1 beside
/// `scalingFactor` 1 and a pip of 0.01 is 108.061; a 2025 USDCHF level of 0.79715 beside `scalingFactor` 10000 and
/// a pip of 0.0001 is itself). This is only a CHECK: a level that is not already a price is REFUSED, never rescaled
/// by that guess, and the absent field is a refusal too. Used where no table stands behind the price
/// ([`QuoteRateReading::PriceOnly`]); with a table, a scaled level disagrees with the stated rate and is refused there.
fn price_units(body: &Value) -> Result<(), String> {
    let pip = body
        .pointer("/instrument/onePipMeans")
        .and_then(Value::as_str)
        .and_then(pip_and_pair)
        .map(|(pip, _, _)| pip)
        .ok_or_else(|| {
            "its onePipMeans is unreadable, so its levels' unit is not known".to_string()
        })?;
    let Some(factor) = body.pointer("/snapshot/scalingFactor").and_then(json_number) else {
        return Err(
            "its snapshot states no scalingFactor, so whether its levels are price units is \
                    not known"
                .to_string(),
        );
    };
    let product = factor * pip;
    if !product.is_finite() || (product - 1.0).abs() > 1e-6 {
        return Err(format!(
            "its levels are not price units (scalingFactor {factor} x pip {pip} = {product}, not 1): a \
             level read as a price would be off by that factor, and it is not rescaled on a guess"
        ));
    }
    Ok(())
}

/// The names of the pairs that convert `quote` into `account`, the account being the quote of the first
/// (`GBPUSD` for GBP on USD: the mid IS account-per-quote) or the base of the second (`USDJPY`: `1/mid`), in
/// `epic`'s own product family (`CS.D.EURGBP.MINI.IP` -> `CS.D.GBPUSD.MINI.IP`). Empty for an epic that is not
/// five dot-separated segments: its family cannot be copied.
fn helper_epics(epic: &str, quote: &str, account: &str) -> Vec<String> {
    let parts: Vec<&str> = epic.split('.').collect();
    let [head, kind, _pair, family, tail] = parts[..] else { return Vec::new() };
    [format!("{quote}{account}"), format!("{account}{quote}")]
        .into_iter()
        .map(|pair| format!("{head}.{kind}.{pair}.{family}.{tail}"))
        .collect()
}

/// [`helper_epics`] for an epic that is no currency pair: the same two names in the FX Mini family
/// (`CS.D.GBPUSD.MINI.IP`, `CS.D.USDGBP.MINI.IP`).
fn fx_mini_pairs(quote: &str, account: &str) -> Vec<String> {
    [format!("{quote}{account}"), format!("{account}{quote}")]
        .into_iter()
        .map(|pair| fx_mini_epic(&pair))
        .collect()
}

/// The epic of `pair` (`EURUSD`) in IG's FX Mini family, `CS.D.EURUSD.MINI.IP`: the family this bridge trades
/// (its wired EUR/USD epic) and witnesses conversions with, so it is also the form `crate::operator_row` picks.
pub(crate) fn fx_mini_epic(pair: &str) -> String {
    format!("CS.D.{pair}.MINI.IP")
}

/// The price-derived account-per-quote out of ONE market body of a pair that relates `quote` and `account`,
/// the direction read from the body's own `onePipMeans` (`<pip> <QUOTE>/<BASE>`).
fn helper_witness(
    name: &str,
    body: &Value,
    quote: &str,
    account: &str,
) -> Result<(f64, Result<(), String>), IgPropertiesError> {
    let name_text = || name.to_string();
    let instrument = body.get("instrument").filter(|i| i.is_object());
    let text = |key: &str| {
        instrument.and_then(|i| i.get(key)).and_then(Value::as_str).unwrap_or("").to_string()
    };
    if text("epic") != name || text("type") != "CURRENCIES" {
        return Err(no_rate(IgRateRefusal::PairAnsweredAsOther {
            name: name_text(),
            answered: text("epic"),
            kind: text("type"),
        }));
    }
    let Some((_, pair_quote, pair_base)) = pip_and_pair(&text("onePipMeans")) else {
        return Err(no_rate(IgRateRefusal::PairPipUnreadable {
            name: name_text(),
            one_pip_means: text("onePipMeans"),
        }));
    };
    let price = mid(body)
        .map_err(|why| no_rate(IgRateRefusal::PairCannotWitness { name: name_text(), why }))?;
    let units = price_units(body).map_err(|why| format!("the conversion pair {name}: {why}"));
    if pair_base == quote && pair_quote == account {
        Ok((price, units))
    } else if pair_base == account && pair_quote == quote {
        Ok((inverse(price), units))
    } else {
        Err(no_rate(IgRateRefusal::PairUnrelated {
            name: name_text(),
            pair_base,
            pair_quote,
            quote: quote.to_string(),
            account: account.to_string(),
        }))
    }
}

/// The price-derived account-per-quote of `pair.quote`, where it came from, and whether the body it was taken
/// from says its levels are price units ([`price_units`]; only [`QuoteRateReading::PriceOnly`] needs it to be `Ok`).
fn witness(
    epic: &str,
    market: &Value,
    pair: &Pair,
    account: &str,
    helper: &mut dyn FnMut(&str) -> Result<Option<Value>, IgPropertiesError>,
) -> Result<(f64, String, Result<(), String>), IgPropertiesError> {
    if pair.base == account {
        let price = mid(market).map_err(|why| {
            no_rate(IgRateRefusal::CannotWitnessOwnRate { epic: epic.to_string(), why })
        })?;
        let units = price_units(market).map_err(|why| format!("{epic}: {why}"));
        return Ok((inverse(price), format!("1 / the mid {price} of {epic} itself"), units));
    }
    // An epic that is no currency pair (an index, a commodity, a coin, a bond: an empty `pair.base`) has no family
    // of its own to copy a conversion pair from (`IX.D.FTSE.CFD.IP` is not `IX.D.GBPUSD.CFD.IP`): its quote
    // currency is converted through IG's FX Mini family, measured to exist for GBPUSD, EURGBP and USDJPY.
    let what = if pair.base.is_empty() {
        format!("{epic} is quoted in {} on a {account} account", pair.quote)
    } else {
        format!("{epic} is a cross ({}/{} on a {account} account)", pair.base, pair.quote)
    };
    let names = if pair.base.is_empty() {
        fx_mini_pairs(&pair.quote, account)
    } else {
        helper_epics(epic, &pair.quote, account)
    };
    if names.is_empty() {
        return Err(no_rate(IgRateRefusal::NoPairFamily { what }));
    }
    for name in &names {
        if let Some(body) = helper(name)? {
            let (rate, units) = helper_witness(name, &body, &pair.quote, account)?;
            return Ok((rate, format!("the conversion pair {name}"), units));
        }
    }
    Err(no_rate(IgRateRefusal::NoPairListed {
        what,
        first: names[0].clone(),
        second: names[1].clone(),
    }))
}

/// **The rate for `epic`'s quote currency in `account`**, asked at `asked_ms` (module doc) and stamped with that
/// moment (IG states none of its own), with the [`QuoteRateReading`] of IG's table that held against the price beside it.
/// `helper` reads a conversion pair's market details: `Ok(None)` = IG has no such epic, `Err` = the read failed.
pub(crate) fn derive(
    epic: &str,
    market: &Value,
    pair: &Pair,
    account: &str,
    asked_ms: i64,
    helper: &mut dyn FnMut(&str) -> Result<Option<Value>, IgPropertiesError>,
) -> Result<(QuoteRate, QuoteRateReading), IgPropertiesError> {
    let instrument = market.get("instrument").unwrap_or(&Value::Null);
    let readings = candidates(epic, instrument, &pair.quote, account)?;
    let (priced, how, units) = witness(epic, market, pair, account, helper)?;
    // The rule `AccountPerQuote` gives the other bridges' rates, held on the price every rate below is judged by:
    // a mid that overflowed, or the reciprocal of one too small to invert, is refused, never handed on.
    let Some(price_rate) = AccountPerQuote::new(priced) else {
        return Err(no_rate(IgRateRefusal::PriceNotARate {
            epic: epic.to_string(),
            how,
            account: account.to_string(),
            quote: pair.quote.clone(),
            priced,
        }));
    };
    if readings.is_empty() {
        // The table lists no row for the account currency and its base column reads nothing (candidates): the
        // price is the whole evidence, so the price must itself be believable as a price. Refused, never
        // rescaled, when its levels are not price units (a pip-scaled 2020 USDJPY level of 10806.1 would be a
        // rate 100x off).
        if let Err(why) = units {
            return Err(no_rate(IgRateRefusal::PriceNotInPriceUnits {
                epic: epic.to_string(),
                account: account.to_string(),
                quote: pair.quote.clone(),
                how,
                why,
            }));
        }
        // shortcut: mid, no fee, IG converts at a bank rate + ~0.7%, up to 0.7% gap (see the end of `derive`).
        return Ok((rate_of(epic, pair, price_rate, asked_ms)?, QuoteRateReading::PriceOnly));
    }
    // `priced` is finite and positive (checked above), so a candidate within the tolerance of it is a rate too:
    // `AccountPerQuote::new` refuses none of them.
    let agreeing: Vec<(QuoteRateReading, AccountPerQuote)> = readings
        .iter()
        .filter(|(_, x)| (x / priced - 1.0).abs() <= RATE_TOLERANCE)
        .filter_map(|&(reading, x)| Some((reading, AccountPerQuote::new(x)?)))
        .collect();
    let Some(&(reading, account_per_quote)) = agreeing.first() else {
        let stated = readings
            .iter()
            .map(|(r, x)| format!("{} = {x}", r.word()))
            .collect::<Vec<_>>()
            .join("; ");
        let scale = market
            .pointer("/snapshot/scalingFactor")
            .map_or_else(|| "absent".to_string(), Value::to_string);
        return Err(no_rate(IgRateRefusal::NoReadingAgrees {
            epic: epic.to_string(),
            account: account.to_string(),
            quote: pair.quote.clone(),
            tolerance_percent: RATE_TOLERANCE * 100.0,
            how,
            priced,
            stated,
            scale,
        }));
    };
    // More than one reading agrees with the price, and they are not the same number: which one IG meant is a
    // GUESS (the order the readings are tried in would decide it), so the symbol is not admitted.
    let rates = agreeing.iter().map(|(_, x)| x.get());
    let (low, high) = rates.fold((f64::INFINITY, 0.0_f64), |(lo, hi), x| (lo.min(x), hi.max(x)));
    if high / low - 1.0 > AMBIGUITY_MARGIN {
        let both = agreeing
            .iter()
            .map(|(r, x)| format!("{} = {}", r.word(), x.get()))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(no_rate(IgRateRefusal::ReadingsDisagree {
            epic: epic.to_string(),
            count: agreeing.len(),
            how,
            priced,
            tolerance_percent: RATE_TOLERANCE * 100.0,
            both,
        }));
    }
    // shortcut: mid, no fee, IG converts at a bank rate + ~0.7%, up to 0.7% gap. IG converts a realised P/L
    // itself (instantly, or about 22:00 UK on a 'daily' setting the API cannot read) at the best rate of several
    // banks plus an administration fee it states as 0.7% (ig.com help article; 0.8% in its own example); the
    // engine books the rate below, a mid within 2% of a market price. Upgrade by reading the booked amount from
    // the close's confirm / the transaction history once a live close has priced the gap.
    Ok((rate_of(epic, pair, account_per_quote, asked_ms)?, reading))
}

/// The rate for `pair`'s quote currency, refused (never a blank code) when the epic names no quote currency.
fn rate_of(
    epic: &str,
    pair: &Pair,
    account_per_quote: AccountPerQuote,
    asked_ms: i64,
) -> Result<QuoteRate, IgPropertiesError> {
    QuoteRate::new(&pair.quote, account_per_quote, asked_ms).ok_or_else(|| {
        no_rate(IgRateRefusal::NoQuoteCurrency {
            epic: epic.to_string(),
            quote: pair.quote.clone(),
        })
    })
}

/// The refusal of an account that is not in the wire's currency, shared with the facts' own account fence so
/// the two cannot drift.
pub(crate) fn wire_account(account: &str) -> IgSymbolRefusal {
    IgSymbolRefusal::AccountNotInWireCurrency { account: account.to_string() }
}

#[cfg(test)]
#[path = "quote_rate_tests.rs"]
mod quote_rate_tests;
