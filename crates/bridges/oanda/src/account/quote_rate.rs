//! The OANDA RATE SOURCE for the engine's multi-currency money: what one unit of an instrument's QUOTE
//! currency is worth in the ACCOUNT's own (home) currency, read from the venue's `homeConversions`.
//!
//! This module is the cold-path half the node calls: `CatalogProviderOanda::home_conversions` is read by the
//! gatewayhub's symbol-facts lane (`crate::symbol_properties::quoted_rate`; `CatalogProviderOanda::quote_currencies` has no
//! caller yet), and nothing here touches the engine. The node turns a [`QuoteRate`] into the engine's
//! per-currency rate.
//!
//! # The orientation, which is the whole risk
//!
//! Every rate here is [`AccountPerQuote`]: **account-currency units per ONE unit of the quote currency**.
//! A quote-currency amount is MULTIPLIED by it ([`AccountPerQuote::to_account`]), never divided. On a USD
//! account USDJPY at 150 gives JPY `0.00667` (a fraction of a dollar per yen), NOT `150`; EURGBP gives GBP
//! `1.35` (more than a dollar per pound). The flipped rate is a 22,500x error (150 x 150) and is the
//! classic bug, so the direction lives in the TYPE (the only constructor refuses a non-rate and the only
//! way to use one is `to_account`) and in the golden tests below, whose names say which way they point.
//!
//! OANDA's own wording (HomeConversions, <https://developer.oanda.com/rest-live-v20/pricing-df/>):
//! `accountGain` / `accountLoss` / `positionValue` are "the factor used to convert" a gain / a loss / a
//! Position or Trade Value "in the specified currency into the Account's home currency", i.e. a factor
//! the currency amount is multiplied by. The endpoint
//! (<https://developer.oanda.com/rest-live-v20/pricing-ep/>): `GET /v3/accounts/{id}/pricing` takes
//! `includeHomeConversions=true` and then returns a top-level `homeConversions` array with "an entry for
//! each currency in the set of all base and quote currencies present in the requested instruments list".
//! ⚠ The shapes here are DOCS-SHAPED: the pages define the fields and the array but show no example body
//! that contains one, and nothing was captured from a practice account. The pricing STREAM takes the same
//! flag, but its pages do not show where a line carries the array; [`HomeConversions::from_pricing_body`]
//! reads the top-level `homeConversions` of whatever body it is given, and a body without one is an
//! error, never a rate.
//!
//! # Which factor for which money (the multi-currency design, section 3.4)
//!
//! - **`positionValue` -> notional, exposure, margin and unrealized PnL.** OANDA documents it for a
//!   "Position or Trade Value", which is what those numbers are (a position's value in the quote
//!   currency), and margin is that value times the margin rate. It is also the ONE scalar of phase 1
//!   ([`QuoteRate`]): the design says "a single mid is enough for phase 1", the engine's effective
//!   contract value takes one factor per currency, and the gain/loss spread is about 1e-4 relative
//!   (the design's figure, UNMEASURED here).
//! - **`accountGain` / `accountLoss` -> realized PnL, by SIGN.** OANDA documents them for realized P/L
//!   and financing: the gain factor for a positive amount, the loss factor for a negative one
//!   ([`HomeConversion::realized_pnl_rate`]). Where the design is silent (one number for realized PnL),
//!   the conservative reading is NEVER one scalar for both signs: a single factor either overstates the
//!   gains or understates the losses, and overstated PnL is the dangerous direction (it hides a real
//!   loss from the margin call and the drawdown latch). So phase 1's scalar is `positionValue` and the
//!   sign-aware pair is kept, intact, for the two-sided phase the design names as the reopener.
//!
//! Realized PnL converts ONCE, at the factor in force when the fill folds (the margin-account
//! convention); no entry rate is stored. That is a claim about the venue that no demo has measured.
//!
//! # Fail closed
//!
//! No conversion, an unknown currency, a missing, zero, negative, non-finite or non-numeric factor, a
//! currency listed twice, a response without the array, a request that failed: each is a [`QuoteRateError`].
//! No path returns `1.0` for "I do not know". The single `1.0` is the home currency converting into
//! itself, which is an identity and not a lookup, and an entry the venue DOES send for the home currency
//! must say 1 too, or the home currency we read is not the one OANDA converts into (a wrong label makes
//! every rate wrong at once).

use std::collections::BTreeMap;

