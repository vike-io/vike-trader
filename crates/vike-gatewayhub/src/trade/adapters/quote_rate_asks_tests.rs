//! The guard in front of a costly rate ask ([`QuoteRateAsks`], [`QuoteRateAsksHub`]) over cTrader's spec
//! (`vike_ctrader::symbol_properties::QuoteRateAskSpecCtrader`) and the bridge's rate
//! (`vike_ctrader::symbol_properties::quoted_rate`), worded as the lane words it. The quotes are DOCS-SHAPED (the Open
//! API states bid and ask in 1/100000 of a price unit, <https://help.ctrader.com/open-api/messages/>): nothing was
//! captured from a demo account.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use crate::trade::session::properties_refusal_code;
use vike_ctrader::account::rate_of_quote;
use vike_ctrader::symbol_properties::{QuoteRateAskSpecCtrader, RATE_FAILED_FOR, RATE_REUSE_FOR};
use vike_gatewayhub_client::proto::PROPERTIES_BUSY;
use vike_model::{AccountPerQuoteLeg, QuoteRate, QuoteRateError};

use super::*;

const ASKED_AT_MS: i64 = 1_700_000_000_123;

/// The bridge's rate, worded as the lane words it.
fn quoted_rate(
    quote: &str,
    deposit: &str,
    read: impl FnOnce(&str) -> Result<(QuoteRate, String), QuoteRateError>,
) -> Result<Option<QuoteRate>, String> {
    vike_ctrader::symbol_properties::quoted_rate(quote, deposit, read)
        .map_err(|e| super::super::symbol_facts::worded("ctrader", e))
}

/// A USD account's venue: USDJPY 150.09 / 150.11 (USD is the BASE of the pair), GBPUSD 1.34999 / 1.35001 (the
/// quote currency is the BASE), nothing for any other currency.
fn usd_account(quote: &str) -> Result<(QuoteRate, String), QuoteRateError> {
    priced_for("USD", quote)
}

/// [`usd_account`] for an account whose deposit currency is `account_currency` as the SESSION states it (the pair is
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

fn fresh_memory() -> (QuoteRateAsks<QuoteRateAskSpecCtrader>, QuoteRateAsksHub) {
    (QuoteRateAsks::<QuoteRateAskSpecCtrader>::default(), QuoteRateAsksHub::default())
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
            Err(QuoteRateError::Unavailable(
                "ctrader: symbols: timed out waiting for payloadType 2101".into(),
            ))
        })
    };
    let first = quoted_rate("JPY", "USD", failing).expect_err("a mute venue");
    assert!(
        first.contains("timed out") && properties_refusal_code(&first).is_transient(),
        "{first}"
    );
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
    let (expired, hub) = (
        QuoteRateAsks::<QuoteRateAskSpecCtrader>::with_windows(RATE_REUSE_FOR, Duration::ZERO),
        QuoteRateAsksHub::default(),
    );
    let tries = Cell::new(0);
    for _ in 0..3 {
        let _ = expired.read(&hub, "JPY", || {
            tries.set(tries.get() + 1);
            Err(QuoteRateError::Unavailable("ctrader: down".into()))
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

    let stale =
        QuoteRateAsks::<QuoteRateAskSpecCtrader>::with_windows(Duration::ZERO, RATE_FAILED_FOR);
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
/// refused in the `PROPERTIES_BUSY` words WITHOUT asking the venue (a transient answer), and that refusal is not
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
        assert!(
            busy.contains(PROPERTIES_BUSY) && properties_refusal_code(&busy).is_transient(),
            "{busy}"
        );
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
/// `PROPERTIES_BUSY` words without asking, so cTrader never holds more than half of the hub's four lookup threads;
/// the count returns to zero when they end.
#[test]
fn the_hub_runs_at_most_two_rate_asks_at_once() {
    assert_eq!(vike_ctrader::symbol_properties::HUB_RATE_ASKS, 2);
    let hub = QuoteRateAsksHub::default();
    let (a, b, c) = (
        QuoteRateAsks::<QuoteRateAskSpecCtrader>::default(),
        QuoteRateAsks::<QuoteRateAskSpecCtrader>::default(),
        QuoteRateAsks::<QuoteRateAskSpecCtrader>::default(),
    );
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
                matches!(&busy, QuoteRateError::Unavailable(w) if w.contains(PROPERTIES_BUSY) && w.contains("hub")),
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
