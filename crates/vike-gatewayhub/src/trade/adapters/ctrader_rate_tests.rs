//! [`quoted_rate`] over the bridge's own rate type, built the way the bridge builds it from a two-sided quote
//! (`vike_ctrader::quote_rate::rate_of_quote`). The quotes are DOCS-SHAPED (the Open API states bid and ask in
//! 1/100000 of a price unit, <https://help.ctrader.com/open-api/messages/>): nothing was captured from a demo
//! account. The goldens are the decision's own, in the orientation the engine takes: account-currency units per
//! ONE unit of the quote currency.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use vike_ctrader::quote_rate::{Leg, QuoteRate, RateError, rate_of_quote};
use vike_gatewayhub_client::proto::{FACTS_BUSY, facts_refusal_is_transient};

use super::*;

const ASKED_AT_MS: i64 = 1_700_000_000_123;

/// A USD account's venue: USDJPY 150.09 / 150.11 (USD is the BASE of the pair), GBPUSD 1.34999 / 1.35001 (the
/// quote currency is the BASE), nothing for any other currency.
fn usd_account(quote: &str) -> Result<QuoteRate, RateError> {
    priced_for("USD", quote)
}

/// [`usd_account`] for an account whose deposit currency is `account_ccy` as the SESSION states it (the pair is
/// still the USD one: the session is what names the account).
fn priced_for(account_ccy: &str, quote: &str) -> Result<QuoteRate, RateError> {
    let (leg, bid, ask) = match quote {
        "JPY" => (Leg::AccountIsBase, 15_009_000, 15_011_000),
        "GBP" => (Leg::QuoteIsBase, 134_999, 135_001),
        other => {
            return Err(RateError::NoPair(format!("no forex pair between {other} and USD")));
        }
    };
    Ok(QuoteRate {
        quote_ccy: quote.to_string(),
        account_ccy: account_ccy.to_string(),
        account_per_quote: rate_of_quote(leg, bid, ask)?,
        as_of_ms: ASKED_AT_MS,
    })
}

/// **The orientation, by value**: a JPY quote is a fraction of a dollar, a GBP quote is above one, in any case
/// of the currency code, and the moment is the one the read stamped.
#[test]
fn a_yen_is_a_fraction_of_a_dollar_and_a_pound_is_more_than_one() {
    for jpy in ["JPY", "jpy", " Jpy "] {
        let rate = quoted_rate(jpy, "USD", usd_account).expect("priced").expect("foreign");
        assert_eq!((rate.quote_ccy.as_str(), rate.as_of_ms), ("JPY", ASKED_AT_MS), "{jpy}");
        assert!(rate.account_per_quote < 0.01 && rate.account_per_quote > 0.006, "{jpy}");
        // 100,000 units of USDJPY at 150.10 are 15,010,000 yen = 100,000 dollars; a 0.10 move is 66.62.
        assert!((rate.account_per_quote * 100_000.0 * 150.10 - 100_000.0).abs() < 1e-6, "{jpy}");
        assert!((rate.account_per_quote * 100_000.0 * 0.10 - 66.62).abs() < 0.01, "{jpy}");
    }
    let rate = quoted_rate("GBP", "USD", usd_account).expect("priced").expect("foreign");
    assert_eq!((rate.quote_ccy.as_str(), rate.account_per_quote), ("GBP", 1.35));
    // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars (understated 1.35x if raw).
    assert!((rate.account_per_quote * 85_000.0 - 114_750.0).abs() < 1e-6);
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
    let unnamed = quoted_rate("  ", "USD", usd_account).expect_err("no quote currency");
    assert!(unnamed.contains("names no quote currency") && unnamed.contains("(USD)"), "{unnamed}");
    assert!(!facts_refusal_is_transient(&unnamed), "a verdict on the row: {unnamed}");

    for failed in [
        RateError::Unavailable("reading USDJPY: cTrader error CH_ACCESS_TOKEN_INVALID".into()),
        RateError::BadQuote("the conversion pair's quote is crossed".into()),
    ] {
        let why = quoted_rate("JPY", "USD", |_| Err(failed.clone())).expect_err("no rate");
        assert!(why.contains("no rate for JPY in USD"), "{why}");
        assert!(why.contains(&failed.to_string()), "{why}");
        assert!(facts_refusal_is_transient(&why), "{why}");
    }

    let no_pair = quoted_rate("NZD", "USD", usd_account).expect_err("no pair");
    assert!(no_pair.contains("no forex pair between NZD and USD"), "{no_pair}");
    assert!(
        no_pair.contains("NOT admitted") && no_pair.contains("never valued at par"),
        "{no_pair}"
    );
    assert!(!facts_refusal_is_transient(&no_pair), "the node remembers a verdict: {no_pair}");

    // A read that answers for another currency than the one asked is the flipped-rate class of bug.
    let wrong = quoted_rate("GBP", "USD", |_| usd_account("JPY")).expect_err("wrong currency");
    assert!(
        wrong.contains("the read answered for JPY") && facts_refusal_is_transient(&wrong),
        "{wrong}"
    );
}

