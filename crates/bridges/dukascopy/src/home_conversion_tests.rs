//! `crate::home_conversion`'s tests: the ORIENTATION as goldens (the numbers say which way each points), the pair
//! choice over the SDK table, every unusable tick refused in words, and the whole path over a bus with a scripted
//! sidecar. DOCS-SHAPED: the ticks are what the vendor's `ITick` states (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>:
//! `getBid`, `getAsk`, `getTime`); no tick of a conversion pair was captured from a live login.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::proto::Command;
use crate::proto::encode_line;
use crate::quotes::{FEATURE_TICK_ACKS, FEATURE_TICKS, LinePort, MAX_TICK_AGE_MS};

use super::*;

const NOW: i64 = 1_760_000_000_000;

fn tick(symbol: &str, bid: f64, ask: f64) -> PriceTick {
    PriceTick { symbol: symbol.into(), bid, ask, ts: NOW }
}

/// `x` is `want` up to a relative `1e-12`: the figure is a closed form that differs from the code's only in
/// association, ten thousand times tighter than the 150x it must tell apart.
fn close(x: f64, want: f64) -> bool {
    (x - want).abs() <= 1e-12 * want.abs().max(1.0)
}

// ---- the pair, over the SDK table ---------------------------------------------------------------------

#[test]
fn the_pair_that_converts_a_quote_currency_points_the_way_the_table_has_it() {
    let pair = |quote: &str, account: &str| conversion_pair(quote, account).expect("a pair");
    // USD account: the dollar is the BASE of the yen pair and the QUOTE of the pound pair
    assert_eq!(
        pair("JPY", "USD"),
        ConversionPair { symbol: "USDJPY".into(), leg: Leg::AccountIsBase }
    );
    assert_eq!(
        pair("GBP", "USD"),
        ConversionPair { symbol: "GBPUSD".into(), leg: Leg::QuoteIsBase }
    );
    assert_eq!(pair("TRY", "USD").leg, Leg::AccountIsBase);
    assert_eq!(pair("AUD", "USD").leg, Leg::QuoteIsBase);
    // a lower-case or padded code is the same code
    assert_eq!(pair(" jpy ", "usd").symbol, "USDJPY");
    // another account currency
    assert_eq!(
        pair("JPY", "EUR"),
        ConversionPair { symbol: "EURJPY".into(), leg: Leg::AccountIsBase }
    );
    assert_eq!(
        pair("EUR", "GBP"),
        ConversionPair { symbol: "EURGBP".into(), leg: Leg::QuoteIsBase }
    );
    assert_eq!(
        pair("USD", "EUR"),
        ConversionPair { symbol: "EURUSD".into(), leg: Leg::AccountIsBase }
    );
    assert_eq!(
        pair("USD", "JPY"),
        ConversionPair { symbol: "USDJPY".into(), leg: Leg::QuoteIsBase }
    );
}

/// Every pair this picks is a FOREX row of the table whose two sides are exactly the two currencies, for EVERY
/// ordered pair of currencies the table's FOREX rows are made of: no orientation is a guess.
#[test]
fn every_pair_picked_has_the_two_currencies_on_its_two_sides() {
    let rows: Vec<_> =
        jforex_instruments().iter().filter(|r| r.kind == JforexKind::Forex).collect();
    let mut currencies: Vec<String> = rows
        .iter()
        .flat_map(|r| r.pair.split('/').map(str::to_string).collect::<Vec<_>>())
        .collect();
    currencies.sort();
    currencies.dedup();
    let mut found = 0;
    for quote in &currencies {
        for account in &currencies {
            let Ok(pair) = conversion_pair(quote, account) else { continue };
            found += 1;
            let row = rows.iter().find(|r| r.symbol == pair.symbol).expect("a table row");
            let (base, counter) = row.pair.split_once('/').expect("a slashed pair");
            match pair.leg {
                Leg::QuoteIsBase => assert_eq!((base, counter), (quote.as_str(), account.as_str())),
                Leg::AccountIsBase => {
                    assert_eq!((base, counter), (account.as_str(), quote.as_str()))
                }
            }
        }
    }
    // each row converts in both directions (QUOTEACCOUNT for one, ACCOUNTQUOTE for the other)
    assert_eq!(found, rows.len() * 2, "{found} pairs from {} rows", rows.len());
}

