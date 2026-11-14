//! [`super::quoted_facts`] and [`currency_read`] over a scripted sidecar (a `QuoteBus` with a recording command pipe;
//! the ticks are delivered by hand). DOCS-SHAPED: the sidecar's `ready` currency is `IAccount.getAccountCurrency()`
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/IAccount.html>) and a tick is `ITick`'s bid, ask
//! and time (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/ITick.html>); nothing here was captured
//! from a live login. The goldens are the decision's own, in the orientation the engine takes: account-currency
//! units per ONE unit of the quote currency (a yen is a fraction of a dollar, a pound more than one).

use std::sync::Arc;
use std::time::Instant;

use crate::feed::{FEATURE_TICK_ACKS, FEATURE_TICKS, LinePort};
use crate::test_support::{Wire, attached, listed};
use crate::{SidecarCommand, encode_line};
use vike_model::SymbolProperties;

use super::*;

const MOUNT: &str = "EURUSD";

/// The test's answer: the fields a converted answer fills, named as the gatewayhub's wire answer names them.
#[derive(Debug, Clone, PartialEq, Default)]
struct Answer {
    properties: SymbolProperties,
    quote_currency: Option<String>,
    quote_rate: Option<f64>,
    rate_as_of_ms: Option<i64>,
    account_currency: Option<String>,
}

/// The word a test reads a "not now" by (the gatewayhub words it with the wire's own marker).
const NOT_NOW: &str = "<not now>";

/// [`super::quoted_facts`] into the test's [`Answer`], its refusal worded as the gatewayhub words it with [`NOT_NOW`] for
/// the wire's marker.
fn quoted_facts(
    bus: &QuoteBus,
    wait: Duration,
    mount_symbol: &str,
    symbol: &str,
    plain: impl FnOnce() -> Result<Answer, String>,
) -> Result<Answer, String> {
    super::quoted_facts(bus, wait, mount_symbol, symbol, plain, |properties, rate, ledger| Answer {
        properties,
        quote_currency: rate.as_ref().map(|r| r.quote_currency.clone()),
        quote_rate: rate.as_ref().map(|r| r.account_per_quote.get()),
        rate_as_of_ms: rate.map(|r| r.as_of_ms),
        account_currency: Some(ledger),
    })
    .map_err(|e| {
        if e.is_transient() { format!("dukascopy: {NOT_NOW}: {e}") } else { e.to_string() }
    })
}

/// Whether the node would ask again after `why` (the gatewayhub's marker, [`NOT_NOW`] here).
fn properties_refusal_is_transient(why: &str) -> bool {
    why.contains(NOT_NOW)
}

fn now() -> i64 {
    vike_model::now_ms()
}

fn subscribe(symbol: &str) -> String {
    encode_line(&SidecarCommand::Subscribe { symbols: vec![symbol.to_string()] })
}

/// A bus whose sidecar announced the price lane and stated `currency` (`None`: a jar from before the field).
fn bus(currency: Option<&str>) -> (Arc<QuoteBus>, Arc<Wire>) {
    attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS], currency)
}

