//! The ledger-currency bucket of an engine and the report when a node mixes them.

use super::*;
use vike_model::accounts::account_keys::AccountLabel;

type Engine = ExecutionEngine<Box<dyn ExecutionClient + Send>>;

fn engine() -> Engine {
    crate::paper_engine("oanda", "EURUSD", &AccountLabel::Default, &[], None, None).0
}

fn rows(keys: &[(&str, &str)]) -> Vec<(String, String)> {
    keys.iter().map(|(route, ccy)| (route.to_string(), ccy.to_string())).collect()
}

/// An engine nobody told its currency, and one told a dollar in any spelling, are ONE bucket: a node
/// of such engines (every node today) reports nothing and changes no number.
#[test]
fn an_untold_engine_and_a_told_dollar_share_the_default_bucket() {
    let untold = engine();
    assert_eq!(ledger_ccy_key(&untold), DEFAULT_LEDGER);
    for told in ["USD", "usd", " USDT "] {
        let mut e = engine();
        e.set_account_ccy(told).expect("a dollar code is told");
        assert_eq!(ledger_ccy_key(&e), DEFAULT_LEDGER, "{told:?}");
    }
}

/// An engine told another currency is its own bucket, in the code's canonical spelling.
#[test]
fn an_engine_told_another_currency_is_its_own_bucket() {
    let mut e = engine();
    e.set_account_ccy(" eur ").expect("told");
    assert_eq!(ledger_ccy_key(&e), "EUR");
}

#[test]
fn only_a_second_bucket_is_a_mix() {
    assert_eq!(mixed_ledger_currencies(&[]), None);
    assert_eq!(mixed_ledger_currencies(&rows(&[("binance", "USD")])), None);
    assert_eq!(mixed_ledger_currencies(&rows(&[("binance", "USD"), ("oanda", "USD")])), None);
    assert_eq!(
        mixed_ledger_currencies(&rows(&[("binance", "USD"), ("oanda", "EUR"), ("oanda#2", "EUR")])),
        Some(vec!["USD", "EUR"]),
        "distinct buckets, first-seen order"
    );
}

/// The report names every engine with its bucket and says what is wrong with the totals; a node of one
/// bucket logs nothing.
#[tracing_test::traced_test]
#[test]
fn the_report_names_every_engine_and_is_silent_for_one_currency() {
    report_mixed_ledger_currencies(&rows(&[("binance", "USD"), ("oanda", "USD")]));
    assert!(!logs_contain("DIFFERENT currencies"));

    report_mixed_ledger_currencies(&rows(&[("binance", "USD"), ("oanda", "EUR")]));
    assert!(logs_contain("DIFFERENT currencies"));
    assert!(logs_contain("binance=USD, oanda=EUR"));
}
