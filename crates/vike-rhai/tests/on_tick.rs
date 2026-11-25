//! A script works by candles AND by ticks (decision 0245): `fn on_tick(strategy)` is run on the mount's quote
//! lane, beside `fn on_bar(strategy)`, from the run's first bar on, sampled at most once per `TICK_HOOK_MIN_MS`
//! of event time. A script that does not define the hook is untouched by a quote, and a one-sided or crossed
//! quote runs nothing.
//!
//! The mock never fills, so an order re-issued under one name would only amend the one still resting: the
//! tick hook here cancels its order and places it anew, so each RUN is one placement on the seam.

use vike_marketdata::TickQuote;
use vike_model::Strategy;
use vike_rhai::test_support::{OrderBrokerMock, bar};
use vike_rhai::{RhaiStrategy, TICK_HOOK_MIN_MS};

fn quote(ts: i64, bid: f64, ask: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    }
}

const TICKER: &str = r#"fn on_bar(strategy) {}
fn on_tick(strategy) { strategy.cancel("t"); strategy.order("t", OrderSide::Long); }"#;

/// Compiles `src` and runs its first bar, from which the tick hook runs.
fn started(src: &str) -> (RhaiStrategy<OrderBrokerMock>, OrderBrokerMock) {
    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src).unwrap();
    let mut b = OrderBrokerMock::default();
    b.bars.push(bar(100.0));
    strat.on_bar(&mut b, &bar(100.0));
    (strat, b)
}

#[test]
fn a_quote_runs_the_on_tick_hook_and_its_orders_reach_the_broker() {
    let mut before = RhaiStrategy::<OrderBrokerMock>::compile(TICKER).unwrap();
    let mut b = OrderBrokerMock::default();
    before.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert!(b.seam.is_empty(), "before the first bar the run has no symbol: nothing runs");

    let (mut strat, mut b) = started(TICKER);
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.orders(), vec![(1, 1.0)]);
    assert!(b.seam[0].0.starts_with("t#"), "{:?}", b.seam);
}

/// The hook shares `price()`/`position()` with `on_bar`, and both hooks run on their own lane: a bar does not
/// run `on_tick`, a quote does not run `on_bar`.
#[test]
fn a_bar_runs_on_bar_and_a_quote_runs_on_tick_never_the_other() {
    let src = r#"fn on_bar(strategy) { strategy.order("b", OrderSide::Short).size(2.0); }
                 fn on_tick(strategy) { strategy.order("t", OrderSide::Long); }"#;
    let (mut strat, mut b) = started(src);
    assert_eq!(b.orders(), vec![(-1, 2.0)], "a bar: on_bar only");
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.orders(), vec![(-1, 2.0), (1, 1.0)], "a quote: on_tick only");
    assert!(b.seam[0].0.starts_with("b#") && b.seam[1].0.starts_with("t#"), "{:?}", b.seam);
}

/// A script that never wrote `on_tick` pays nothing and trades nothing on a quote, and one that wrote only
/// `on_tick` is refused at compile: its hook enters the order language through `on_bar(strategy)`.
#[test]
fn an_undefined_hook_is_never_attempted() {
    let (mut bar_only, mut b) =
        started(r#"fn on_bar(strategy) { strategy.order("b", OrderSide::Long); }"#);
    assert_eq!(b.seam.len(), 1, "the bar ran");
    bar_only.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.seam.len(), 1, "a script without on_tick: a quote trades nothing");

    let tick_only = r#"fn on_tick(strategy) { strategy.order("t", OrderSide::Long); }"#;
    let err = RhaiStrategy::<OrderBrokerMock>::compile(tick_only).err().expect("refused");
    assert!(err.summary().contains("needs `fn on_bar(strategy)`"), "{err}");
}

/// SAMPLED: the first quote runs, quotes closer than the bound in event time do not, the next one at the bound
/// does, and a clock that steps back runs again (nothing is held for a time that already passed).
#[test]
fn the_hook_is_sampled_not_run_per_quote() {
    let (mut strat, mut b) = started(TICKER);
    let t0 = 10_000;
    strat.on_quote_tick(&mut b, &quote(t0, 99.0, 101.0));
    strat.on_quote_tick(&mut b, &quote(t0 + 1, 99.0, 101.0));
    strat.on_quote_tick(&mut b, &quote(t0 + TICK_HOOK_MIN_MS - 1, 99.0, 101.0));
    assert_eq!(b.seam.len(), 1, "two quotes inside the bound: one run");
    strat.on_quote_tick(&mut b, &quote(t0 + TICK_HOOK_MIN_MS, 99.0, 101.0));
    assert_eq!(b.seam.len(), 2, "a quote at the bound runs it again");
    strat.on_quote_tick(&mut b, &quote(t0, 99.0, 101.0));
    assert_eq!(b.seam.len(), 3, "an event time that went backwards runs it");
}

/// A quote that is not a two-sided price (half a mid, a crossed book, a NaN) runs nothing and spends no sample.
#[test]
fn a_quote_that_is_not_a_price_runs_nothing() {
    let (mut strat, mut b) = started(TICKER);
    for (bid, ask) in [(0.0, 101.0), (101.0, 99.0), (f64::NAN, 101.0), (99.0, f64::INFINITY)] {
        strat.on_quote_tick(&mut b, &quote(1_000, bid, ask));
    }
    assert!(b.seam.is_empty());
    strat.on_quote_tick(&mut b, &quote(1_000, 99.0, 101.0));
    assert_eq!(b.seam.len(), 1, "the bad quotes spent no sample: the same instant still runs");
}