/// What the unconverting lane answers: the grid of `symbol` on the account's list, or its refusal.
fn plain(symbol: &str) -> Result<Answer, String> {
    crate::instrument::any_symbol::instrument_facts(&listed(), symbol, "USD")
        .map(|properties| Answer { properties, ..Answer::default() })
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
) -> Result<Answer, String> {
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

/// **A mounted symbol that is no pair keeps the ledger's money unknown**: a sidecar that states USD on a mount that
/// is not a six-letter pair is `Elsewhere` with no money (the mounted symbol's money is not known), and the lane
/// still tells the node the currency the sidecar STATED (the node's mount fence declines the anchor).
#[test]
fn a_mount_that_is_no_pair_has_no_known_money_and_the_stated_currency_still_travels() {
    let (usd, _) = bus(Some("USD"));
    assert_eq!(ledger_of(&usd, " eurusd "), Ledger::Anchored("USD".into()));
    for bad in ["", "EUR", "EUR/USD", "EURUSDX"] {
        assert!(
            matches!(ledger_of(&usd, bad), Ledger::Elsewhere { money: None, .. }),
            "{bad:?}: the money it is booked in is not known"
        );
        assert_eq!(currency_read(&usd, bad), CcyRead::Stated("USD".into()), "{bad:?}");
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

/// Ask for `symbol`'s facts as an account mounted on `mount`, delivering one tick of `pair` (as
/// [`facts_with_a_tick`], whose mount is the wiring table's).
fn facts_on_mount_with_a_tick(
    bus: &Arc<QuoteBus>,
    wire: &Arc<Wire>,
    mount: &'static str,
    symbol: &'static str,
    pair: &'static str,
    (bid, ask): (f64, f64),
) -> Result<Answer, String> {
    let asker = {
        let bus = Arc::clone(bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), mount, symbol, || plain(symbol))
        })
    };
    let until = Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe(pair)) {
        assert!(Instant::now() < until, "the subscribe of {pair} never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription(pair, true, "");
    let at = now();
    bus.deliver(pair, bid, ask, at - 100, at);
    asker.join().expect("no panic")
}

/// **A stated currency that is not the mount's money is still the ledger, and the MOUNT is rated like any other
/// symbol** (decision 0183's mounted-symbol amendment, dukascopy joining FXCM's mechanism): a EUR account on the
/// `EURUSD` mount is answered the mounted pair itself with its quote currency (USD) and the rate of euros per
/// dollar, which is the INVERSE of the pair's mid (the pair is EURUSD: dollars per euro), and every other pair
/// converts into euros, in the orientation the engine takes (euros per ONE unit of the quote currency).
#[test]
fn a_eur_account_on_the_eurusd_mount_rates_the_mount_and_converts_every_pair_into_euros() {
    // the mount itself: USD-quoted, priced by its own tick, 1 / 1.18 euros per dollar
    let (eur, wire) = bus(Some("EUR"));
    let mount =
        facts_on_mount_with_a_tick(&eur, &wire, MOUNT, "EURUSD", "EURUSD", (1.1799, 1.1801))
            .expect("the mount is rated");
    assert_eq!(mount.quote_currency.as_deref(), Some("USD"));
    let rate = mount.quote_rate.expect("a rate");
    assert!(rate < 1.0, "{rate} is dollars-per-euro, the flipped rate");
    assert!((rate - 1.0 / 1.18).abs() < 1e-12, "the inverse of the MID: {rate}");
    assert!(mount.rate_as_of_ms.is_some());
    assert_eq!(mount.account_currency.as_deref(), Some("EUR"), "the answer states its ledger");

    // a USD-quoted pair beside it: the same conversion pair, the same rate
    let (eur, wire) = bus(Some("EUR"));
    let gbpusd =
        facts_on_mount_with_a_tick(&eur, &wire, MOUNT, "GBPUSD", "EURUSD", (1.1799, 1.1801))
            .expect("a dollar pair converts into euros");
    assert_eq!(
        (gbpusd.quote_currency.as_deref(), gbpusd.quote_rate),
        (Some("USD"), mount.quote_rate)
    );

    // a yen pair: EURJPY's tick is yen per euro, so euros per yen is its inverse (about 1/160)
    let (eur, wire) = bus(Some("EUR"));
    let usdjpy = facts_on_mount_with_a_tick(&eur, &wire, MOUNT, "USDJPY", "EURJPY", (159.9, 160.1))
        .expect("a yen pair converts into euros");
    assert_eq!(usdjpy.quote_currency.as_deref(), Some("JPY"));
    assert!((usdjpy.quote_rate.expect("a rate") - 1.0 / 160.0).abs() < 1e-12);
    assert_eq!(usdjpy.account_currency.as_deref(), Some("EUR"));

    // a pound pair: EURGBP's tick is pounds per euro, so euros per pound is its inverse (about 1.17)
    let (eur, wire) = bus(Some("EUR"));
    let eurgbp =
        facts_on_mount_with_a_tick(&eur, &wire, MOUNT, "EURGBP", "EURGBP", (0.8499, 0.8501))
            .expect("a pound pair converts into euros");
    assert_eq!(eurgbp.quote_currency.as_deref(), Some("GBP"));
    assert!((eurgbp.quote_rate.expect("a rate") - 1.0 / 0.85).abs() < 1e-12);
}

/// The same holds for a USD account mounted on a yen-quoted pair (`USDJPY`): the ledger is the dollar the sidecar
/// states, the mount is quoted in yen and rated by its own tick (dollars per yen, `1/150`), and a dollar-quoted pair
/// beside it needs no rate.
#[test]
fn a_usd_account_on_a_yen_mount_rates_the_mount_in_dollars() {
    let (usd, wire) = bus(Some("USD"));
    let mount =
        facts_on_mount_with_a_tick(&usd, &wire, "USDJPY", "USDJPY", "USDJPY", (149.9, 150.1))
            .expect("the yen-quoted mount is rated");
    assert_eq!(mount.quote_currency.as_deref(), Some("JPY"));
    assert!((mount.quote_rate.expect("a rate") - 1.0 / 150.0).abs() < 1e-12);
    assert_eq!(mount.account_currency.as_deref(), Some("USD"));
    let own = quoted_facts(&usd, Duration::from_millis(5), "USDJPY", "AUDUSD", || plain("AUDUSD"))
        .expect("quoted in the ledger's money");
    assert_eq!((own.quote_currency, own.quote_rate), (None, None));
    assert_eq!(own.account_currency.as_deref(), Some("USD"));
}

/// **A mount that is no pair refuses EVERY symbol beyond the mounted one, in words naming the stated currency**: the
/// node never declares a mount whose money it cannot read off the name, so the ledger would count the mount at par
/// in a money nobody knows. Nothing is converted, nothing is priced, and the sentence is a verdict on the account,
/// not a failure of the rate.
#[test]
fn a_mount_that_is_no_pair_refuses_every_extra_symbol_naming_the_stated_currency() {
    for (stated, mount, symbols) in [
        ("EUR", "EUR/USD", ["GBPUSD", "USDJPY", "EURGBP", "AUDUSD"]),
        ("USD", "", ["AUDUSD", "EURJPY", "EURGBP", "GBPUSD"]),
    ] {
        let (bus, wire) = bus(Some(stated));
        for symbol in symbols {
            let why = quoted_facts(&bus, Duration::from_millis(5), mount, symbol, || plain(symbol))
                .expect_err(symbol);
            assert!(
                why.contains(&format!("account's money is {stated}"))
                    && why.contains(&format!("mounted on {mount}"))
                    && why.contains("not a pair")
                    && why.contains(&format!("`{symbol}`"))
                    && why.contains("NOT admitted"),
                "{stated} on {mount}, {symbol}: {why}"
            );
            assert!(why.contains(&format!("pair quoted in {stated}")), "{why}");
            assert!(!properties_refusal_is_transient(&why), "a verdict on the account: {why}");
        }
        assert!(
            wire.lines().is_empty(),
            "nothing is priced for a refused account: {:?}",
            wire.lines()
        );
        // the instrument's own refusals still come first, in their own words
        for (symbol, words) in [
            ("BTCUSD", "a coin typed FOREX"),
            ("GBPNOK", "not on the account's instrument list"),
            ("AAPLUSUSD", "enum-style name"),
        ] {
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
    assert_eq!(got.quote_currency.as_deref(), Some("JPY"));
    assert_eq!(
        got.account_currency.as_deref(),
        Some("USD"),
        "the answer states the ledger it is in"
    );
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
    // the grid is the list row's: USD/JPY has a three-decimal price grid and the sidecar's 1000-unit floor
    assert_eq!(got.properties.tick_size, 0.001);
    assert_eq!((got.properties.step_size, got.properties.min_qty), (1000.0, 1000.0));
}

/// EURGBP: priced by GBPUSD, whose tick is dollars per pound, so the rate is the mid itself.
#[test]
fn a_gbp_pair_on_a_usd_account_is_answered_with_more_than_a_dollar_per_pound() {
    let (bus, wire) = bus(Some("USD"));
    let got = facts_with_a_tick(&bus, &wire, "EURGBP", "GBPUSD", (1.3499, 1.3501), 100)
        .expect("a converted answer");
    assert_eq!(got.quote_currency.as_deref(), Some("GBP"));
    let rate = got.quote_rate.expect("a rate");
    assert!(rate > 1.0, "{rate} is pounds-per-dollar, the flipped rate");
    assert!((rate - 1.35).abs() < 1e-12, "{rate}");
    // EURGBP at 0.85, 100,000 units: 85,000 pounds = 114,750 dollars
    assert!((rate * 85_000.0 - 114_750.0).abs() < 1e-6);
}

/// A pair quoted in the account's own money needs no rate and asks the sidecar nothing; it is the unconverting
/// lane's answer byte for byte, plus the ledger it is expressed in (`account_currency`, which the node checks against the
/// currency its engine was anchored to).
#[test]
fn a_usd_quoted_pair_on_a_usd_account_carries_no_rate_and_subscribes_nothing() {
    let (bus, wire) = bus(Some("USD"));
    for symbol in ["EURUSD", "GBPUSD", "AUDUSD", "NZDUSD"] {
        let got = quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect("admitted");
        let unconverting = plain(symbol).expect("the unconverting answer");
        assert_eq!(
            got,
            Answer { account_currency: Some("USD".into()), ..unconverting },
            "{symbol}: the unconverting answer, stamped with its ledger"
        );
        assert_eq!((got.quote_currency, got.quote_rate, got.rate_as_of_ms), (None, None, None));
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
    assert!(properties_refusal_is_transient(&why), "{why}");
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
        assert!(properties_refusal_is_transient(&why), "{bid} {ask} {age_ms}: {why}");
        assert!(why.contains("no tick of `USDJPY`"), "{bid} {ask} {age_ms}: {why}");
    }
}

/// No sidecar, an old jar and a declined subscription are each a failure of the rate in words.
#[test]
fn no_sidecar_an_old_jar_and_a_decline_are_refusals_in_words() {
    // a sidecar that is not attached (restarting: the list its predecessor stated is held): the ledger is not known
    // YET. A USD pair is admitted as before; a foreign pair is "not now" (it must not be remembered as a verdict on the
    // symbol), and a refusal of the instrument itself (a metal, a name the list lacks) stays definitive
    let bus = QuoteBus::new();
    bus.set_instrument_list(listed());
    let jpy = quoted_facts(&bus, Duration::from_millis(5), MOUNT, "USDJPY", || plain("USDJPY"))
        .expect_err("no sidecar");
    assert!(jpy.contains("sidecar is not up") && jpy.contains("`USDJPY`"), "{jpy}");
    assert!(properties_refusal_is_transient(&jpy), "{jpy}");
    assert!(
        quoted_facts(&bus, Duration::from_millis(5), MOUNT, "GBPUSD", || plain("GBPUSD")).is_ok()
    );
    for symbol in ["GBPNOK", "AAPLUSUSD"] {
        assert_eq!(
            quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol)),
            plain(symbol),
            "{symbol}"
        );
    }
    // a metal or a CFD is the venue's terms' to answer, which need the sidecar: with none attached it is "not now"
    for symbol in ["XAUUSD", "COPPER.CMD/USD"] {
        let why = quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect_err(symbol);
        assert!(why.contains("sidecar is not up") && why.contains(symbol), "{symbol}: {why}");
        assert!(properties_refusal_is_transient(&why), "{symbol}: {why}");
    }
    // an old jar IS attached and states nothing: on a dollar-quoted mount that is the owner's assumed USD, so a yen
    // pair is priced (here no tick arrives in 5 ms: "not now", never remembered as a verdict on the symbol); only a
    // mount the assumed dollar cannot anchor (a yen-quoted one) keeps the unconverting lane's old fence, word for
    // word, definitive
    let (old, _) = self::bus(None);
    let priced = quoted_facts(&old, Duration::from_millis(5), MOUNT, "USDJPY", || plain("USDJPY"))
        .expect_err("no tick");
    assert!(priced.contains("no rate for JPY in USD"), "{priced}");
    assert!(properties_refusal_is_transient(&priced), "{priced}");
    let old_fence =
        quoted_facts(&old, Duration::from_millis(5), "USDJPY", "EURGBP", || plain("EURGBP"))
            .expect_err("old jar, yen mount");
    assert!(
        old_fence.contains("quoted in GBP") && old_fence.contains("nothing converts"),
        "{old_fence}"
    );
    assert!(!properties_refusal_is_transient(&old_fence), "{old_fence}");

    // a sidecar whose jar states no list (it predates the line): no grid is known, so every symbol is "not now", in
    // words, and none is remembered as a verdict
    let unlisted = QuoteBus::new();
    let wire = Arc::new(Wire::default());
    let id = unlisted.attach(Arc::clone(&wire) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    unlisted.set_account_currency(id, Some("USD"));
    for symbol in ["EURUSD", "USDJPY", "XAUUSD"] {
        let why =
            quoted_facts(&unlisted, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
                .expect_err(symbol);
        assert!(why.contains("no Dukascopy instrument list yet"), "{symbol}: {why}");
        assert!(properties_refusal_is_transient(&why), "{symbol}: {why}");
    }

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
    assert!(properties_refusal_is_transient(&why), "{why}");
}

// ---- what is not converted -----------------------------------------------------------------------------

/// A jar that states no currency answers everything that needs no conversion exactly as it did: the unconverting
/// answers, byte for byte (a pair quoted in the assumed USD, every refusal by name), except that an answer of the
/// assumed USD ledger now says so (`account_currency`), which the node checks against the currency it anchored the
/// engine to. What it does with a pair quoted in another currency is the owner's default, the next tests'.
#[test]
fn a_jar_without_the_currency_answers_as_the_unconverting_lane_does() {
    let (old, wire) = bus(None);
    for symbol in ["EURUSD", "BTCUSD", "AAPLUSUSD", "GBPNOK", ""] {
        let got = quoted_facts(&old, Duration::from_millis(5), MOUNT, symbol, || plain(symbol));
        let unstamped = got.clone().map(|facts| Answer { account_currency: None, ..facts });
        assert_eq!(unstamped, plain(symbol), "{symbol:?}");
        if let Ok(facts) = got {
            assert_eq!(
                facts.account_currency.as_deref(),
                Some("USD"),
                "{symbol:?}: the assumed ledger"
            );
        }
    }
    // a metal or a CFD needs the venue's terms, which a jar from before the field cannot state: "not now", in words
    for symbol in ["XAUUSD", "COPPER.CMD/USD"] {
        let why = quoted_facts(&old, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect_err(symbol);
        assert!(
            why.contains("predates the price lane") || why.contains("predates the venue's terms"),
            "{why}"
        );
        assert!(properties_refusal_is_transient(&why), "{symbol}: {why}");
    }
    assert!(wire.lines().is_empty(), "an unconverting answer prices nothing");
}

/// **The owner's USD default** (2026-10-19: "the account currency is what the venue API states; if the API does not
/// state it, or the read fails: USD"): a jar from before the field that is ATTACHED and states no currency is read as
/// an ASSUMED USD ledger when the mounted pair is quoted in it; where the mount is quoted in another money the
/// assumption cannot anchor it, and a sidecar that is not attached is "not now", never assumed over. A currency the
/// sidecar STATES is passed through untouched and never flagged (the golden).
#[test]
fn an_attached_jar_that_states_no_currency_is_read_as_an_assumed_usd_ledger() {
    let (old, _) = bus(None);
    assert_eq!(ledger_assuming(&old, MOUNT), (Ledger::Anchored("USD".into()), true));
    assert_eq!(ledger_assuming(&old, "gbpusd"), (Ledger::Anchored("USD".into()), true));
    assert_eq!(
        ledger_assuming(&old, "USDJPY"),
        (Ledger::Unstated { attached: true }, false),
        "a yen-quoted mount cannot be anchored to an assumed dollar"
    );
    assert_eq!(
        ledger_assuming(&QuoteBus::new(), MOUNT),
        (Ledger::Unstated { attached: false }, false),
        "no sidecar attached: not now"
    );
    let (usd, _) = bus(Some("USD"));
    assert_eq!(ledger_assuming(&usd, MOUNT), (Ledger::Anchored("USD".into()), false));
    let (eur, _) = bus(Some("EUR"));
    assert_eq!(ledger_assuming(&eur, "GBPEUR"), (Ledger::Anchored("EUR".into()), false));
    assert_eq!(ledger_assuming(&eur, MOUNT), (ledger_of(&eur, MOUNT), false), "stated: untouched");
}

/// **The start reads the sidecar's word, else the assumed USD, and declines the rest**: what a start tells the node
/// (`assumed_currency::start_currency`) is the sidecar's stated currency when the mount is quoted in it, the ruling's USD
/// (silence, to be flagged) when an attached jar states none, and nothing at all (declined: no assumption over a
/// stated money the mount does not count in, a yen-quoted mount, or a sidecar that is not up).
#[test]
fn the_start_reads_the_sidecars_currency_then_assumed_usd_and_declines_the_rest() {
    let (usd, _) = bus(Some("USD"));
    assert_eq!(currency_read(&usd, MOUNT), CcyRead::Stated("USD".into()));
    let (old, _) = bus(None);
    assert!(
        matches!(currency_read(&old, MOUNT), CcyRead::Silent(why) if why.contains("states no currency"))
    );
    assert_eq!(currency_read(&old, "USDJPY"), CcyRead::Declined);
    assert_eq!(currency_read(&QuoteBus::new(), MOUNT), CcyRead::Declined);
    // HIGH-1 of the review: a currency the sidecar STATES is never erased into "nothing" (and so into an assumed
    // USD) because the mounted pair is quoted in another money: EUR/GBP/CHF/JPY travel as stated, and the node's
    // mount fence declines the anchor in its own words, as it does for every other venue
    for stated in ["EUR", "GBP", "CHF", "JPY"] {
        let (stating, _) = bus(Some(stated));
        assert_eq!(
            currency_read(&stating, MOUNT),
            CcyRead::Stated(stated.into()),
            "{stated} on {MOUNT}"
        );
    }
    let (eur, _) = bus(Some("EUR"));
    assert_eq!(currency_read(&eur, "GBPEUR"), CcyRead::Stated("EUR".into()));
}

/// **An attached jar that states no currency PRICES a cross on the assumed USD ledger**, as a jar that states USD
/// does (EURGBP: priced by GBPUSD, whose mid is dollars per pound, a rate above one), and a pair quoted in the
/// assumed dollar needs no rate; where the assumption cannot anchor the mount (a yen-quoted one) the old fence's
/// words stand.
#[test]
fn a_jar_that_states_no_currency_prices_a_cross_on_the_assumed_usd_ledger() {
    let (old, wire) = bus(None);
    let got = facts_with_a_tick(&old, &wire, "EURGBP", "GBPUSD", (1.3499, 1.3501), 100)
        .expect("priced on the assumed USD ledger");
    assert_eq!(got.quote_currency.as_deref(), Some("GBP"));
    let rate = got.quote_rate.expect("a rate");
    assert!((rate - 1.35).abs() < 1e-12, "dollars per pound, not the flipped rate: {rate}");

    let own = quoted_facts(&old, Duration::from_millis(5), MOUNT, "AUDUSD", || plain("AUDUSD"))
        .expect("quoted in the assumed dollar");
    assert_eq!((own.quote_currency, own.quote_rate), (None, None));

    let (yen_mounted, _) = bus(None);
    let why = quoted_facts(&yen_mounted, Duration::from_millis(5), "USDJPY", "EURGBP", || {
        plain("EURGBP")
    })
    .expect_err("the old fence stands");
    assert!(why.contains("quoted in"), "{why}");
}

/// The coins, the CFDs in pence, the enum-style CFD names and the names the list lacks keep their refusals for a
/// converting node, by name and before anything is subscribed.
#[test]
fn the_other_refusals_stand_for_a_node_that_converts() {
    let (bus, wire) = bus(Some("USD"));
    let ask = |symbol: &str| {
        quoted_facts(&bus, Duration::from_millis(5), MOUNT, symbol, || plain(symbol))
            .expect_err(symbol)
    };
    assert!(ask("BTCUSD").contains("is a coin typed FOREX in the SDK"));
    assert!(ask("LSE.GB/GBX").contains("quoted in pence"));
    assert!(ask("AAPLUSUSD").contains("write the instrument `AAPL.US/USD`"));
    assert!(ask("COPPERCMDUSD").contains("write the instrument `COPPER.CMD/USD`"));
    assert!(ask("GBPNOK").contains("not on the account's instrument list (180 instruments"));
    for symbol in ["BTCUSD", "LSE.GB/GBX", "AAPLUSUSD", "GBPNOK"] {
        assert!(
            !properties_refusal_is_transient(&ask(symbol)),
            "a verdict on the symbol: {symbol}"
        );
    }
    assert!(wire.lines().is_empty(), "nothing refused by name is priced: {:?}", wire.lines());
}

// ---- a metal or a CFD, on the venue's own terms --------------------------------------------------------------

/// A bus whose sidecar announced the price lane, its answers AND the venue's terms, and stated `currency`.
fn bus_with_terms(currency: &str) -> (Arc<QuoteBus>, Arc<Wire>) {
    attached(&[FEATURE_TICKS, FEATURE_TICK_ACKS, crate::feed::FEATURE_TERMS], Some(currency))
}

/// The terms line the sidecar writes for one instrument: SDK pip scales, the given amounts, quoted in `quote`.
fn terms_line(
    symbol: &str,
    class: &str,
    (min, step): (f64, f64),
    (pip, pip_scale): (f64, u32),
    quote: &str,
) -> crate::VenueTerms {
    crate::VenueTerms {
        symbol: symbol.into(),
        class: class.into(),
        min: Some(min),
        step: Some(step),
        max: Some(5.0e8),
        per_contract: Some(1.0),
        pip: Some(pip),
        pip_scale: Some(pip_scale),
        tick_scale: Some(pip_scale + 1),
        quote: Some(quote.into()),
    }
}

/// Ask for `symbol`'s facts on a thread; once the `subscribe` of `symbol` is on the wire, answer it and state its terms
/// (the sidecar writes the line at the first tick, before the tick).
fn facts_with_terms(
    bus: &Arc<QuoteBus>,
    wire: &Arc<Wire>,
    symbol: &'static str,
    terms: crate::VenueTerms,
) -> Result<Answer, String> {
    let asker = {
        let bus = Arc::clone(bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), MOUNT, symbol, || plain(symbol))
        })
    };
    let until = Instant::now() + Duration::from_secs(10);
    while !wire.lines().contains(&subscribe(symbol)) {
        assert!(Instant::now() < until, "the subscribe of {symbol} never went out");
        std::thread::sleep(Duration::from_millis(5));
    }
    bus.subscription(symbol, true, "");
    bus.terms(terms);
    asker.join().expect("no panic")
}

/// **A metal is admitted on the grid the venue stated**, in the account's own money with no rate (XAU/USD on a USD
/// ledger), and a CFD likewise: the symbol the bus subscribes is the wire spelling, the answer carries the ledger it is
/// expressed in, and the grid is the terms' (ounces for the metal, contracts for the CFD).
#[test]
fn a_metal_and_a_usd_cfd_are_admitted_on_the_terms_the_venue_stated() {
    let (bus, wire) = bus_with_terms("USD");
    let gold = facts_with_terms(
        &bus,
        &wire,
        "XAUUSD",
        terms_line("XAUUSD", "METAL", (1.0, 1.0), (0.01, 2), "USD"),
    )
    .expect("gold");
    let SymbolProperties { tick_size, step_size, min_qty, .. } = gold.properties;
    assert_eq!((tick_size, step_size, min_qty), (0.001, 1.0, 1.0));
    assert_eq!(
        (gold.quote_currency, gold.quote_rate),
        (None, None),
        "quoted in the ledger's money: no rate"
    );
    assert_eq!(gold.account_currency.as_deref(), Some("USD"));

    let copper = facts_with_terms(
        &bus,
        &wire,
        "COPPER.CMD/USD",
        terms_line("COPPER.CMD/USD", "CFD", (20.0, 1.0), (0.001, 3), "USD"),
    )
    .expect("copper");
    assert_eq!(
        (copper.properties.tick_size, copper.properties.min_qty, copper.properties.step_size),
        (0.0001, 20.0, 1.0)
    );
    assert_eq!(copper.account_currency.as_deref(), Some("USD"));
    // the CFD was subscribed under its slashed SDK name, the one spelling the sidecar resolves
    assert!(wire.lines().contains(&subscribe("COPPER.CMD/USD")), "{:?}", wire.lines());
}

/// **A CFD quoted in euros on a dollar ledger is converted like a euro pair**: the terms give its grid, the rate of
/// EUR in USD is the mid of EURUSD (above one), and the answer carries the quote currency, the rate and the ledger.
#[test]
fn a_euro_cfd_on_a_dollar_ledger_is_converted_like_a_euro_pair() {
    let (bus, wire) = bus_with_terms("USD");
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), MOUNT, "DEU.IDX/EUR", || {
                plain("DEU.IDX/EUR")
            })
        })
    };
    let wait_for = |line: String| {
        let until = Instant::now() + Duration::from_secs(10);
        while !wire.lines().contains(&line) {
            assert!(Instant::now() < until, "{line} never went out");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait_for(subscribe("DEU.IDX/EUR"));
    bus.subscription("DEU.IDX/EUR", true, "");
    bus.terms(terms_line("DEU.IDX/EUR", "CFD", (0.1, 0.1), (0.01, 2), "EUR"));
    wait_for(subscribe("EURUSD"));
    bus.subscription("EURUSD", true, "");
    let at = now();
    bus.deliver("EURUSD", 1.0799, 1.0801, at, at);
    let got = asker.join().expect("no panic").expect("a converted CFD");
    assert_eq!((got.properties.min_qty, got.properties.step_size), (0.1, 0.1));
    assert_eq!(got.quote_currency.as_deref(), Some("EUR"));
    assert!(
        (got.quote_rate.unwrap() - 1.08).abs() < 1e-12,
        "dollars per euro: {:?}",
        got.quote_rate
    );
    assert_eq!(got.account_currency.as_deref(), Some("USD"));
}

/// **Gold on a euro account is converted through the euro pair** (decision 0213's money model, unchanged): a EUR ledger on
/// the default `EURUSD` mount rates XAU/USD's dollar quote by the INVERSE of the EURUSD mid (euros per dollar, below one),
/// and the answer states the euro ledger it is expressed in.
#[test]
fn gold_on_a_euro_account_is_converted_by_the_inverse_of_the_euro_pair() {
    let (bus, wire) = bus_with_terms("EUR");
    let asker = {
        let bus = Arc::clone(&bus);
        std::thread::spawn(move || {
            quoted_facts(&bus, Duration::from_secs(10), MOUNT, "XAUUSD", || plain("XAUUSD"))
        })
    };
    let wait_for = |line: String| {
        let until = Instant::now() + Duration::from_secs(10);
        while !wire.lines().contains(&line) {
            assert!(Instant::now() < until, "{line} never went out");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait_for(subscribe("XAUUSD"));
    bus.subscription("XAUUSD", true, "");
    bus.terms(terms_line("XAUUSD", "METAL", (1.0, 1.0), (0.01, 2), "USD"));
    wait_for(subscribe("EURUSD"));
    bus.subscription("EURUSD", true, "");
    let at = now();
    bus.deliver("EURUSD", 1.0799, 1.0801, at, at);
    let got = asker.join().expect("no panic").expect("gold on a euro ledger");
    assert_eq!((got.properties.min_qty, got.properties.tick_size), (1.0, 0.001));
    assert_eq!(got.quote_currency.as_deref(), Some("USD"));
    assert_eq!(got.account_currency.as_deref(), Some("EUR"));
    let rate = got.quote_rate.expect("a rate");
    assert!((rate - 1.0 / 1.08).abs() < 1e-12, "euros per dollar, not the flipped rate: {rate}");
}

/// **Terms that do not admit an instrument, and terms that cannot be read now, are told apart**: a contract of more
/// than one unit is a verdict (remembered), a sidecar that never states the line is "not now" (never remembered), and
/// a jar that announces no `terms` is refused in words before anything is subscribed.
#[test]
fn terms_that_refuse_are_a_verdict_and_terms_that_never_arrive_are_not_now() {
    let (bus, wire) = bus_with_terms("USD");
    let mut gas = terms_line("GAS.CMD/USD", "CFD", (1.0, 1.0), (0.001, 3), "USD");
    gas.per_contract = Some(100.0);
    let why = facts_with_terms(&bus, &wire, "GAS.CMD/USD", gas).expect_err("100 per contract");
    assert!(why.contains("100 units of the underlying per contract"), "{why}");
    assert!(!properties_refusal_is_transient(&why), "a verdict on the symbol: {why}");

    // a quiet market: the line never comes
    let (quiet, qwire) = bus_with_terms("USD");
    let why = quoted_facts(&quiet, Duration::from_millis(40), MOUNT, "XAGUSD", || plain("XAGUSD"))
        .expect_err("no terms");
    assert!(why.contains("stated no terms of `XAGUSD`") && why.contains("closed market"), "{why}");
    assert!(properties_refusal_is_transient(&why), "not now: {why}");
    assert_eq!(qwire.lines(), [subscribe("XAGUSD")]);

    // a jar that ticks and announces no terms: nothing is written to it
    let (old, owire) = self::bus(Some("USD"));
    let why = quoted_facts(&old, Duration::from_millis(40), MOUNT, "XAUUSD", || plain("XAUUSD"))
        .expect_err("an old jar");
    assert!(why.contains("predates the venue's terms") && why.contains("`XAUUSD`"), "{why}");
    assert!(properties_refusal_is_transient(&why), "{why}");
    assert!(owire.lines().is_empty(), "{:?}", owire.lines());
}

/// A USD-stating bus whose CONSUMER holds all eight of the sidecar's places (the data lane winning every place).
fn bus_with_every_place_taken() -> (Arc<QuoteBus>, crate::feed::Listening) {
    let bus = QuoteBus::new();
    // a jar that announces ticks and no acks, so a want waits for no answer
    let id =
        bus.attach(Arc::new(Wire::default()) as Arc<dyn LinePort>, &[FEATURE_TICKS.to_string()]);
    bus.set_account_currency(id, Some("USD"));
    bus.set_instrument_list(listed());
    let listening = bus.listen(Arc::new(|_: &crate::feed::PriceTick| {}));
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
    assert!(properties_refusal_is_transient(&why), "still a 'not now', never a verdict: {why}");
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

/// **Which instruments a converting node can open on a USD ledger, by the list itself**: every currency pair of
/// the recorded list (the coins and the metals aside, which keep their own refusals) is answered with a quote
/// currency, and every one quoted in another money than the dollar has a single conversion pair against the
/// dollar in the same list (so no admission waits on a cross). An account whose list lacks such a pair refuses the
/// cross by name at run time (`crate::account::quote_rate::conversion_pair`).
#[test]
fn every_currency_pair_of_the_table_is_priceable_against_the_dollar() {
    let rows = listed();
    let mut opened = 0;
    for row in &rows {
        let Ok((_, quote)) =
            crate::instrument::any_symbol::instrument_facts_quoted(&rows, &row.symbol)
        else {
            continue;
        };
        opened += 1;
        if quote != "USD" {
            // (USDJPY's own tick is the JPY conversion: the pair prices itself, no second subscription)
            crate::account::quote_rate::conversion_pair(&rows, &quote, "USD")
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
    assert_eq!(got.quote_currency.as_deref(), Some("CHF"));
    assert!((got.quote_rate.unwrap() - 1.0 / 0.88).abs() < 1e-12);
}
