//! [`quoted_facts`] and [`ledger_currency`] over a scripted sidecar (a `QuoteBus` with a recording command pipe;
//! the ticks are delivered by hand). DOCS-SHAPED: the sidecar's `ready` currency is `IAccount.getAccountCurrency()`
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/IAccount.html>) and a tick is `ITick`'s bid, ask
//! and time (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>); nothing here was captured
//! from a live login. The goldens are the decision's own, in the orientation the engine takes: account-currency
//! units per ONE unit of the quote currency (a yen is a fraction of a dollar, a pound more than one).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use vike_dukascopy::Command;
use vike_dukascopy::encode_line;
use vike_dukascopy::quotes::{FEATURE_TICK_ACKS, FEATURE_TICKS, LinePort};
use vike_gatewayhub_client::proto::facts_refusal_is_transient;
use vike_model::SymbolProperties;

use super::*;

const MOUNT: &str = "EURUSD";

fn now() -> i64 {
    vike_model::now_ms()
}

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

fn subscribe(symbol: &str) -> String {
    encode_line(&Command::Subscribe { symbols: vec![symbol.to_string()] })
}

/// A bus whose sidecar announced the price lane and stated `currency` (`None`: a jar from before the field).
fn bus(currency: Option<&str>) -> (Arc<QuoteBus>, Arc<Wire>) {
    let bus = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let id = bus.attach(
        Arc::clone(&wire) as Arc<dyn LinePort>,
        &[FEATURE_TICKS.to_string(), FEATURE_TICK_ACKS.to_string()],
    );
    bus.set_account_currency(id, currency);
    (bus, wire)
}

/// What the unconverting lane answers: the grid of `symbol`, or its refusal.
fn plain(symbol: &str) -> Result<WireSymbolFacts, String> {
    vike_dukascopy::any_symbol::instrument_facts(symbol, "USD")
        .map(|properties| WireSymbolFacts { properties, ..WireSymbolFacts::default() })
}

