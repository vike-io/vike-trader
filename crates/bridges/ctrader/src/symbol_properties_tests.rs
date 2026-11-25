//! [`super::quoted_rate`] over the bridge's rate read (the rate and the session's deposit currency), built the way the
//! bridge builds it from a two-sided quote
//! ([`crate::account::rate_of_quote`]). The quotes are DOCS-SHAPED (the Open API states bid and ask in
//! 1/100000 of a price unit, <https://help.ctrader.com/open-api/messages/>): nothing was captured from a demo
//! account. The goldens are the decision's own, in the orientation the engine takes: account-currency units per
//! ONE unit of the quote currency.

use vike_model::AccountPerQuoteLeg;

use super::*;
use crate::account::rate_of_quote;

const ASKED_AT_MS: i64 = 1_700_000_000_123;

/// [`super::quoted_rate`]'s typed refusal and whether the node would ask again after it (the seam's kind,
/// `SymbolPropertiesError::is_transient`); the seam carries the refusal's text.
fn refused(
    quote: &str,
    deposit: &str,
    read: impl Fn(&str) -> Result<(QuoteRate, String), QuoteRateError>,
) -> (CtraderPropertiesRefusal, bool) {
    let typed = rate_for(quote, deposit, &read).expect_err("refused");
    let seam = super::quoted_rate(quote, deposit, &read).expect_err("refused");
    assert_eq!(seam.text(), typed.to_string(), "the seam carries the refusal's text");
    let (SymbolPropertiesError::Refused(carried) | SymbolPropertiesError::RateUnavailable(carried)) =
        &seam
    else {
        panic!("a rate refusal is a verdict or a failed rate: {seam:?}")
    };
    assert_eq!(*carried, typed.carried(), "and its id");
    (typed, seam.is_transient())
}

/// The refusal of a JPY rate on a USD account, the read's own words aside.
fn no_jpy_rate(why: &str) -> CtraderPropertiesRefusal {
    CtraderPropertiesRefusal::RateUnavailable {
        quote: "JPY".into(),
        deposit: "USD".into(),
        why: why.to_string(),
    }
}

/// A USD account's venue: USDJPY 150.09 / 150.11 (USD is the BASE of the pair), GBPUSD 1.34999 / 1.35001 (the
/// quote currency is the BASE), nothing for any other currency.
fn usd_account(quote: &str) -> Result<(QuoteRate, String), QuoteRateError> {
    priced_for("USD", quote)
}

/// [`usd_account`] for an account whose deposit currency is `account_ccy` as the SESSION states it (the pair is
/// still the USD one: the session is what names the account).
fn priced_for(account_ccy: &str, quote: &str) -> Result<(QuoteRate, String), QuoteRateError> {
    let (leg, bid, ask) = match quote {
        "JPY" => (AccountPerQuoteLeg::AccountIsBase, 15_009_000, 15_011_000),
        "GBP" => (AccountPerQuoteLeg::QuoteIsBase, 134_999, 135_001),
        other => {
            return Err(QuoteRateError::NoPair(format!(
                "ctrader: no forex pair between {other} and USD"
            )));
        }
    };
    let rate = QuoteRate {
        quote_currency: quote.to_string(),
        account_per_quote: rate_of_quote(leg, bid, ask)?,
        as_of_ms: ASKED_AT_MS,
    };
    Ok((rate, account_ccy.to_string()))
}