use serde_json::Value;
use vike_model::{AccountPerQuote, Instrument, QuoteRate, QuoteRateError, currency_code};

use crate::catalog::decimal_field;
use crate::refusals::OandaRateRefusal;

/// How far an entry for the HOME currency may sit from 1 before the home we read is called wrong.
/// OANDA states decimals to a handful of places; this only has to be tighter than any real rate.
const HOME_IDENTITY_TOLERANCE: f64 = 1e-6;

/// OANDA sent no entry for `currency` (and it is not the home currency): an answer that names no rate for it,
/// never a verdict on the instrument ([`QuoteRateError::BadQuote`]).
fn unknown_currency(currency: &str) -> QuoteRateError {
    OandaRateRefusal::NoConversionFor { currency: currency.to_string() }.into()
}

/// One `homeConversions` entry, as OANDA states it. All three are [`AccountPerQuote`] for `currency`.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeConversion {
    /// The currency being converted FROM (upper-case: `JPY`).
    pub currency: String,
    /// For a positive realized amount or financing in `currency` (`accountGain`).
    pub account_gain: AccountPerQuote,
    /// For a negative realized amount or financing in `currency` (`accountLoss`).
    pub account_loss: AccountPerQuote,
    /// For a position's or trade's VALUE in `currency`: notional, margin, unrealized (`positionValue`).
    pub position_value: AccountPerQuote,
}

impl HomeConversion {
    /// The factor OANDA documents for a REALIZED amount of this sign: the loss factor for a negative
    /// amount, the gain factor otherwise (zero converts to zero at either). One scalar for both signs
    /// is the thing this method exists to avoid (module doc).
    #[must_use]
    pub fn realized_pnl_rate(&self, pnl_in_quote_currency: f64) -> AccountPerQuote {
        if pnl_in_quote_currency < 0.0 { self.account_loss } else { self.account_gain }
    }
}

/// The parsed `homeConversions` of one pricing response, with the account's home currency and the time.
#[derive(Debug, Clone, PartialEq)]
pub struct HomeConversions {
    home: String,
    as_of_ms: i64,
    /// Per currency: the usable entry, or why it is not (a bad entry fails ITS currency only).
    by_currency: BTreeMap<String, Result<HomeConversion, QuoteRateError>>,
}

impl HomeConversions {
    /// Parse the top-level `homeConversions` array of a pricing body for the account whose home currency
    /// is `home`, observed at `as_of_ms`. `Err` when the array is absent or empty, or when the venue's
    /// entry for the home currency itself does not say 1 (the home currency we hold is then not the one
    /// OANDA converts into, and every rate would be wrong at once). Any other bad entry is kept as an
    /// error for ITS currency, so one corrupt currency does not take the others down.
    pub fn from_pricing_body(
        home: &str,
        body: &Value,
        as_of_ms: i64,
    ) -> Result<Self, QuoteRateError> {
        let home = home.trim().to_ascii_uppercase();
        if home.is_empty() {
            return Err(OandaRateRefusal::HomeUnknown.into());
        }
        let list = body
            .get("homeConversions")
            .and_then(Value::as_array)
            .filter(|list| !list.is_empty())
            .ok_or(OandaRateRefusal::NoConversions)?;
        let mut by_currency = BTreeMap::new();
        for entry in list {
            let Some(currency) = entry
                .get("currency")
                .and_then(Value::as_str)
                .map(|c| c.trim().to_ascii_uppercase())
                .filter(|c| !c.is_empty())
            else {
                continue; // an entry naming no currency answers no question
            };
            let parsed = if by_currency.contains_key(&currency) {
                Err(OandaRateRefusal::ListedTwice { currency: currency.clone() })
            } else {
                conversion_of(&currency, entry)
            };
            by_currency.insert(currency, parsed.map_err(QuoteRateError::from));
        }
        if let Some(Ok(own)) = by_currency.get(&home) {
            let factors = [own.account_gain, own.account_loss, own.position_value];
            if factors.iter().any(|f| (f.get() - 1.0).abs() > HOME_IDENTITY_TOLERANCE) {
                let [gain, loss, position] = factors.map(AccountPerQuote::get);
                let bad = OandaRateRefusal::HomeNotOne { currency: home, gain, loss, position };
                return Err(bad.into());
            }
        }
        Ok(Self { home, as_of_ms, by_currency })
    }

