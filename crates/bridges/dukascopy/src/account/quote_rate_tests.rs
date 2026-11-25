//! `crate::account::quote_rate`'s tests: the ORIENTATION as goldens (the numbers say which way each points), the pair
//! choice over the account's list (the fixture, `crate::test_support::listed`), every unusable tick refused in words, and the whole path over a bus with a scripted
//! sidecar. DOCS-SHAPED: the ticks are what the vendor's `ITick` states (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>:
//! `getBid`, `getAsk`, `getTime`); no tick of a conversion pair was captured from a live login.

use std::sync::Arc;

use crate::feed::{FEATURE_TICK_ACKS, FEATURE_TICKS, MAX_TICK_AGE_MS};
use crate::refusals::DukascopyPriceLaneRefusal;
use crate::test_support::attached;
use crate::transport::SidecarCommand;
use crate::transport::encode_line;

use vike_model::AccountPerQuoteLeg;

use super::*;

const NOW: i64 = 1_760_000_000_000;

/// "Not now", in the rate's words: `why`'s text.
fn unavailable(why: impl std::fmt::Display) -> QuoteRateError {
    QuoteRateError::Unavailable(why.to_string())
}

/// No tick of `pair` arrived within `millis` of asking for it.
fn no_tick(pair: &str, millis: u128) -> QuoteRateError {
    unavailable(DukascopyPriceLaneRefusal::NoTickInTime { symbol: pair.to_string(), millis })
}

fn tick(symbol: &str, bid: f64, ask: f64) -> PriceTick {
    PriceTick { symbol: symbol.into(), bid, ask, timestamp: NOW }
}

/// The DIRECT pair of the account's list that converts `quote` into `account`.
fn pair(quote: &str, account: &str) -> ConversionLeg<String> {
    match conversion_route(&crate::test_support::listed(), quote, account).expect("a pair") {
        ConversionRoute::Direct(leg) => leg,
        other => panic!("{quote}/{account}: no direct pair: {other:?}"),
    }
}

/// A leg's pair and its orientation.
fn shape(leg: &ConversionLeg<String>) -> (&str, AccountPerQuoteLeg) {
    (leg.pair.as_str(), leg.orientation)
}

/// `x` is `want` up to a relative `1e-12`: the figure is a closed form that differs from the code's only in
/// association, ten thousand times tighter than the 150x it must tell apart.
fn close(x: f64, want: f64) -> bool {
    (x - want).abs() <= 1e-12 * want.abs().max(1.0)
}

// ---- the pair, over the account's list ----------------------------------------------------------------

#[test]
fn the_pair_that_converts_a_quote_currency_points_the_way_the_table_has_it() {
    // USD account: the dollar is the BASE of the yen pair and the QUOTE of the pound pair
    assert_eq!(shape(&pair("JPY", "USD")), ("USDJPY", AccountPerQuoteLeg::AccountIsBase));
    assert_eq!(shape(&pair("GBP", "USD")), ("GBPUSD", AccountPerQuoteLeg::QuoteIsBase));
    assert_eq!(pair("TRY", "USD").orientation, AccountPerQuoteLeg::AccountIsBase);
    assert_eq!(pair("AUD", "USD").orientation, AccountPerQuoteLeg::QuoteIsBase);
    // a lower-case or padded code is the same code
    assert_eq!(pair(" jpy ", "usd").pair, "USDJPY");
    // another account currency
    assert_eq!(shape(&pair("JPY", "EUR")), ("EURJPY", AccountPerQuoteLeg::AccountIsBase));
    assert_eq!(shape(&pair("EUR", "GBP")), ("EURGBP", AccountPerQuoteLeg::QuoteIsBase));
    assert_eq!(shape(&pair("USD", "EUR")), ("EURUSD", AccountPerQuoteLeg::AccountIsBase));
    assert_eq!(shape(&pair("USD", "JPY")), ("USDJPY", AccountPerQuoteLeg::QuoteIsBase));
}

