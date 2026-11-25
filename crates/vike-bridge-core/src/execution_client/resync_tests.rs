//! `replay_foreign`'s walk over a scripted venue: which symbols cost a request, in what order, and where a
//! "slow down" or a refused read budget ends it.

use super::*;
use std::cell::RefCell;

/// A venue whose reads answer `slow` for the symbols named there, `[]` otherwise; it records every read.
struct Venue {
    reads: RefCell<Vec<String>>,
    slow: &'static [&'static str],
    failing: &'static [&'static str],
}

impl Venue {
    fn new(slow: &'static [&'static str], failing: &'static [&'static str]) -> Self {
        Self { reads: RefCell::new(Vec::new()), slow, failing }
    }

    /// One read of `symbol`'s `what`.
    fn get(&self, symbol: &str, what: &str) -> Result<Value, VenueApiError> {
        self.reads.borrow_mut().push(format!("{symbol}:{what}"));
        if self.slow.contains(&symbol) {
            return Err(VenueApiError { code: 429, msg: "slow down".into() });
        }
        if self.failing.contains(&symbol) {
            return Err(VenueApiError { code: 500, msg: "broken".into() });
        }
        Ok(serde_json::json!([symbol]))
    }

    /// The walk over `foreign`, two reads per symbol, admitting every label that does not end in `.P`.
    fn walk(&self, foreign: &[&str]) -> Vec<String> {
        let foreign: Vec<String> = foreign.iter().map(|s| s.to_string()).collect();
        let mapped = |symbol: &str, rows: &dyn Fn(Result<Value, VenueApiError>) -> Value| {
            let _ = rows(self.get(symbol, "orders"));
            let _ = rows(self.get(symbol, "trades"));
            Vec::<Event>::new()
        };
        replay_foreign(
            "binance",
            "BTCUSDT",
            &foreign,
            |label| !label.ends_with(".P"),
            |e| e.code == 429,
            |rows| mapped("BTCUSDT", rows),
            &mapped,
        );
        self.reads.take()
    }
}

#[test]
fn the_mounted_symbol_goes_first_and_only_admitted_new_labels_cost_a_request() {
    let venue = Venue::new(&[], &[]);
    let reads =
        venue.walk(&["ETHUSDT", "", "btcusdt", "ethusdt", "SOLUSDT.P", "XRPUSDT", "BTCUSDT"]);
    assert_eq!(
        reads,
        [
            "BTCUSDT:orders",
            "BTCUSDT:trades",
            "ETHUSDT:orders",
            "ETHUSDT:trades",
            "XRPUSDT:orders",
            "XRPUSDT:trades"
        ]
    );
}

#[test]
fn a_slow_down_ends_the_walk_once_that_symbols_reads_are_made() {
    let venue = Venue::new(&["ETHUSDT"], &[]);
    let reads = venue.walk(&["ETHUSDT", "XRPUSDT"]);
    assert_eq!(reads, ["BTCUSDT:orders", "BTCUSDT:trades", "ETHUSDT:orders", "ETHUSDT:trades"]);
}

#[test]
fn a_slow_down_on_the_mounted_read_reads_no_foreign_symbol() {
    let venue = Venue::new(&["BTCUSDT"], &[]);
    assert_eq!(venue.walk(&["ETHUSDT"]), ["BTCUSDT:orders", "BTCUSDT:trades"]);
}

#[test]
fn a_failed_read_that_is_no_slow_down_costs_its_own_rows_only() {
    let venue = Venue::new(&[], &["ETHUSDT"]);
    let reads = venue.walk(&["ETHUSDT", "XRPUSDT"]);
    assert_eq!(reads.len(), 6, "the walk went on: {reads:?}");
}

/// A foreign symbol the request budget refuses is not read, and nothing after it is: the rest is reconcile's, as after
/// a "slow down" (the test sink of `crate::read_budget` refuses its one symbol and admits every other read).
#[test]
fn a_refused_foreign_symbol_ends_the_walk_before_its_reads() {
    crate::read_budget::install_refusing_sink_for_tests();
    let venue = Venue::new(&[], &[]);
    let reads = venue.walk(&["ETHUSDT", crate::read_budget::REFUSED_IN_TESTS, "XRPUSDT"]);
    assert_eq!(reads, ["BTCUSDT:orders", "BTCUSDT:trades", "ETHUSDT:orders", "ETHUSDT:trades"]);
}