    /// The account's home currency (upper-case).
    #[must_use]
    pub fn home(&self) -> &str {
        &self.home
    }

    /// When this was observed, epoch ms (the node's freshness clock for every rate in it).
    #[must_use]
    pub fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// OANDA's entry for `currency`, all three factors. `Err` for a currency OANDA sent no entry for
    /// (the home currency included: [`Self::quote_rate`] is the call that knows the identity) or whose
    /// entry is unusable.
    pub fn conversion(&self, currency: &str) -> Result<&HomeConversion, QuoteRateError> {
        let Some(currency) = currency_code(currency) else {
            return Err(unknown_currency(""));
        };
        match self.by_currency.get(&currency) {
            Some(Ok(entry)) => Ok(entry),
            Some(Err(why)) => Err(why.clone()),
            None => Err(unknown_currency(&currency)),
        }
    }

    /// **The phase-1 rate for an instrument quoted in `quote_ccy`**: `positionValue` (module doc), with
    /// the observation time: the moment the request was ASKED, not when it was answered (the conservative
    /// side, since the rate is no newer than that). The home currency is the identity (1, no entry needed); an
    /// empty or unknown currency is an error, so an absent currency is never read as the account's.
    pub fn quote_rate(&self, quote_ccy: &str) -> Result<QuoteRate, QuoteRateError> {
        let Some(quote_ccy) = currency_code(quote_ccy) else {
            return Err(unknown_currency(""));
        };
        let account_per_quote = if quote_ccy == self.home {
            AccountPerQuote::IDENTITY
        } else {
            self.conversion(&quote_ccy)?.position_value
        };
        QuoteRate::new(&quote_ccy, account_per_quote, self.as_of_ms)
            .ok_or_else(|| unknown_currency(&quote_ccy))
    }
}

/// One entry's three factors, or the first reason it is unusable.
fn conversion_of(currency: &str, entry: &Value) -> Result<HomeConversion, OandaRateRefusal> {
    Ok(HomeConversion {
        currency: currency.to_string(),
        account_gain: factor(currency, entry, "accountGain")?,
        account_loss: factor(currency, entry, "accountLoss")?,
        position_value: factor(currency, entry, "positionValue")?,
    })
}

/// One factor field of `currency`'s entry: a v20 `DecimalNumber` (a JSON string, a bare number accepted), finite
/// and positive.
fn factor(
    currency: &str,
    entry: &Value,
    field: &'static str,
) -> Result<AccountPerQuote, OandaRateRefusal> {
    let raw = entry.get(field);
    let x = decimal_field(raw).ok_or_else(|| OandaRateRefusal::FactorUnread {
        currency: currency.to_string(),
        field,
        raw: raw.map_or_else(|| "absent".to_string(), Value::to_string),
    })?;
    AccountPerQuote::new(x).ok_or_else(|| OandaRateRefusal::FactorNotPositive {
        currency: currency.to_string(),
        field,
        x,
    })
}

/// **The quote currency of an OANDA instrument**, from its wire `BASE_QUOTE` name (`EUR_USD` -> `USD`,
/// `SPX500_USD` -> `USD`; the name a kept row carries, never a user name: `EURUSD` says nothing here). `None` unless
/// the name is exactly one base, one underscore and an alphabetic quote: a name that does not say its quote currency
/// is unknown, and unknown is never the account's currency.
#[must_use]
pub fn quote_currency_of(instrument: &str) -> Option<String> {
    let name = instrument.trim().to_ascii_uppercase();
    let (base, quote) = name.split_once('_')?;
    let base_ok = !base.is_empty() && base.chars().all(|c| c.is_ascii_alphanumeric());
    let quote_ok = !quote.is_empty() && quote.chars().all(|c| c.is_ascii_alphabetic());
    (base_ok && quote_ok).then(|| quote.to_string())
}