/// Every DIRECT pair this picks is a FOREX row of the list whose two sides are exactly the two currencies, for EVERY
/// ordered pair of currencies the list's FOREX rows are made of: no orientation is a guess.
#[test]
fn every_pair_picked_has_the_two_currencies_on_its_two_sides() {
    let listed = crate::test_support::listed();
    let rows: Vec<_> = listed.iter().filter(|r| r.kind == InstrumentType::Forex).collect();
    let mut currencies: Vec<String> = rows
        .iter()
        .flat_map(|r| r.pair.split('/').map(str::to_string).collect::<Vec<_>>())
        .collect();
    currencies.sort();
    currencies.dedup();
    let mut found = 0;
    for quote in &currencies {
        for account in &currencies {
            let Ok(ConversionRoute::Direct(pair)) =
                conversion_route(&crate::test_support::listed(), quote, account)
            else {
                continue;
            };
            found += 1;
            let row = rows.iter().find(|r| r.symbol == pair.pair).expect("a table row");
            let (base, counter) = row.pair.split_once('/').expect("a slashed pair");
            match pair.orientation {
                AccountPerQuoteLeg::QuoteIsBase => {
                    assert_eq!((base, counter), (quote.as_str(), account.as_str()))
                }
                AccountPerQuoteLeg::AccountIsBase => {
                    assert_eq!((base, counter), (account.as_str(), quote.as_str()))
                }
            }
        }
    }
    // each row converts in both directions (QUOTEACCOUNT for one, ACCOUNTQUOTE for the other)
    assert_eq!(found, rows.len() * 2, "{found} pairs from {} rows", rows.len());
}

/// **A quote currency the list has no pair of with the account's is converted through USD**: two pairs, each read by
/// its own orientation (owner, 2026-10-26: "a user must be able to trade ANY pair of every venue"; the shared pick of
/// 2026-11-22). A zloty on a yen ledger: the zloty into dollars by `USDPLN` (inverted), the dollar into yen by `USDJPY`
/// (as it stands). A pair listed BOTH ways round is refused, never chosen between.
#[test]
fn a_quote_with_no_pair_of_the_account_is_converted_through_usd_by_two_pairs() {
    let listed = crate::test_support::listed();
    match conversion_route(&listed, "PLN", "JPY").expect("a route through USD") {
        ConversionRoute::ThroughHub([first, second]) => {
            assert_eq!(
                (first.pair.as_str(), first.from.as_str(), first.to.as_str(), first.orientation),
                ("USDPLN", "PLN", "USD", AccountPerQuoteLeg::AccountIsBase)
            );
            assert_eq!(
                (
                    second.pair.as_str(),
                    second.from.as_str(),
                    second.to.as_str(),
                    second.orientation
                ),
                ("USDJPY", "USD", "JPY", AccountPerQuoteLeg::QuoteIsBase)
            );
        }
        other => panic!("PLN/JPY: {other:?}"),
    }
    let mut both = listed.clone();
    let mut twin = listed.iter().find(|r| r.symbol == "USDJPY").cloned().expect("USDJPY");
    (twin.symbol, twin.pair) = ("JPYUSD".into(), "JPY/USD".into());
    both.push(twin);
    let both_ways =
        DukascopyRateRefusal::BothOrientations { from: "JPY".to_string(), to: "USD".to_string() };
    assert_eq!(
        conversion_route(&both, "JPY", "USD"),
        Err(QuoteRateError::NoPair(both_ways.to_string()))
    );
}