/// **Credentials swapped under one label**: the listing (kept minutes) says the account deposits in USD, the
/// session that priced the rate was for an EUR account (a rate that is par for EUR). The rate is REFUSED, as a
/// failure of the rate (the node does not remember it as a verdict on the symbol), never booked at par on a
/// USD engine; the same rate read for the listing's own currency is a rate.
#[test]
fn a_rate_priced_for_another_accounts_currency_is_refused() {
    let swapped = quoted_rate("JPY", "USD", |q| priced_for("EUR", q)).expect_err("another account");
    assert!(
        swapped.contains("an account in EUR") && swapped.contains("listing says USD"),
        "{swapped}"
    );
    assert!(facts_refusal_is_transient(&swapped), "{swapped}");

    let own = quoted_rate("JPY", "USD", |q| priced_for("usd", q)).expect("own").expect("foreign");
    assert_eq!(own.quote_ccy, "JPY");
}

fn fresh_memory() -> (RateMemory, HubRateAsks) {
    (RateMemory::default(), HubRateAsks::default())
}

/// **A failed ask is not dialled again inside its window**: the second ask of the same currency (the next order,
/// the node's timer, the commit after a look) gets the remembered failure with NO session, whatever the failure
/// was; after the window it asks again.
#[test]
fn a_failed_ask_is_not_redialled_within_the_memory_window() {
    let (memory, hub) = fresh_memory();
    let dialled = Cell::new(0);
    let failing = |q: &str| {
        memory.read(&hub, q, || {
            dialled.set(dialled.get() + 1);
            Err(RateError::Unavailable("symbols: timed out waiting for payloadType 2101".into()))
        })
    };
    let first = quoted_rate("JPY", "USD", failing).expect_err("a mute venue");
    assert!(first.contains("timed out") && facts_refusal_is_transient(&first), "{first}");
    for _ in 0..5 {
        let again = quoted_rate("jpy", "USD", failing).expect_err("remembered");
        assert_eq!(again, first, "the same failure, from memory");
    }
    assert_eq!(dialled.get(), 1, "one session for six asks");

    // Another currency is its own ask.
    let _ = quoted_rate("GBP", "USD", failing);
    assert_eq!(dialled.get(), 2);

    // A verdict ("this account lists no pair") is remembered as well, and also costs one session.
    let nzd = Cell::new(0);
    for _ in 0..3 {
        let why = quoted_rate("NZD", "USD", |q| {
            memory.read(&hub, q, || {
                nzd.set(nzd.get() + 1);
                usd_account("NZD")
            })
        })
        .expect_err("no pair");
        assert!(why.contains("no forex pair between NZD and USD"), "{why}");
    }
    assert_eq!(nzd.get(), 1);

    // Past the window it asks again, and a success replaces the failure.
    let (expired, hub) =
        (RateMemory::with_windows(RATE_REUSE_FOR, Duration::ZERO), HubRateAsks::default());
    let tries = Cell::new(0);
    for _ in 0..3 {
        let _ = expired.read(&hub, "JPY", || {
            tries.set(tries.get() + 1);
            Err(RateError::Unavailable("down".into()))
        });
    }
    assert_eq!(tries.get(), 3, "a zero window remembers nothing");
}

