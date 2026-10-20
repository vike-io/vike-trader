//! The multi-currency money's venue half (decision 0183) end to end against the scripted fake bridge
//! (`src/bin/fake_jforex_bridge.rs`) - no Java, no network. The real exec client reads the account's currency off
//! the sidecar's `ready` line onto the bus, and a conversion pair's rate is the mid of a tick the sidecar sends
//! after a `subscribe` only the rate reader wrote. What this proves is the Rust half over the real stdio protocol,
//! in BOTH directions of jar skew: a jar from before the currency field (`ticks-no-ccy` is exactly the price
//! lane's release, `old-jar` the one before it) leaves the currency UNKNOWN and still prices; a jar of this
//! release states it. DOCS-SHAPED: the Java half is `IAccount.getAccountCurrency()`
//! (<https://www.dukascopy.com/client/javadoc/com/dukascopy/api/IAccount.html>), unobserved on a live login.

use std::sync::Arc;
use std::time::Duration;

use vike_dukascopy::home_conversion::{RateError, quote_rate};
use vike_dukascopy::quotes::QuoteBus;
use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{ExecutionClient, event_channel};

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

fn spawn(mode: &str, bus: &Arc<QuoteBus>) -> DukascopyExecutionClient {
    let (events, _rx) = event_channel(64);
    let args: Vec<String> = if mode.is_empty() { vec![] } else { vec![mode.to_string()] };
    DukascopyExecutionClient::spawn_with_program_quoting(
        BRIDGE,
        &args,
        &config(),
        events,
        Arc::clone(bus),
    )
    .expect("the fake sidecar says ready")
}

/// A jar of this release states the account's currency on `ready`, and it is on the bus by the time the start
/// has returned (the trade lane reads it right after).
#[test]
fn a_jar_of_this_release_states_the_accounts_currency_before_the_start_returns() {
    let bus = QuoteBus::new();
    assert_eq!(bus.account_currency(), None, "no sidecar yet");
    let mut client = spawn("", &bus);
    assert_eq!(bus.account_currency().as_deref(), Some("USD"));
    client.detach();
    assert_eq!(bus.account_currency(), None, "it dies with the sidecar");

    let mut eur = spawn("ccy-eur", &bus);
    assert_eq!(bus.account_currency().as_deref(), Some("EUR"));
    eur.detach();
}

/// OLD JAR + NEW RUST: a jar that states no currency (the price lane's release, and the one before it) leaves it
/// unknown - never USD - and nothing else about the start changes. A word that is no currency code is unknown too.
#[test]
fn an_old_jar_leaves_the_currency_unknown_and_the_start_unchanged() {
    for mode in ["ticks-no-ccy", "old-jar", "ccy-bad"] {
        let bus = QuoteBus::new();
        let mut client = spawn(mode, &bus);
        assert_eq!(bus.account_currency(), None, "{mode}: unknown, never a default");
        client.detach();
    }
}

/// The rate of a JPY-quoted pair on a USD account over the real client and the real stdio protocol: USDJPY is the
/// conversion pair, the fake's tick is 150.09 / 150.11, and a yen is a FRACTION of a dollar.
#[test]
fn the_rate_is_the_inverse_mid_of_the_conversion_pairs_tick_over_the_real_client() {
    let bus = QuoteBus::new();
    let mut client = spawn("", &bus);
    let rate = quote_rate(&bus, "JPY", "USD", Duration::from_secs(10)).expect("a rate");
    assert_eq!(rate.quote_ccy, "JPY");
    let r = rate.account_per_quote.get();
    assert!((r - 1.0 / 150.10).abs() < 1e-9, "1 / 150.10, not 150.10: {r}");
    let age = vike_model::now_ms() - rate.as_of_ms;
    assert!((0..10_000).contains(&age), "stamped with the tick's own, current time: {age} ms old");
    // 0.10 yen on 100,000 units is 10,000 yen = 66.62 dollars
    assert_eq!((rate.account_per_quote.to_account(10_000.0) * 100.0).round() / 100.0, 66.62);

    // a pound is more than a dollar (GBPUSD 1.3499 / 1.3501)
    let gbp = quote_rate(&bus, "GBP", "USD", Duration::from_secs(10)).expect("a rate");
    assert!((gbp.account_per_quote.get() - 1.35).abs() < 1e-9, "{}", gbp.account_per_quote.get());
    client.detach();
}

/// OLD JAR + NEW RUST, the other half: a jar of the price lane's release prices (it announces `ticks`), so a rate
/// is still available from it - it is the GATEWAY that refuses a foreign-quoted pair for want of a stated currency.
/// A jar from before the price lane has no ticks at all and the rate is refused in words.
#[test]
fn a_jar_without_the_currency_still_prices_and_a_jar_without_ticks_is_refused_in_words() {
    let bus = QuoteBus::new();
    let mut lane = spawn("ticks-no-ccy", &bus);
    assert!(quote_rate(&bus, "JPY", "USD", Duration::from_secs(10)).is_ok());
    lane.detach();

    let bus = QuoteBus::new();
    let mut old = spawn("old-jar", &bus);
    match quote_rate(&bus, "JPY", "USD", Duration::from_millis(200)) {
        Err(RateError::Unavailable(why)) => {
            assert!(why.contains("predates the price lane"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    old.detach();
}

/// No sidecar at all (the account is not armed, the start failed): the rate is refused in words.
#[test]
fn no_sidecar_no_rate() {
    let bus = QuoteBus::new();
    match quote_rate(&bus, "JPY", "USD", Duration::from_millis(50)) {
        Err(RateError::Unavailable(why)) => assert!(why.contains("no JForex sidecar"), "{why}"),
        other => panic!("{other:?}"),
    }
}