#[test]
fn no_pair_is_refused_in_words_and_never_guessed() {
    let same = |quote: &str| DukascopyRateRefusal::SameCurrency { quote: quote.to_string() };
    let none = |quote: &str| DukascopyRateRefusal::NoPair {
        quote: quote.to_string(),
        account: "USD".to_string(),
    };
    let not_codes = |quote_ccy: &str, account_ccy: &str| DukascopyRateRefusal::NotCurrencyCodes {
        quote_ccy: quote_ccy.to_string(),
        account_ccy: account_ccy.to_string(),
    };
    for (quote, account, refusal) in [
        ("USD", "USD", same("USD")),
        ("usd", " USD ", same("USD")),
        (
            "PLN",
            "XAU",
            DukascopyRateRefusal::NoPair { quote: "PLN".to_string(), account: "XAU".to_string() },
        ),
        ("XAU", "USD", none("XAU")), // a metal is not a currency pair of the list
        ("", "USD", not_codes("", "USD")),
        ("JPY", "", not_codes("JPY", "")),
        (" ", "USD", not_codes(" ", "USD")),
        // a code is the engine's (ruling 6): any word, so these reach the list and find no pair
        ("JP", "USD", none("JP")),
        ("USDT", "USD", none("USDT")),
        ("J1Y", "USD", none("J1Y")),
    ] {
        assert_eq!(
            conversion_route(&crate::test_support::listed(), quote, account),
            Err(QuoteRateError::NoPair(refusal.to_string())),
            "{quote}/{account}"
        );
    }
}

// ---- the orientation, as goldens ------------------------------------------------------------------

/// USD account, JPY quote: USDJPY at 150.10 is JPY per dollar, so a yen is a FRACTION of a dollar.
#[test]
fn a_jpy_quote_on_a_usd_account_is_a_fraction_of_a_dollar_per_yen_not_yen_per_dollar() {
    let pair = pair("JPY", "USD");
    let r = leg_rate(&pair, &tick("USDJPY", 150.09, 150.11)).expect("a rate");
    assert!(r.get() < 0.01 && r.get() > 0.006, "{} is yen-per-dollar, the flipped rate", r.get());
    assert!(close(r.get(), 1.0 / 150.10), "the inverse of the MID: {}", r.get());
    // 100,000 units of USDJPY at 150.10 are 15,010,000 yen = 100,000 dollars
    assert!((r.to_account(100_000.0 * 150.10) - 100_000.0).abs() < 1e-6);
    // a 0.10 move on 100,000 units is 10,000 yen = 66.62 dollars, NOT 10,000 dollars
    let pnl = r.to_account(100_000.0 * 0.10);
    assert_eq!((pnl * 100.0).round() / 100.0, 66.62, "{pnl}");
    assert!((pnl - 10_000.0).abs() > 1.0);
}