/// **The order's look and its commit are ONE read**: a rate read a moment ago is handed out again with the
/// moment it was ASKED (so no age is hidden), and asked afresh once the window has passed.
#[test]
fn a_rate_just_read_is_reused_with_its_own_stamp_and_then_asked_afresh() {
    let (memory, hub) = fresh_memory();
    let dialled = Cell::new(0);
    let read = || {
        quoted_rate("GBP", "USD", |q| {
            memory.read(&hub, q, || {
                dialled.set(dialled.get() + 1);
                usd_account(q)
            })
        })
        .expect("priced")
        .expect("foreign")
    };
    let look = read();
    let commit = read();
    assert_eq!(dialled.get(), 1, "one session for the look and the commit");
    assert_eq!(look, commit);
    assert_eq!(commit.as_of_ms, ASKED_AT_MS, "the stamp is the ask's, never the reuse's");

    let stale = RateMemory::with_windows(Duration::ZERO, RATE_FAILED_FOR);
    let again = Cell::new(0);
    for _ in 0..2 {
        stale
            .read(&hub, "GBP", || {
                again.set(again.get() + 1);
                usd_account("GBP")
            })
            .expect("priced");
    }
    assert_eq!(again.get(), 2, "past its window a rate is asked afresh");
}

/// **One ask per account at a time**: while an account's ask is running, another asker of that account is
/// refused in the `FACTS_BUSY` words WITHOUT asking the venue (a transient answer), and that refusal is not
/// remembered; the place is given back when the ask ends, a panic included.
#[test]
fn an_account_has_one_ask_in_flight_and_the_place_comes_back() {
    let (memory, hub) = fresh_memory();
    let inner_asked = Cell::new(false);
    let outer = memory.read(&hub, "JPY", || {
        let busy = quoted_rate("GBP", "USD", |q| {
            memory.read(&hub, q, || {
                inner_asked.set(true);
                usd_account(q)
            })
        })
        .expect_err("the account's place is taken");
        assert!(busy.contains(FACTS_BUSY) && facts_refusal_is_transient(&busy), "{busy}");
        usd_account("JPY")
    });
    assert!(outer.is_ok());
    assert!(!inner_asked.get(), "the refused asker never reached the venue");

    // The refusal was not remembered: the same currency is asked now that the place is free.
    let after = quoted_rate("GBP", "USD", |q| memory.read(&hub, q, || usd_account(q)));
    assert!(after.expect("asked").is_some());

    // A panic inside an ask gives both places back (and remembers nothing).
    let (fresh, hub) = fresh_memory();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = fresh.read(&hub, "CHF", || panic!("a bridge panic"));
    }));
    assert!(panicked.is_err());
    assert_eq!(hub.0.load(std::sync::atomic::Ordering::SeqCst), 0, "the hub's place came back");
    assert!(fresh.read(&hub, "JPY", || usd_account("JPY")).is_ok(), "the account's place too");
}

/// **At most two asks in the hub**: with two accounts' asks running, a third account is refused in the
/// `FACTS_BUSY` words without asking, so cTrader never holds more than half of the hub's four lookup threads;
/// the count returns to zero when they end.
#[test]
fn the_hub_runs_at_most_two_rate_asks_at_once() {
    assert_eq!(HUB_RATE_ASKS, 2);
    let hub = HubRateAsks::default();
    let (a, b, c) = (RateMemory::default(), RateMemory::default(), RateMemory::default());
    let third_asked = Cell::new(false);
    let result = a.read(&hub, "JPY", || {
        b.read(&hub, "JPY", || {
            let busy = c
                .read(&hub, "JPY", || {
                    third_asked.set(true);
                    usd_account("JPY")
                })
                .expect_err("the hub's places are taken");
            assert!(
                matches!(&busy, RateError::Unavailable(w) if w.contains(FACTS_BUSY) && w.contains("hub")),
                "{busy}"
            );
            usd_account("JPY")
        })
    });
    assert!(result.is_ok());
    assert!(!third_asked.get());
    assert_eq!(hub.0.load(std::sync::atomic::Ordering::SeqCst), 0, "every place was given back");
    assert!(c.read(&hub, "JPY", || usd_account("JPY")).is_ok(), "now the third account is served");
}