/// The quote currency of every row of the account's own instrument list (`EUR_USD` -> `USD`). A row whose
/// name is not `BASE_QUOTE` ([`quote_currency_of`]) is absent, and absent means unknown.
#[must_use]
pub fn quote_currencies_of(rows: &[Instrument]) -> BTreeMap<String, String> {
    rows.iter()
        .filter_map(|row| Some((row.raw_symbol.clone(), quote_currency_of(&row.raw_symbol)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape OANDA's pricing pages DOCUMENT (a top-level `homeConversions` array of
    /// `{currency, accountGain, accountLoss, positionValue}` decimal strings beside `prices`), with
    /// illustrative values. DOCS-SHAPED: no body was captured from a practice account, and the pages show
    /// no example that contains the array.
    fn body(entries: Value) -> Value {
        json!({ "time": "1666000000.000000000", "prices": [], "homeConversions": entries })
    }

    fn entry(currency: &str, gain: &str, loss: &str, position: &str) -> Value {
        json!({
            "currency": currency,
            "accountGain": gain,
            "accountLoss": loss,
            "positionValue": position
        })
    }

    /// A USD account asking after USD_JPY at 150.10 and EUR_GBP: JPY is a FRACTION of a dollar per yen,
    /// GBP is MORE than a dollar per pound, USD converts at exactly 1.
    fn usd_account() -> HomeConversions {
        let entries = json!([
            entry("USD", "1", "1", "1"),
            entry("JPY", "0.0066600", "0.0066650", "0.0066622"),
            entry("GBP", "1.3495", "1.3505", "1.35"),
        ]);
        HomeConversions::from_pricing_body("USD", &body(entries), 1_700_000_000_123)
            .expect("parses")
    }

    #[test]
    fn jpy_quote_on_a_usd_account_is_a_fraction_of_a_dollar_per_yen_not_yen_per_dollar() {
        let rate = usd_account().quote_rate("JPY").expect("JPY");
        let r = rate.account_per_quote;
        assert!(
            r.get() < 0.01 && r.get() > 0.006,
            "{} is yen-per-dollar, the flipped rate",
            r.get()
        );
        // 100,000 units of USDJPY at 150.10 are 15,010,000 yen = 100,000 dollars (the design's golden).
        assert!((r.to_account(100_000.0 * 150.10) - 100_000.0).abs() < 1.0);
        // a 0.10 yen move on 100,000 units is 10,000 yen = 66.62 dollars, NOT 10,000 dollars.
        assert!((r.to_account(100_000.0 * 0.10) - 66.62).abs() < 0.01);
        assert_eq!((rate.quote_currency.as_str(), rate.as_of_ms), ("JPY", 1_700_000_000_123));
    }

    #[test]
    fn gbp_quote_on_a_usd_account_is_more_than_one_dollar_per_pound() {
        let r = usd_account().quote_rate("GBP").expect("GBP").account_per_quote;
        assert!(r.get() > 1.0, "{} is pounds-per-dollar, the flipped rate", r.get());
        // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars (understated 1.35x if raw).
        assert!((r.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    }

    #[test]
    fn the_home_currency_is_the_identity_even_with_no_entry_for_it() {
        let entries = json!([entry("JPY", "0.00666", "0.00667", "0.006665")]);
        let rates = HomeConversions::from_pricing_body("usd", &body(entries), 1).expect("parses");
        assert_eq!(rates.home(), "USD");
        for spelling in ["USD", "usd", " Usd "] {
            let rate = rates.quote_rate(spelling).expect("the identity").account_per_quote;
            assert_eq!(rate.get(), 1.0, "{spelling}");
        }
        // ...but `conversion` is OANDA's entry only: none was sent for USD.
        assert_eq!(rates.conversion("USD"), Err(unknown_currency("USD")));
    }

    #[test]
    fn a_currency_oanda_did_not_send_has_no_rate_and_never_one() {
        let rates = usd_account();
        // a refusal of the answer (never a verdict on the symbol)
        let chf = OandaRateRefusal::NoConversionFor { currency: "CHF".into() };
        assert_eq!(rates.quote_rate("CHF"), Err(QuoteRateError::BadQuote(chf.to_string())));
        // an absent currency is not the account's currency
        assert_eq!(rates.quote_rate(""), Err(unknown_currency("")));
        assert_eq!(rates.quote_rate("  "), Err(unknown_currency("")));
    }

    #[test]
    fn a_body_without_home_conversions_is_an_error_not_a_rate() {
        for bad in [
            json!({ "prices": [] }),
            json!({ "prices": [], "homeConversions": [] }),
            json!({ "homeConversions": "USD" }),
            json!({ "homeConversions": { "currency": "JPY" } }),
            json!([]),
            Value::Null,
        ] {
            assert_eq!(
                HomeConversions::from_pricing_body("USD", &bad, 1),
                Err(QuoteRateError::BadQuote(OandaRateRefusal::NoConversions.to_string())),
                "{bad}"
            );
        }
        assert_eq!(
            HomeConversions::from_pricing_body("", &body(json!([entry("JPY", "1", "1", "1")])), 1),
            Err(QuoteRateError::Unavailable(OandaRateRefusal::HomeUnknown.to_string()))
        );
    }

    /// Each unusable factor fails ITS currency with a reason, and leaves the rest answering.
    #[test]
    fn a_missing_zero_negative_or_non_finite_factor_is_an_error_for_that_currency_only() {
        let broken = |field: &str, value: Value| {
            let mut jpy = entry("JPY", "0.00666", "0.00667", "0.006665");
            match value {
                Value::Null => drop(jpy.as_object_mut().unwrap().remove(field)),
                v => drop(jpy.as_object_mut().unwrap().insert(field.into(), v)),
            }
            json!([jpy, entry("GBP", "1.3495", "1.3505", "1.35")])
        };
        let unread = |field: &'static str, raw: &str| OandaRateRefusal::FactorUnread {
            currency: "JPY".into(),
            field,
            raw: raw.into(),
        };
        let not_positive = |field: &'static str, x: f64| OandaRateRefusal::FactorNotPositive {
            currency: "JPY".into(),
            field,
            x,
        };
        let cases = [
            ("positionValue", json!("0"), not_positive("positionValue", 0.0)),
            ("positionValue", json!("-0.00666"), not_positive("positionValue", -0.00666)),
            ("positionValue", json!("NaN"), unread("positionValue", r#""NaN""#)),
            ("positionValue", json!("inf"), unread("positionValue", r#""inf""#)),
            ("positionValue", json!(""), unread("positionValue", r#""""#)),
            ("positionValue", json!("cheap"), unread("positionValue", r#""cheap""#)),
            ("positionValue", json!(true), unread("positionValue", "true")),
            ("positionValue", json!(0), not_positive("positionValue", 0.0)),
            ("positionValue", Value::Null, unread("positionValue", "absent")), // no field
            ("accountGain", json!("0"), not_positive("accountGain", 0.0)),
            ("accountLoss", Value::Null, unread("accountLoss", "absent")),
        ];
        for (field, value, refusal) in cases {
            let rates =
                HomeConversions::from_pricing_body("USD", &body(broken(field, value.clone())), 1)
                    .expect("one bad entry does not fail the response");
            let why = rates.quote_rate("JPY").expect_err(&format!("{field}={value}"));
            assert_eq!(why, QuoteRateError::BadQuote(refusal.to_string()), "{field}={value}");
            // even for the field that is NOT the engine's scalar: a half-usable entry is not used
            assert!(rates.conversion("JPY").is_err(), "{field}={value}");
            assert_eq!(
                rates.quote_rate("GBP").expect("GBP is intact").account_per_quote.get(),
                1.35
            );
        }
    }

    #[test]
    fn a_bare_json_number_is_a_decimal_too() {
        let entries = json!([{
            "currency": "JPY", "accountGain": 0.0066, "accountLoss": 0.0067, "positionValue": 0.00665
        }]);
        let rates = HomeConversions::from_pricing_body("USD", &body(entries), 1).expect("parses");
        assert_eq!(rates.quote_rate("JPY").unwrap().account_per_quote.get(), 0.00665);
    }

    #[test]
    fn a_currency_listed_twice_is_an_error_not_the_last_one() {
        let entries = json!([
            entry("JPY", "0.00666", "0.00667", "0.006665"),
            entry("JPY", "6.66", "6.67", "6.665"),
        ]);
        let rates = HomeConversions::from_pricing_body("USD", &body(entries), 1).expect("parses");
        let twice = OandaRateRefusal::ListedTwice { currency: "JPY".into() };
        assert_eq!(rates.quote_rate("JPY"), Err(QuoteRateError::BadQuote(twice.to_string())));
    }

    /// OANDA converts into ITS home currency; if its entry for the one we hold is not 1, ours is wrong.
    #[test]
    fn an_entry_for_the_home_currency_that_is_not_one_means_the_home_currency_is_wrong() {
        // The body of a JPY account: a dollar is 150 yen, a yen is 1 yen.
        let entries = json!([entry("USD", "150.0", "150.2", "150.1"), entry("JPY", "1", "1", "1")]);
        // We believe the account is USD: OANDA's own USD entry would then have to say 1.
        let wrong = OandaRateRefusal::HomeNotOne {
            currency: "USD".into(),
            gain: 150.0,
            loss: 150.2,
            position: 150.1,
        };
        assert_eq!(
            HomeConversions::from_pricing_body("USD", &body(entries.clone()), 1),
            Err(QuoteRateError::BadQuote(wrong.to_string()))
        );
        // ...and the same body is fine for the account that really is JPY.
        let jpy = HomeConversions::from_pricing_body("JPY", &body(entries), 1).expect("JPY home");
        assert_eq!(jpy.quote_rate("JPY").unwrap().account_per_quote.get(), 1.0);
        assert_eq!(jpy.quote_rate("USD").unwrap().account_per_quote.get(), 150.1);
    }

    #[test]
    fn realized_pnl_takes_the_gain_factor_for_a_gain_and_the_loss_factor_for_a_loss() {
        let rates = usd_account();
        let jpy = rates.conversion("jpy").expect("JPY");
        assert_eq!(jpy.currency, "JPY");
        assert_eq!(jpy.realized_pnl_rate(10_000.0).get(), 0.00666);
        assert_eq!(jpy.realized_pnl_rate(-10_000.0).get(), 0.006665);
        // the engine's single phase-1 scalar is the position value, a third number
        assert_eq!(rates.quote_rate("JPY").unwrap().account_per_quote.get(), 0.0066622);
    }

    #[test]
    fn the_quote_currency_is_the_part_after_the_underscore() {
        for (name, quote) in [
            ("EUR_USD", "USD"),
            ("USD_JPY", "JPY"),
            ("usd_jpy", "JPY"),
            ("SPX500_USD", "USD"),
            ("XAU_USD", "USD"),
            ("DE30_EUR", "EUR"),
            (" EUR_CHF ", "CHF"),
        ] {
            assert_eq!(quote_currency_of(name).as_deref(), Some(quote), "{name}");
        }
        // `EURGBP` is a USER name: the quote is the wire name's to say (decision 0290).
        for unknown in [
            "",
            "EURGBP",
            "EURUSDX",
            "EUR_",
            "_USD",
            "EUR_USD_JPY",
            "EUR_U$D",
            "EUR_US1",
            "EUR USD",
        ] {
            assert_eq!(quote_currency_of(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn every_listed_instrument_has_its_quote_currency_from_the_accounts_own_list() {
        let payload: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/instruments.json"))
                .expect("fixture is JSON");
        let quotes = quote_currencies_of(&crate::catalog::parse_instruments(&payload));
        assert_eq!(quotes.get("EUR_USD").map(String::as_str), Some("USD"));
        assert_eq!(quotes.get("USD_JPY").map(String::as_str), Some("JPY"));
        assert_eq!(quotes.get("SPX500_USD").map(String::as_str), Some("USD"));
        assert_eq!(quotes.len(), 6, "{quotes:?}");
    }

    /// No credentials is an ERROR on both reads, raised before any request is built: an empty answer
    /// would read as "nothing to convert".
    #[test]
    fn an_account_with_no_credentials_has_no_rates_and_no_quote_currencies() {
        use vike_secrets::account_credentials::AccountCredentials;
        let catalog = crate::catalog::catalog_tests::catalog(&AccountCredentials::none());
        assert!(matches!(
            catalog.home_conversions(&["USD_JPY"]),
            Err(QuoteRateError::Unavailable(_))
        ));
        assert!(catalog.quote_currencies().is_err());
    }

    /// Only an instrument-name alphabet reaches the pre-formatted query, and an empty list asks nothing.
    #[test]
    fn the_rate_request_refuses_a_hostile_or_empty_instrument_list_before_any_request() {
        use vike_secrets::account_credentials::{AccountCredentials, CredentialFields};
        let fields =
            CredentialFields::from_pairs([("API_KEY", "tok"), ("ACCOUNT_ID", "101-004-1-001")]);
        let catalog = crate::catalog::catalog_tests::catalog(&AccountCredentials::new(
            Some(1),
            Some(&fields),
            None,
        ));
        for bad in [&[][..], &["USD_JPY&since=1"][..], &["USD_JPY", ""][..], &["USD JPY"][..]] {
            let refused = QuoteRateError::from(OandaRateRefusal::NoInstrumentNames);
            assert_eq!(catalog.home_conversions(bad), Err(refused), "{bad:?}");
        }
    }
}