/// USD account, GBP quote: GBPUSD at 1.35 is dollars per pound, so a pound is MORE than a dollar.
#[test]
fn a_gbp_quote_on_a_usd_account_is_more_than_one_dollar_per_pound() {
    let pair = pair("GBP", "USD");
    let r = leg_rate(&pair, &tick("GBPUSD", 1.3499, 1.3501)).expect("a rate");
    assert!(r.get() > 1.0, "{} is pounds-per-dollar, the flipped rate", r.get());
    assert!(close(r.get(), 1.35), "{}", r.get());
    // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars (1.35x low if left raw)
    assert!((r.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    assert!((r.to_account(85_000.0) - 85_000.0).abs() > 1.0);
}

/// A EUR account: the same pair table, the other way round. A yen is 1/162 of a euro (EURJPY is the BASE-account
/// pair), and a dollar is 1/1.10 of a euro (EURUSD is the base-account pair too).
#[test]
fn a_third_account_currency_points_the_same_rules_the_same_way() {
    let eurjpy = pair("JPY", "EUR");
    let r = leg_rate(&eurjpy, &tick("EURJPY", 161.9, 162.1)).expect("a rate");
    assert!(close(r.get(), 1.0 / 162.0), "{}", r.get());
    let eurusd = pair("USD", "EUR");
    let r = leg_rate(&eurusd, &tick("EURUSD", 1.0999, 1.1001)).expect("a rate");
    assert!(close(r.get(), 1.0 / 1.1), "{}", r.get());
    // and a GBP account trading a EUR-quoted pair: EURGBP is the QUOTE-base pair, the euro is worth 0.85 pounds
    let eurgbp = pair("EUR", "GBP");
    let r = leg_rate(&eurgbp, &tick("EURGBP", 0.8499, 0.8501)).expect("a rate");
    assert!(close(r.get(), 0.85), "{}", r.get());
}

/// The mid, not a side: the same quote read at its bid, its ask and its mid differs, and only the mid is used.
#[test]
fn the_rate_is_the_mid_of_the_quote_not_a_side() {
    let pair = pair("JPY", "USD");
    let wide = tick("USDJPY", 150.0, 150.2);
    let r = leg_rate(&pair, &wide).expect("a rate").get();
    assert!(close(r, 1.0 / 150.1));
    assert!(r < 1.0 / 150.0 && r > 1.0 / 150.2, "strictly between the two sides");
}

// ---- every unusable tick is refused ---------------------------------------------------------------------

#[test]
fn a_tick_that_states_no_rate_is_refused_in_words() {
    let (yen, gbp) = (pair("JPY", "USD"), pair("GBP", "USD"));
    for (bid, ask) in [
        (0.0, 0.0),
        (-150.0, 150.0),
        (-1.0, -2.0),
        (f64::NAN, 150.0),
        (150.0, f64::NAN),
        (f64::INFINITY, 150.0),
        (150.0, f64::INFINITY),
        (f64::NEG_INFINITY, f64::INFINITY),
    ] {
        let unusable = DukascopyRateRefusal::UnusableTick { pair: "USDJPY".to_string(), bid, ask };
        assert_eq!(
            leg_rate(&yen, &tick("USDJPY", bid, ask)),
            Err(unavailable(unusable)),
            "{bid} {ask}"
        );
    }
    // the QuoteIsBase leg too (a zero mid is no rate, not zero dollars per pound)
    assert!(leg_rate(&gbp, &tick("GBPUSD", 0.0, 0.0)).is_err());
    // a tick of ANOTHER pair prices nothing here
    let wrong =
        DukascopyRateRefusal::WrongTick { tick: "EURUSD".to_string(), pair: "USDJPY".to_string() };
    assert_eq!(leg_rate(&yen, &tick("EURUSD", 1.1, 1.1002)), Err(unavailable(wrong)));
}

// ---- the whole path, over a bus ------------------------------------------------------------------------

fn subscribe(symbol: &str) -> String {
    encode_line(&SidecarCommand::Subscribe { symbols: vec![symbol.to_string()] })
}

/// Ask for a rate on a thread, deliver `bid`/`ask` for the conversion pair once its `subscribe` is on the wire.
fn rate_over_a_bus(
    quote: &'static str,
    account: &'static str,
    pair: &'static str,
    bid: f64,
    ask: f64,
    time: i64,
) -> Result<QuoteRate, QuoteRateError> {
    let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quote_rate(&bus, quote, account, Duration::from_secs(10)).map_err(|no| no.error)
        })
    };
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe(pair)) {
        assert!(std::time::Instant::now() < until, "the subscribe never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription(pair, true, "");
    bus.deliver(pair, bid, ask, time, NOW);
    asker.join().expect("no panic")
}

#[test]
fn the_rate_over_a_bus_is_the_inverse_mid_of_the_tick_with_the_ticks_own_time() {
    let got = rate_over_a_bus("JPY", "USD", "USDJPY", 150.09, 150.11, NOW - 250).expect("a rate");
    assert_eq!(got.quote_currency, "JPY");
    assert!(close(got.account_per_quote.get(), 1.0 / 150.10), "{}", got.account_per_quote.get());
    assert_eq!(got.as_of_ms, NOW - 250, "the tick's own stamp, not the moment it was read");
    // 66.62, not 10,000
    let pnl = got.account_per_quote.to_account(10_000.0);
    assert_eq!((pnl * 100.0).round() / 100.0, 66.62, "{pnl}");

    let gbp = rate_over_a_bus("GBP", "USD", "GBPUSD", 1.3499, 1.3501, NOW).expect("a rate");
    assert!(close(gbp.account_per_quote.get(), 1.35));
    assert_eq!((gbp.quote_currency.as_str(), gbp.as_of_ms), ("GBP", NOW));
}

/// Wait until `pair`'s `subscribe` is on the wire, then acknowledge it and deliver one tick of it stamped `time`.
fn tick_once_subscribed(
    bus: &QuoteBus,
    wire: &crate::test_support::Wire,
    pair: &str,
    bid: f64,
    ask: f64,
    time: i64,
) {
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe(pair)) {
        assert!(std::time::Instant::now() < until, "the subscribe of {pair} never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription(pair, true, "");
    bus.deliver(pair, bid, ask, time, NOW);
}

/// **A zloty on a yen ledger is about 37.5 yen, composed through USD from two ticks, never 1/37.5**: `1 / mid(USDPLN)`
/// dollars per zloty times `mid(USDJPY)` yen per dollar, stamped with the OLDER tick's own time. The second pair is asked
/// for only once the first answered (two of the bus's rate places, two waits in a row).
#[test]
fn a_zloty_on_a_yen_ledger_is_composed_through_usd_and_stamped_with_the_older_tick() {
    let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quote_rate(&bus, "PLN", "JPY", Duration::from_secs(10)).map_err(|no| no.error)
        })
    };
    tick_once_subscribed(&bus, &wire, "USDPLN", 3.9999, 4.0001, NOW - 500);
    tick_once_subscribed(&bus, &wire, "USDJPY", 149.99, 150.01, NOW - 100);
    let got = asker.join().expect("no panic").expect("a rate");
    assert_eq!(got.quote_currency, "PLN");
    assert!(
        close(got.account_per_quote.get(), (1.0 / 4.0) * 150.0),
        "{}",
        got.account_per_quote.get()
    );
    assert!(got.account_per_quote.get() > 1.0, "a zloty is many yen: the flipped rate is 1/37.5");
    assert_eq!(got.as_of_ms, NOW - 500, "the older of the two ticks");
}