/// **The orientation, by value**: a JPY quote is a fraction of a dollar, a GBP quote is above one, in any case
/// of the currency code, and the moment is the one the read stamped.
#[test]
fn a_yen_is_a_fraction_of_a_dollar_and_a_pound_is_more_than_one() {
    for jpy in ["JPY", "jpy", " Jpy "] {
        let rate = quoted_rate(jpy, "USD", usd_account).expect("priced").expect("foreign");
        assert_eq!((rate.quote_currency.as_str(), rate.as_of_ms), ("JPY", ASKED_AT_MS), "{jpy}");
        let x = rate.account_per_quote.get();
        assert!(x < 0.01 && x > 0.006, "{jpy}");
        // 100,000 units of USDJPY at 150.10 are 15,010,000 yen = 100,000 dollars; a 0.10 move is 66.62.
        assert!((x * 100_000.0 * 150.10 - 100_000.0).abs() < 1e-6, "{jpy}");
        assert!((x * 100_000.0 * 0.10 - 66.62).abs() < 0.01, "{jpy}");
    }
    let rate = quoted_rate("GBP", "USD", usd_account).expect("priced").expect("foreign");
    assert_eq!((rate.quote_currency.as_str(), rate.account_per_quote.get()), ("GBP", 1.35));
    // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars (understated 1.35x if raw).
    assert!((rate.account_per_quote.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
}

/// A pair quoted in the deposit currency needs no conversion, and the venue is never asked about it.
#[test]
fn a_quote_in_the_deposit_currency_needs_no_rate_and_asks_nothing() {
    for (quote, deposit) in [("USD", "USD"), ("usd", "USD"), ("USD", "usd"), (" EUR ", "EUR")] {
        let mut asked = false;
        let out = quoted_rate(quote, deposit, |q| {
            asked = true;
            usd_account(q)
        });
        assert_eq!(out, Ok(None), "{quote}/{deposit}");
        assert!(!asked, "{quote}/{deposit}: no session for a pair that needs no conversion");
    }
}

/// **No rate, no admission**: a row with no quote currency, a read that failed, a quote that is no price and
/// an answer for another currency are each refused as a FAILED RATE (the node does not remember it as a
/// verdict on the symbol); a currency the account lists no single pair for is a VERDICT; none answers 1.0.
#[test]
fn what_cannot_be_priced_is_refused_never_as_par() {
    let (unnamed, transient) = refused("  ", "USD", usd_account);
    assert_eq!(unnamed, CtraderPropertiesRefusal::InstrumentNamesNoQuote { deposit: "USD".into() });
    assert!(!transient, "a verdict on the row: {unnamed}");

    for failed in [
        QuoteRateError::Unavailable(
            "ctrader: reading USDJPY: cTrader error CH_ACCESS_TOKEN_INVALID".into(),
        ),
        QuoteRateError::BadQuote("ctrader: the conversion pair's quote is crossed".into()),
    ] {
        let (why, transient) = refused("JPY", "USD", |_| Err(failed.clone()));
        assert_eq!(why, no_jpy_rate(&failed.to_string()));
        assert!(transient, "{why}");
    }

    let (no_pair, transient) = refused("NZD", "USD", usd_account);
    assert_eq!(
        no_pair,
        CtraderPropertiesRefusal::NoPair {
            no_pair: QuoteRateError::NoPair("ctrader: no forex pair between NZD and USD".into())
                .to_string()
        }
    );
    assert!(!transient, "the node remembers a verdict: {no_pair}");

    // A read that answers for another currency than the one asked is the flipped-rate class of bug.
    let (wrong, transient) = refused("GBP", "USD", |_| usd_account("JPY"));
    assert_eq!(
        wrong,
        CtraderPropertiesRefusal::RateForAnotherCurrency {
            quote: "GBP".into(),
            deposit: "USD".into(),
            answered: "JPY".into()
        }
    );
    assert!(transient, "{wrong}");
}

/// **Credentials swapped under one label**: the listing (kept minutes) says the account deposits in USD, the
/// session that priced the rate was for an EUR account (a rate that is par for EUR). The rate is REFUSED, as a
/// failure of the rate (the node does not remember it as a verdict on the symbol), never booked at par on a
/// USD engine; the same rate read for the listing's own currency is a rate.
#[test]
fn a_rate_priced_for_another_accounts_currency_is_refused() {
    let (swapped, transient) = refused("JPY", "USD", |q| priced_for("EUR", q));
    assert_eq!(
        swapped,
        CtraderPropertiesRefusal::RateForAnotherAccount {
            quote: "JPY".into(),
            deposit: "USD".into(),
            account: "EUR".into()
        }
    );
    assert!(transient, "{swapped}");

    let own = quoted_rate("JPY", "USD", |q| priced_for("usd", q)).expect("own").expect("foreign");
    assert_eq!(own.quote_currency, "JPY");
}

/// F-8: a failed ask is kept no longer than the node waits before it asks again (`RATE_RETRY_AFTER`, 10 s), or the
/// node's retry is answered from memory and the admission stays refused past the venue's recovery. Red at the old 30 s.
/// cTrader holds at most half of the hub's four lookup threads.
#[test]
fn a_failed_ask_is_kept_no_longer_than_the_nodes_retry_and_the_hub_runs_two() {
    assert!(RATE_FAILED_FOR <= Duration::from_secs(10), "{RATE_FAILED_FOR:?}");
    assert_eq!(HUB_RATE_ASKS, 2);
    assert_eq!(
        QuoteRateAskSpecCtrader::MEMORY,
        QuoteRateMemoryConfig {
            reuse: RATE_REUSE_FOR,
            failed: RATE_FAILED_FOR,
            remember_no_pair: true
        }
    );
}

/// The words of a deposit the broker's asset list does not name, which the gatewayhub's assumption says once.
#[test]
fn an_unnamed_deposit_is_said_in_ctraders_words() {
    assert_eq!(deposit_named(Some("EUR".into())), Ok("EUR".to_string()));
    assert_eq!(
        deposit_named(None).expect_err("unnamed"),
        CtraderPropertiesRefusal::DepositUnnamed.to_string()
    );
}