/// Ask for `symbol`'s facts on a thread, and once the `subscribe` of `pair` is on the wire deliver `bid`/`ask`
/// (the tick's own time `age_ms` ago).
fn facts_with_a_tick(
    bus: &Arc<QuoteBus>,
    wire: &Arc<Wire>,
    symbol: &'static str,
    pair: &'static str,
    (bid, ask): (f64, f64),
    age_ms: i64,
) -> Result<WireSymbolFacts, String> {
    let asker = {
        let bus = Arc::clone(bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), MOUNT, symbol, || plain(symbol))
        })
    };
    let until = Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe(pair)) {
        assert!(Instant::now() < until, "the subscribe of {pair} never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription(pair, true, "");
    let at = now();
    bus.deliver(pair, bid, ask, at - age_ms, at);
    asker.join().expect("no panic")
}

// ---- the ledger's currency ---------------------------------------------------------------------------

#[test]
fn the_ledger_currency_is_the_sidecars_and_only_when_it_is_the_mounted_pairs_money() {
    let (usd, _) = bus(Some("USD"));
    assert_eq!(ledger_currency(&usd, MOUNT).as_deref(), Ok("USD"));
    assert_eq!(ledger_currency(&usd, " eurusd ").as_deref(), Ok("USD"));

    // a jar from before the field, or none at all: unknown, never USD
    let (old, _) = bus(None);
    let why = ledger_currency(&old, MOUNT).expect_err("unstated");
    assert!(why.contains("states no currency for the account"), "{why}");
    let why = ledger_currency(&QuoteBus::new(), MOUNT).expect_err("no sidecar");
    assert!(why.contains("states no currency for the account"), "{why}");

    // a EUR account on a EURUSD mount: the mounted pair is booked at par in the wrong money
    let (eur, _) = bus(Some("EUR"));
    let why = ledger_currency(&eur, MOUNT).expect_err("a EUR account on a USD-quoted mount");
    assert!(why.contains("money is EUR") && why.contains("mounted on EURUSD"), "{why}");
    assert!(why.contains("books at par"), "{why}");
    // the same EUR account on a pair quoted in EUR would be fine
    assert_eq!(ledger_currency(&eur, "GBPEUR").as_deref(), Ok("EUR"));

    // the mount must be a pair
    for bad in ["", "EUR", "EUR/USD", "EURUSDX"] {
        let why = ledger_currency(&usd, bad).expect_err(bad);
        assert!(why.contains("is not a pair"), "{bad}: {why}");
    }
}

/// **The ledger's state is a function of what the sidecar states and the symbol the engine was STARTED on**, not of
/// the wiring table's `EURUSD`: a USD account mounted on `USDJPY` books its mounted pair at par in yen, so its
/// ledger is not the dollar the sidecar states.
#[test]
fn the_ledger_state_follows_the_start_symbol_and_what_the_sidecar_states() {
    let (usd, _) = bus(Some("USD"));
    assert_eq!(ledger_of(&usd, "EURUSD"), Ledger::Anchored("USD".into()));
    assert_eq!(ledger_of(&usd, "gbpusd"), Ledger::Anchored("USD".into()));
    assert_eq!(
        ledger_of(&usd, "USDJPY"),
        Ledger::Elsewhere {
            stated: "USD".into(),
            mounted: "USDJPY".into(),
            money: Some("JPY".into())
        }
    );
    assert_eq!(
        ledger_of(&usd, "EUR/USD"),
        Ledger::Elsewhere { stated: "USD".into(), mounted: "EUR/USD".into(), money: None }
    );
    let (eur, _) = bus(Some("EUR"));
    assert_eq!(ledger_of(&eur, "GBPEUR"), Ledger::Anchored("EUR".into()));
    assert_eq!(
        ledger_of(&eur, MOUNT),
        Ledger::Elsewhere {
            stated: "EUR".into(),
            mounted: MOUNT.into(),
            money: Some("USD".into())
        }
    );
    // nothing stated: an old jar is attached and silent, a sidecar that is starting is not attached
    let (old, _) = bus(None);
    assert_eq!(ledger_of(&old, MOUNT), Ledger::Unstated { attached: true });
    assert_eq!(ledger_of(&QuoteBus::new(), MOUNT), Ledger::Unstated { attached: false });
}

/// **A stated currency that is not the mount's money refuses EVERY symbol beyond the mounted one, in words naming
/// it** (a EUR account on the `EURUSD` mount: the mounted pair is booked in dollars as if they were euros, and a
/// USD pair admitted at par beside it would have been the same mistake; a USD account on a `USDJPY` mount: its
/// book is in yen, and a converted `EURJPY` would have been a second treatment of it). Nothing is converted, nothing
/// is priced, and the sentence is a verdict on the account, not a failure of the rate.
#[test]
fn a_stated_currency_that_is_not_the_mounts_money_refuses_every_extra_symbol_naming_it() {
    for (stated, mount, symbols) in [
        ("EUR", "EURUSD", ["GBPUSD", "USDJPY", "EURGBP", "AUDUSD"]),
        ("USD", "USDJPY", ["AUDUSD", "EURJPY", "EURGBP", "GBPUSD"]),
    ] {
        let (bus, wire) = bus(Some(stated));
        for symbol in symbols {
            let why = quoted_facts(&bus, Duration::from_millis(5), mount, symbol, || plain(symbol))
                .expect_err(symbol);
            assert!(
                why.contains(&format!("account's money is {stated}"))
                    && why.contains(&format!("mounted on {mount}"))
                    && why.contains(&format!("`{symbol}`"))
                    && why.contains("NOT admitted"),
                "{stated} on {mount}, {symbol}: {why}"
            );
            assert!(why.contains(&format!("pair quoted in {stated}")), "{why}");
            assert!(!why.contains("money is USD") || stated == "USD", "{why}");
            assert!(!facts_refusal_is_transient(&why), "a verdict on the account: {why}");
        }
        assert!(
            wire.lines().is_empty(),
            "nothing is priced for a refused account: {:?}",
            wire.lines()
        );
        // the instrument's own refusals still come first, in their own words
        for (symbol, words) in
            [("XAUUSD", "a metal's amount is in ounces"), ("GBPNOK", "not one of the")]
        {
            let why = quoted_facts(&bus, Duration::from_millis(5), mount, symbol, || plain(symbol))
                .expect_err(symbol);
            assert!(why.contains(words), "{symbol}: {why}");
        }
    }
    // the same sidecar on a mount quoted in its money is the converting lane (nothing refused)
    let (usd, wire) = bus(Some("USD"));
    assert!(
        quoted_facts(&usd, Duration::from_millis(5), "EURUSD", "GBPUSD", || plain("GBPUSD"))
            .is_ok()
    );
    assert!(wire.lines().is_empty());
}

// ---- the facts, orientation by value ----------------------------------------------------------------

/// USDJPY: the dollar is the base, the tick is yen per dollar, so the rate is its INVERSE.
#[test]
fn a_jpy_pair_on_a_usd_account_is_answered_with_a_fraction_of_a_dollar_per_yen() {
    let (bus, wire) = bus(Some("USD"));
    let got = facts_with_a_tick(&bus, &wire, "USDJPY", "USDJPY", (150.09, 150.11), 250)
        .expect("a converted answer");
    assert_eq!(got.quote_ccy.as_deref(), Some("JPY"));
    let rate = got.quote_rate.expect("a rate");
    assert!(rate < 0.01 && rate > 0.006, "{rate} is yen-per-dollar, the flipped rate");
    assert!((rate - 1.0 / 150.10).abs() < 1e-12, "the inverse of the MID: {rate}");
    // 100,000 units at 150.10 are 15,010,000 yen = 100,000 dollars; a 0.10 move is 66.62 dollars, not 10,000
    assert!((rate * 100_000.0 * 150.10 - 100_000.0).abs() < 1e-6);
    assert_eq!((rate * 100_000.0 * 0.10 * 100.0).round() / 100.0, 66.62);
    // the rate's moment is the TICK's own time, not the moment the lookup answered
    let stamp = got.rate_as_of_ms.expect("a moment");
    let age = now() - stamp;
    assert!((200..5_000).contains(&age), "the tick's own stamp: {age} ms old");
    // the grid is the SDK table's: USD/JPY has a three-decimal price grid and the sidecar's 1000-unit floor
    assert_eq!(got.properties.tick_size, 0.001);
    assert_eq!((got.properties.step_size, got.properties.min_qty), (1000.0, 1000.0));
    assert_eq!(got.leverage, None);
}

/// EURGBP: priced by GBPUSD, whose tick is dollars per pound, so the rate is the mid itself.
#[test]
fn a_gbp_pair_on_a_usd_account_is_answered_with_more_than_a_dollar_per_pound() {
    let (bus, wire) = bus(Some("USD"));
    let got = facts_with_a_tick(&bus, &wire, "EURGBP", "GBPUSD", (1.3499, 1.3501), 100)
        .expect("a converted answer");
    assert_eq!(got.quote_ccy.as_deref(), Some("GBP"));
    let rate = got.quote_rate.expect("a rate");
    assert!(rate > 1.0, "{rate} is pounds-per-dollar, the flipped rate");
    assert!((rate - 1.35).abs() < 1e-12, "{rate}");
    // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars
    assert!((rate * 85_000.0 - 114_750.0).abs() < 1e-6);
}

/// A pair quoted in the account's own money needs no rate and asks the sidecar nothing; it is the unconverting
/// lane's answer byte for byte.
#[test]
fn a_usd_quoted_pair_on_a_usd_account_carries_no_rate_and_subscribes_nothing() {
    let (bus, wire) = bus(Some("USD"));
    for symbol in ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD"] {
        let got = quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect("admitted");
        assert_eq!(got, plain(symbol).expect("the unconverting answer"), "{symbol}");
        assert_eq!((got.quote_ccy, got.quote_rate, got.rate_as_of_ms), (None, None, None));
    }
    assert!(wire.lines().is_empty(), "no conversion pair was asked for: {:?}", wire.lines());
}

