//! A script works by candles AND by ticks (decision 0245): `fn on_tick()` is run on the mount's quote lane,
//! sampled at most once per `TICK_HOOK_MIN_MS` of event time, beside `on_bar`. A script that does not define
//! the hook is untouched by a quote, and a one-sided or crossed quote runs nothing.

mod common;
use common::MockBroker;
use common::test_support::bar;
use vike_marketdata::QuoteTick;
use vike_model::Strategy;
use vike_script::{RhaiStrategy, TICK_HOOK_MIN_MS};

fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
    QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 0.0, ask_size: 0.0, symbol: String::new() }
}

const TICKER: &str = "fn on_tick() { buy(1.0); }";

#[test]
fn a_quote_runs_the_on_tick_hook_and_its_orders_reach_the_broker() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(TICKER).unwrap();
    let mut b = MockBroker::default();
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.submits, vec![(1, 1.0)]);
}

/// The hook shares `price()`/`position()` with `on_bar`, and both hooks run on their own lane: a bar does not
/// run `on_tick`, a quote does not run `on_bar`.
#[test]
fn a_bar_runs_on_bar_and_a_quote_runs_on_tick_never_the_other() {
    let src = "fn on_bar() { sell(2.0); } fn on_tick() { buy(1.0); }";
    let mut strat = RhaiStrategy::<MockBroker>::compile(src).unwrap();
    let mut b = MockBroker::default();
    b.bars.push(bar(100.0));
    strat.on_bar(&mut b, &bar(100.0));
    assert_eq!(b.submits, vec![(-1, 2.0)], "a bar: on_bar only");
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.submits, vec![(-1, 2.0), (1, 1.0)], "a quote: on_tick only");
}

/// A script that never wrote `on_tick` pays nothing and trades nothing on a quote, and a script that wrote
/// only `on_tick` ignores bars: no hook is attempted for a name the script does not define.
#[test]
fn an_undefined_hook_is_never_attempted() {
    let mut bar_only = RhaiStrategy::<MockBroker>::compile("fn on_bar() { buy(1.0); }").unwrap();
    let mut b = MockBroker::default();
    bar_only.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert!(b.submits.is_empty(), "a script without on_tick: a quote trades nothing");

    let mut tick_only = RhaiStrategy::<MockBroker>::compile(TICKER).unwrap();
    let mut b = MockBroker::default();
    b.bars.push(bar(100.0));
    tick_only.on_bar(&mut b, &bar(100.0));
    assert!(b.submits.is_empty(), "a script without on_bar: a bar trades nothing");
}

/// SAMPLED: the first quote runs, quotes closer than the bound in event time do not, the next one at the bound
/// does, and a clock that steps back runs again (nothing is held for a time that already passed).
#[test]
fn the_hook_is_sampled_not_run_per_quote() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(TICKER).unwrap();
    let mut b = MockBroker::default();
    let t0 = 10_000;
    strat.on_quote_tick(&mut b, &quote(t0, 99.0, 101.0));
    strat.on_quote_tick(&mut b, &quote(t0 + 1, 99.0, 101.0));
    strat.on_quote_tick(&mut b, &quote(t0 + TICK_HOOK_MIN_MS - 1, 99.0, 101.0));
    assert_eq!(b.submits.len(), 1, "two quotes inside the bound: one run");
    strat.on_quote_tick(&mut b, &quote(t0 + TICK_HOOK_MIN_MS, 99.0, 101.0));
    assert_eq!(b.submits.len(), 2, "a quote at the bound runs it again");
    strat.on_quote_tick(&mut b, &quote(t0, 99.0, 101.0));
    assert_eq!(b.submits.len(), 3, "an event time that went backwards runs it");
}

/// A quote that is not a two-sided price (half a mid, a crossed book, a NaN) runs nothing and spends no sample.
#[test]
fn a_quote_that_is_not_a_price_runs_nothing() {
    let mut strat = RhaiStrategy::<MockBroker>::compile(TICKER).unwrap();
    let mut b = MockBroker::default();
    for (bid, ask) in [(0.0, 101.0), (101.0, 99.0), (f64::NAN, 101.0), (99.0, f64::INFINITY)] {
        strat.on_quote_tick(&mut b, &quote(1_000, bid, ask));
    }
    assert!(b.submits.is_empty());
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.submits.len(), 1, "the bad quotes spent no sample: the same instant still runs");
}