/// A cross whose second pair sends no tick is "not now", naming that leg, never a rate made of one leg.
#[test]
fn a_cross_whose_second_leg_has_no_tick_is_unavailable_naming_the_leg() {
    let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quote_rate(&bus, "PLN", "JPY", Duration::from_secs(2)).map_err(|no| no.error)
        })
    };
    tick_once_subscribed(&bus, &wire, "USDPLN", 3.9999, 4.0001, NOW);
    let leg = DukascopyRateRefusal::LegHasNoRate {
        from: "PLN".to_string(),
        to: "JPY".to_string(),
        leg_from: "USD".to_string(),
        leg_to: "JPY".to_string(),
        why: DukascopyPriceLaneRefusal::NoTickInTime {
            symbol: "USDJPY".to_string(),
            millis: 2_000,
        }
        .to_string(),
    };
    assert_eq!(asker.join().expect("no panic"), Err(unavailable(leg)));
}

/// No pair is a verdict on the INSTRUMENT and touches the bus not at all; no tick is a verdict on the MOMENT.
#[test]
fn no_pair_asks_the_sidecar_nothing_and_no_tick_is_unavailable_not_a_verdict() {
    let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    assert!(matches!(
        quote_rate(&bus, "PLN", "XAU", Duration::from_millis(5)).map_err(|no| no.error),
        Err(QuoteRateError::NoPair(_))
    ));
    assert!(matches!(
        quote_rate(&bus, "USD", "USD", Duration::from_millis(5)).map_err(|no| no.error),
        Err(QuoteRateError::NoPair(_))
    ));
    assert!(wire.lines().is_empty(), "a pair that cannot exist is never subscribed");

    assert_eq!(
        quote_rate(&bus, "JPY", "USD", Duration::from_millis(40)).map_err(|no| no.error),
        Err(no_tick("USDJPY", 40))
    );
    assert_eq!(wire.lines(), [subscribe("USDJPY")]);

    // no sidecar at all, so no list was stated either: not now, never a pair guessed
    assert_eq!(
        quote_rate(&QuoteBus::new(), "JPY", "USD", Duration::from_millis(5)).map_err(|no| no.error),
        Err(unavailable(crate::instrument::grid::not_listed()))
    );
    // a list an earlier sidecar stated, and no sidecar up now
    let listed_only = QuoteBus::new();
    listed_only.set_instrument_list(crate::test_support::listed());
    let no_sidecar = DukascopyPriceLaneRefusal::NoSidecar {
        symbol: "USDJPY".to_string(),
        purpose: "priced for a conversion",
    };
    assert_eq!(
        quote_rate(&listed_only, "JPY", "USD", Duration::from_millis(5)).map_err(|no| no.error),
        Err(unavailable(no_sidecar))
    );
}

