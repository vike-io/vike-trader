use std::cell::Cell;

use super::*;

/// A venue read that counts its asks and answers `answer`.
fn read(
    asks: &Cell<usize>,
    answer: Result<f64, String>,
) -> impl FnOnce() -> Result<f64, String> + '_ {
    move || {
        asks.set(asks.get() + 1);
        answer
    }
}

/// **A re-ask with nothing changed costs nothing**: the flapping-link case the review found, where every
/// `AdapterStart` that found the adapter running spent three venue reads for the same figure.
#[test]
fn a_confirmed_cap_is_kept_and_a_re_ask_reads_nothing() {
    let caps = LeverageCaps::default();
    let asks = Cell::new(0);
    let first = caps.confirmed("", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(10.0)));
    assert_eq!(first, Ok(10.0));
    for _ in 0..5 {
        let again = caps.confirmed("", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(99.0)));
        assert_eq!(again, Ok(10.0), "the kept figure, not a new read");
    }
    assert_eq!(asks.get(), 1, "one read for six asks");
}

/// A different coin, tier, account or STATED LEVERAGE is a different question and is read afresh.
#[test]
fn another_coin_tier_account_or_stated_leverage_is_read_afresh() {
    let caps = LeverageCaps::default();
    let asks = Cell::new(0);
    let ask = |account: &str, tier, coin: &str, stated| {
        caps.confirmed(account, tier, coin, stated, read(&asks, Ok(5.0))).expect("confirmed")
    };
    ask("", AccountTier::Demo, "BTC", 10.0);
    ask("", AccountTier::Demo, "ETH", 10.0);
    ask("", AccountTier::Live, "BTC", 10.0);
    ask("ALT", AccountTier::Demo, "BTC", 10.0);
    ask("", AccountTier::Demo, "BTC", 20.0);
    assert_eq!(asks.get(), 5, "five distinct questions, five reads");
    ask("", AccountTier::Demo, "BTC", 20.0);
    assert_eq!(asks.get(), 5, "and each is kept");
}

/// A restart of the adapter forgets that account's figures (and only that account's).
#[test]
fn a_restart_forgets_the_accounts_figures_and_no_other() {
    let caps = LeverageCaps::default();
    let asks = Cell::new(0);
    caps.confirmed("", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(10.0))).expect("a");
    caps.confirmed("ALT", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(10.0))).expect("b");
    caps.forget("");
    caps.confirmed("ALT", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(1.0))).expect("kept");
    assert_eq!(asks.get(), 2, "the other account kept its figure");
    caps.confirmed("", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(10.0))).expect("again");
    assert_eq!(asks.get(), 3, "the restarted account reads the venue again");
}

/// Only a CONFIRMED figure is kept: a refusal or a failed read is asked again next time.
#[test]
fn a_refusal_is_not_kept() {
    let caps = LeverageCaps::default();
    let asks = Cell::new(0);
    let refused = caps.confirmed(
        "",
        AccountTier::Demo,
        "BTC",
        10.0,
        read(&asks, Err("meta could not be read".into())),
    );
    assert_eq!(refused, Err("meta could not be read".to_string()));
    let confirmed = caps.confirmed("", AccountTier::Demo, "BTC", 10.0, read(&asks, Ok(7.0)));
    assert_eq!(confirmed, Ok(7.0));
    assert_eq!(asks.get(), 2, "the failure was not remembered as an answer");
}