#[test]
fn no_pair_is_refused_in_words_and_never_guessed() {
    for (quote, account, why) in [
        ("USD", "USD", "nothing to convert"),
        ("usd", " USD ", "nothing to convert"),
        ("PLN", "JPY", "neither PLN/JPY nor JPY/PLN"),
        ("XAU", "USD", "neither XAU/USD nor USD/XAU"), // a metal is not a currency pair of the table
        ("", "USD", "not both currency codes"),
        ("JPY", "", "not both currency codes"),
        ("JP", "USD", "not both currency codes"),
        ("JPYX", "USD", "not both currency codes"),
        ("J1Y", "USD", "not both currency codes"),
    ] {
        match conversion_pair(quote, account) {
            Err(RateError::NoPair(text)) => {
                assert!(text.contains(why), "{quote}/{account}: {text}")
            }
            other => panic!("{quote}/{account}: {other:?}"),
        }
    }
}

// ---- the orientation, as goldens ------------------------------------------------------------------

/// USD account, JPY quote: USDJPY at 150.10 is JPY per dollar, so a yen is a FRACTION of a dollar.
#[test]
fn a_jpy_quote_on_a_usd_account_is_a_fraction_of_a_dollar_per_yen_not_yen_per_dollar() {
    let pair = conversion_pair("JPY", "USD").expect("USDJPY");
    let r = pair.account_per_quote(&tick("USDJPY", 150.09, 150.11)).expect("a rate");
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
    let pair = conversion_pair("GBP", "USD").expect("GBPUSD");
    let r = pair.account_per_quote(&tick("GBPUSD", 1.3499, 1.3501)).expect("a rate");
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
    let eurjpy = conversion_pair("JPY", "EUR").expect("EURJPY");
    let r = eurjpy.account_per_quote(&tick("EURJPY", 161.9, 162.1)).expect("a rate");
    assert!(close(r.get(), 1.0 / 162.0), "{}", r.get());
    let eurusd = conversion_pair("USD", "EUR").expect("EURUSD");
    let r = eurusd.account_per_quote(&tick("EURUSD", 1.0999, 1.1001)).expect("a rate");
    assert!(close(r.get(), 1.0 / 1.1), "{}", r.get());
    // and a GBP account trading a EUR-quoted pair: EURGBP is the QUOTE-base pair, the euro is worth 0.85 pounds
    let eurgbp = conversion_pair("EUR", "GBP").expect("EURGBP");
    let r = eurgbp.account_per_quote(&tick("EURGBP", 0.8499, 0.8501)).expect("a rate");
    assert!(close(r.get(), 0.85), "{}", r.get());
}

/// The mid, not a side: the same quote read at its bid, its ask and its mid differs, and only the mid is used.
#[test]
fn the_rate_is_the_mid_of_the_quote_not_a_side() {
    let pair = conversion_pair("JPY", "USD").expect("USDJPY");
    let wide = tick("USDJPY", 150.0, 150.2);
    let r = pair.account_per_quote(&wide).expect("a rate").get();
    assert!(close(r, 1.0 / 150.1));
    assert!(r < 1.0 / 150.0 && r > 1.0 / 150.2, "strictly between the two sides");
}

// ---- every unusable tick is refused ---------------------------------------------------------------------

#[test]
fn a_tick_that_states_no_rate_is_refused_in_words() {
    let pair = conversion_pair("JPY", "USD").expect("USDJPY");
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
        match pair.account_per_quote(&tick("USDJPY", bid, ask)) {
            Err(RateError::Unavailable(text)) => {
                assert!(text.contains("states no usable rate"), "{bid} {ask}: {text}");
            }
            other => panic!("{bid} {ask}: {other:?}"),
        }
    }
    // the QuoteIsBase leg too (a zero mid is no rate, not zero dollars per pound)
    let gbp = conversion_pair("GBP", "USD").expect("GBPUSD");
    assert!(gbp.account_per_quote(&tick("GBPUSD", 0.0, 0.0)).is_err());
    // a tick of ANOTHER pair prices nothing here
    match pair.account_per_quote(&tick("EURUSD", 1.1, 1.1002)) {
        Err(RateError::Unavailable(text)) => assert!(text.contains("cannot price"), "{text}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_rate_cannot_be_built_from_a_non_rate() {
    for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
    }
    assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
}

// ---- the whole path, over a bus ------------------------------------------------------------------------

#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<String>>,
    closed: AtomicBool,
}

impl Wire {
    fn lines(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }
}

impl LinePort for Wire {
    fn send(&self, line: &str) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sent.lock().unwrap().push(line.to_string());
        true
    }
}

fn attached() -> (Arc<QuoteBus>, Arc<Wire>) {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    bus.attach(
        Arc::clone(&wire) as Arc<dyn LinePort>,
        &[FEATURE_TICKS.to_string(), FEATURE_TICK_ACKS.to_string()],
    );
    (bus, wire)
}