/// **A pair that could not be SUBSCRIBED is said as a value beside the refusal** (`NoRate::pair_not_subscribed`, what
/// `crate::ledger` warns on once an hour), direct and through USD alike; a pair subscribed and silent is not one.
#[test]
fn a_pair_that_could_not_be_subscribed_is_a_value_of_the_refusal() {
    for (quote, account) in [("JPY", "USD"), ("PLN", "JPY")] {
        let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
        wire.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let no =
            quote_rate(&bus, quote, account, Duration::from_millis(5)).expect_err("a closed pipe");
        assert!(no.pair_not_subscribed, "{quote} in {account}: {no:?}");
        assert!(matches!(no.error, QuoteRateError::Unavailable(_)), "{no:?}");
    }
    let (bus, _wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    let quiet = quote_rate(&bus, "JPY", "USD", Duration::from_millis(5)).expect_err("no tick");
    assert!(!quiet.pair_not_subscribed, "{quiet:?}");
}

/// A tick the price lane would not quote never becomes a rate: its own time is outside the bus's bound, so the
/// bus drops it and the wait ends in the same words as a quiet market.
#[test]
fn a_tick_outside_the_age_bound_is_never_a_rate() {
    let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quote_rate(&bus, "JPY", "USD", Duration::from_millis(300)).map_err(|no| no.error)
        })
    };
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe("USDJPY")) {
        assert!(std::time::Instant::now() < until, "the subscribe never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.deliver("USDJPY", 150.09, 150.11, NOW - MAX_TICK_AGE_MS - 1, NOW);
    assert_eq!(asker.join().expect("no panic"), Err(no_tick("USDJPY", 300)));
}

/// **A tick the bus lets through but the engine would refuse as `quote-rate-future` says so, in words naming the
/// clock.** The bus accepts a stamp up to ten seconds ahead of this box's clock; the engine takes a rate stamped at
/// most five ahead of its own. A tick between the two used to reach the engine and come back as an opaque refusal.
#[test]
fn a_tick_stamped_further_ahead_than_the_engine_accepts_is_refused_naming_the_clock() {
    for (ahead_ms, accepted) in [(3_000, true), (7_000, false)] {
        let (bus, wire) = attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], None);
        let asker = {
            let bus = Arc::clone(&bus);
            std::thread::spawn(move || {
                quote_rate(&bus, "JPY", "USD", Duration::from_secs(10)).map_err(|no| no.error)
            })
        };
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while !wire.lines().contains(&subscribe("USDJPY")) {
            assert!(std::time::Instant::now() < until, "the subscribe never went out");
            std::thread::sleep(Duration::from_millis(5));
        }
        bus.subscription("USDJPY", true, "");
        let now = vike_model::now_ms();
        bus.deliver("USDJPY", 150.09, 150.11, now + ahead_ms, now);
        match (asker.join().expect("no panic"), accepted) {
            (Ok(rate), true) => assert_eq!(rate.as_of_ms, now + ahead_ms),
            (Err(QuoteRateError::Unavailable(text)), false) => {
                // the rate's own refusal for a tick stamped too far ahead; its figure is the skew measured when the
                // tick was read, a few milliseconds under the 7 s it was stamped ahead
                let ahead = |tenths: u32| DukascopyRateRefusal::TickAhead {
                    pair: "USDJPY".to_string(),
                    ahead_secs: f64::from(tenths) / 10.0,
                    max_secs: vike_execution::QUOTE_RATE_FUTURE_SKEW_MS / 1000,
                };
                assert!((60..=70).any(|tenths| text == ahead(tenths).to_string()), "{text}");
            }
            (other, _) => panic!("{ahead_ms} ms ahead: {other:?}"),
        }
    }
}