// ---- no rate, no admission ---------------------------------------------------------------------------

/// A quiet market (or a feed not live yet) is a failure of the RATE, which the node does not remember as a
/// verdict on the symbol; the conversion pair stays subscribed for the next ask.
#[test]
fn no_tick_in_time_is_a_transient_refusal_and_never_a_rate() {
    let (bus, wire) = bus(Some("USD"));
    let why = quoted_facts(&bus, Duration::from_millis(60), MOUNT, "USDJPY", || plain("USDJPY"))
        .expect_err("no tick");
    assert!(why.contains("no rate for JPY in USD") && why.contains("no tick of `USDJPY`"), "{why}");
    assert!(facts_refusal_is_transient(&why), "{why}");
    assert_eq!(wire.lines(), [subscribe("USDJPY")]);
}

#[test]
fn a_stale_one_sided_or_crossed_tick_is_never_a_rate() {
    for (bid, ask, age_ms) in [
        (150.09, 150.11, 11_000),  // older than the lane's ten seconds
        (0.0, 150.11, 0),          // one-sided
        (150.2, 150.1, 0),         // crossed
        (150.09, 150.11, -11_000), // from the future
    ] {
        let (bus, wire) = bus(Some("USD"));
        let asker = {
            let bus = Arc::clone(&bus);
            std::thread::spawn(move || {
                quoted_facts(&bus, Duration::from_millis(400), MOUNT, "USDJPY", || plain("USDJPY"))
            })
        };
        let until = Instant::now() + Duration::from_secs(10);
        while wire.lines().is_empty() {
            assert!(Instant::now() < until, "the subscribe never went out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let at = now();
        bus.deliver("USDJPY", bid, ask, at - age_ms, at);
        let why = asker.join().expect("no panic").expect_err("not a quote");
        assert!(facts_refusal_is_transient(&why), "{bid} {ask} {age_ms}: {why}");
        assert!(why.contains("no tick of `USDJPY`"), "{bid} {ask} {age_ms}: {why}");
    }
}

/// No sidecar, an old jar and a declined subscription are each a failure of the rate in words.
#[test]
fn no_sidecar_an_old_jar_and_a_decline_are_refusals_in_words() {
    // a sidecar that is not attached (starting, restarting): the ledger is not known YET. A USD pair is admitted as
    // before; a foreign pair is "not now" (it must not be remembered as a verdict on the symbol), and a refusal of
    // the instrument itself (a metal, a name the table lacks) stays definitive
    let bus = QuoteBus::new();
    let jpy = quoted_facts(&bus, Duration::from_millis(5), MOUNT, "USDJPY", || plain("USDJPY"))
        .expect_err("no sidecar");
    assert!(jpy.contains("sidecar is not up") && jpy.contains("`USDJPY`"), "{jpy}");
    assert!(facts_refusal_is_transient(&jpy), "{jpy}");
    assert!(
        quoted_facts(&bus, Duration::from_millis(5), MOUNT, "GBPUSD", || plain("GBPUSD")).is_ok()
    );
    for symbol in ["XAUUSD", "GBPNOK", "AAPLUSUSD"] {
        assert_eq!(
            quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol)),
            plain(symbol),
            "{symbol}"
        );
    }
    // an old jar IS attached and states nothing: the unconverting lane's old fence, word for word, definitive
    let (old, _) = self::bus(None);
    let old_fence =
        quoted_facts(&old, Duration::from_millis(5), MOUNT, "USDJPY", || plain("USDJPY"))
            .expect_err("old jar");
    assert!(
        old_fence.contains("quoted in JPY") && old_fence.contains("nothing converts"),
        "{old_fence}"
    );
    assert!(!facts_refusal_is_transient(&old_fence), "{old_fence}");

    // the sidecar declines the conversion pair
    let (bus, wire) = self::bus(Some("USD"));
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), MOUNT, "USDJPY", || plain("USDJPY"))
        })
    };
    let until = Instant::now() + Duration::from_secs(10);
    while wire.lines().is_empty() {
        assert!(Instant::now() < until, "the subscribe never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription("USDJPY", false, "unknown instrument");
    let why = asker.join().expect("no panic").expect_err("declined");
    assert!(
        why.contains("declined to price `USDJPY`") && why.contains("unknown instrument"),
        "{why}"
    );
    assert!(facts_refusal_is_transient(&why), "{why}");
}

// ---- what is not converted -----------------------------------------------------------------------------

/// A jar that states no currency leaves the node exactly where it was: the unconverting answers, byte for byte.
#[test]
fn a_jar_without_the_currency_answers_as_the_unconverting_lane_does() {
    let (old, wire) = bus(None);
    for symbol in [
        "EURUSD",
        "USDJPY",
        "EURGBP",
        "XAUUSD",
        "BTCUSD",
        "AAPLUSUSD",
        "COPPER.CMD/USD",
        "GBPNOK",
        "",
    ] {
        let got = quoted_facts(&old, Duration::from_millis(5), MOUNT, symbol, || plain(symbol));
        assert_eq!(got, plain(symbol), "{symbol:?}");
    }
    assert!(wire.lines().is_empty(), "an unconverting answer prices nothing");
}

/// The metals, the coins and every CFD keep their own refusals for a converting node, in the same words.
#[test]
fn the_other_refusals_stand_for_a_node_that_converts() {
    let (bus, wire) = bus(Some("USD"));
    let ask = |symbol: &str| {
        quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect_err(symbol)
    };
    assert!(ask("XAUUSD").contains("a metal's amount is in ounces"));
    assert!(ask("XAGUSD").contains("a metal's amount is in ounces"));
    assert!(ask("BTCUSD").contains("is a coin typed FOREX in the SDK"));
    assert!(ask("AAPLUSUSD").contains("a CFD whose pip in the SDK is its blanket default"));
    assert!(ask("COPPER.CMD/USD").contains("COPPERCMDUSD"));
    assert!(ask("GBPNOK").contains("not one of the 180 instruments"));
    assert!(wire.lines().is_empty(), "nothing refused by name is priced: {:?}", wire.lines());
}

/// A USD-stating bus whose CONSUMER holds all eight of the sidecar's places (the data lane winning every place).
fn bus_with_every_place_taken() -> (Arc<QuoteBus>, vike_dukascopy::quotes::Listening) {
    let bus = QuoteBus::new();
    // a jar that announces ticks and no acks, so a want waits for no answer
    let id =
        bus.attach(Arc::new(Wire::default()) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    bus.set_account_currency(id, Some("USD"));
    let listening = bus.listen(Arc::new(|_: &vike_dukascopy::quotes::PriceTick| {}));
    for symbol in ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD", "USDCHF", "EURGBP", "EURJPY", "GBPJPY"] {
        listening.want(symbol).expect("a place");
    }
    (bus, listening)
}

/// **A conversion pair the data lane leaves no place for is said, once an hour per symbol** (a held foreign
/// position's rate would otherwise go stale after the engine's 120 s with no line saying why): the refusal is the
/// same "not now" as ever, and the one WARN names the symbol, the pair's currencies and what happens to a position
/// held in it. The re-ask of the next refresh is refused again and says nothing more.
#[test]
fn a_conversion_pair_with_no_place_is_said_once_an_hour_naming_the_symbol() {
    let (bus, _listening) = bus_with_every_place_taken();
    // AUDCAD is quoted in CAD: its rate needs USDCAD, a ninth instrument on a sidecar that prices eight
    let ask = || quoted_facts(&bus, Duration::from_millis(5), MOUNT, "AUDCAD", || plain("AUDCAD"));
    let (first, events) = vike_log::capture::captured(ask);
    let why = first.expect_err("no place for the conversion pair");
    assert!(facts_refusal_is_transient(&why), "still a 'not now', never a verdict: {why}");
    assert!(
        why.contains("no rate for CAD in USD") && why.contains("at most 8 instruments"),
        "{why}"
    );
    let warns: Vec<_> = events.iter().filter(|e| e.level == tracing::Level::WARN).collect();
    assert_eq!(warns.len(), 1, "one WARN: {events:?}");
    let said = &warns[0].message;
    for words in
        ["`AUDCAD`", "CAD in USD", "could not be subscribed", "120 s", "once an hour per symbol"]
    {
        assert!(said.contains(words), "{words}: {said}");
    }
    let (second, events) = vike_log::capture::captured(ask);
    assert!(second.is_err(), "refused again");
    assert!(events.iter().all(|e| e.level != tracing::Level::WARN), "said no more: {events:?}");
}

/// A refusal that is NOT "could not be subscribed" (the pair was subscribed and sent no tick) says nothing here: a
/// closed market is the weekend's ordinary state and a line per symbol per hour of it would only be noise.
#[test]
fn a_pair_that_was_subscribed_and_sent_no_tick_is_not_said() {
    let (bus, _wire) = bus(Some("USD"));
    let (answer, events) = vike_log::capture::captured(|| {
        quoted_facts(&bus, Duration::from_millis(30), MOUNT, "NZDCAD", || plain("NZDCAD"))
    });
    let why = answer.expect_err("no tick");
    assert!(why.contains("no tick of `USDCAD`"), "{why}");
    assert!(events.iter().all(|e| e.level != tracing::Level::WARN), "{events:?}");
}

#[test]
fn the_not_subscribed_line_is_due_once_per_hour_per_symbol() {
    assert_eq!(NOT_SUBSCRIBED_SAID_EVERY, Duration::from_secs(3600));
    let said = SaidAt::default();
    let (t0, hour) = (Instant::now(), NOT_SUBSCRIBED_SAID_EVERY);
    assert!(said.due("AUDCAD", t0, hour));
    assert!(!said.due("AUDCAD", t0 + Duration::from_secs(5), hour));
    assert!(!said.due("AUDCAD", t0 + hour - Duration::from_secs(1), hour));
    assert!(
        said.due("NZDCAD", t0 + Duration::from_secs(5), hour),
        "another symbol has its own hour"
    );
    assert!(said.due("AUDCAD", t0 + hour, hour), "an hour later it is said again");
}

/// **Which instruments a converting node can open on a USD ledger, by the table itself**: every currency pair of
/// the SDK table (the coins and the metals aside, which keep their own refusals) is answered with a quote
/// currency, and every one quoted in another money than the dollar has a single conversion pair against the
/// dollar in the same table (so no admission waits on a cross). A row added to the table that breaks this is
/// a reviewed edit, not a silent refusal at run time.
#[test]
fn every_currency_pair_of_the_table_is_priceable_against_the_dollar() {
    let mut opened = 0;
    for row in vike_dukascopy::jforex_grid::jforex_instruments() {
        let Ok((_, quote)) = vike_dukascopy::any_symbol::instrument_facts_quoted(&row.symbol)
        else {
            continue;
        };
        opened += 1;
        if quote != "USD" {
            // (USDJPY's own tick is the JPY conversion: the pair prices itself, no second subscription)
            vike_dukascopy::home_conversion::conversion_pair(&quote, "USD")
                .unwrap_or_else(|e| panic!("{}: {e}", row.symbol));
        }
    }
    assert_eq!(opened, 70, "the 72 FOREX rows less the two coins");
}

#[test]
fn the_properties_a_converted_answer_carries_are_the_unconverting_lanes_grid() {
    let (bus, wire) = bus(Some("USD"));
    let got = facts_with_a_tick(&bus, &wire, "USDCHF", "USDCHF", (0.8799, 0.8801), 0)
        .expect("a converted answer");
    let SymbolProperties { tick_size, step_size, min_qty, .. } = got.properties;
    assert_eq!((tick_size, step_size, min_qty), (0.00001, 1000.0, 1000.0));
    assert_eq!(got.quote_ccy.as_deref(), Some("CHF"));
    assert!((got.quote_rate.unwrap() - 1.0 / 0.88).abs() < 1e-12);
}