fn subscribe(symbol: &str) -> String {
    encode_line(&Command::Subscribe { symbols: vec![symbol.to_string()] })
}

/// Ask for a rate on a thread, deliver `bid`/`ask` for the conversion pair once its `subscribe` is on the wire.
fn rate_over_a_bus(
    quote: &'static str,
    account: &'static str,
    pair: &'static str,
    bid: f64,
    ask: f64,
    time: i64,
) -> Result<QuoteRate, RateError> {
    let (bus, wire) = attached();
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || quote_rate(&bus, quote, account, Duration::from_secs(10)))
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
    assert_eq!(got.quote_ccy, "JPY");
    assert!(close(got.account_per_quote.get(), 1.0 / 150.10), "{}", got.account_per_quote.get());
    assert_eq!(got.as_of_ms, NOW - 250, "the tick's own stamp, not the moment it was read");
    // 66.62, not 10,000
    let pnl = got.account_per_quote.to_account(10_000.0);
    assert_eq!((pnl * 100.0).round() / 100.0, 66.62, "{pnl}");

    let gbp = rate_over_a_bus("GBP", "USD", "GBPUSD", 1.3499, 1.3501, NOW).expect("a rate");
    assert!(close(gbp.account_per_quote.get(), 1.35));
    assert_eq!((gbp.quote_ccy.as_str(), gbp.as_of_ms), ("GBP", NOW));
}

/// No pair is a verdict on the INSTRUMENT and touches the bus not at all; no tick is a verdict on the MOMENT.
#[test]
fn no_pair_asks_the_sidecar_nothing_and_no_tick_is_unavailable_not_a_verdict() {
    let (bus, wire) = attached();
    assert!(matches!(
        quote_rate(&bus, "PLN", "JPY", Duration::from_millis(5)),
        Err(RateError::NoPair(_))
    ));
    assert!(matches!(
        quote_rate(&bus, "USD", "USD", Duration::from_millis(5)),
        Err(RateError::NoPair(_))
    ));
    assert!(wire.lines().is_empty(), "a pair that cannot exist is never subscribed");

    match quote_rate(&bus, "JPY", "USD", Duration::from_millis(40)) {
        Err(RateError::Unavailable(text)) => {
            assert!(text.contains("no tick of `USDJPY`"), "{text}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(wire.lines(), [subscribe("USDJPY")]);

    // no sidecar at all
    match quote_rate(&QuoteBus::new(), "JPY", "USD", Duration::from_millis(5)) {
        Err(RateError::Unavailable(text)) => assert!(text.contains("no JForex sidecar"), "{text}"),
        other => panic!("{other:?}"),
    }
}

/// A tick the price lane would not quote never becomes a rate: its own time is outside the bus's bound, so the
/// bus drops it and the wait ends in the same words as a quiet market.
#[test]
fn a_tick_outside_the_age_bound_is_never_a_rate() {
    let (bus, wire) = attached();
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || quote_rate(&bus, "JPY", "USD", Duration::from_millis(300)))
    };
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe("USDJPY")) {
        assert!(std::time::Instant::now() < until, "the subscribe never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.deliver("USDJPY", 150.09, 150.11, NOW - MAX_TICK_AGE_MS - 1, NOW);
    match asker.join().expect("no panic") {
        Err(RateError::Unavailable(text)) => {
            assert!(text.contains("no tick of `USDJPY`"), "{text}")
        }
        other => panic!("{other:?}"),
    }
}

/// **A tick the bus lets through but the engine would refuse as `quote-rate-future` says so, in words naming the
/// clock.** The bus accepts a stamp up to ten seconds ahead of this box's clock; the engine takes a rate stamped at
/// most five ahead of its own. A tick between the two used to reach the engine and come back as an opaque refusal.
#[test]
fn a_tick_stamped_further_ahead_than_the_engine_accepts_is_refused_naming_the_clock() {
    for (ahead_ms, accepted) in [(3_000, true), (7_000, false)] {
        let (bus, wire) = attached();
        let asker = {
            let bus = Arc::clone(&bus);
            std::thread::spawn(move || quote_rate(&bus, "JPY", "USD", Duration::from_secs(10)))
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
            (Err(RateError::Unavailable(text)), false) => {
                assert!(
                    text.contains("AHEAD of this box's clock")
                        && text.contains("skewed")
                        && text.contains("at most 5 s ahead")
                        && text.contains("USDJPY"),
                    "{text}"
                );
            }
            (other, _) => panic!("{ahead_ms} ms ahead: {other:?}"),
        }
    }
}
