//! Orders and cancels from `Strategy::on_fill` on the sub-bar lane (`StrategyEngine::fill_pending_granular`).
//!
//! What each test pins, over one coarse step split into three sub-bars:
//!
//! - an order submitted from `on_fill` rests and fills on the NEXT sub-bar, never the one that made the fill;
//! - a cancel from `on_fill` removes what still rests, and `on_fill` sees it resting;
//! - an order that already triggered on the same sub-bar is past a cancel's reach, as on the coarse
//!   lane (`StrategyEngine::fill_pending`), so this is not a new way to lose a fill;
//! - a strategy that leaves the book alone gets its fills in pending order, unchanged.

use vike_marketdata::Bar;
use vike_model::{Fill, Strategy};
use vike_simulator::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "TEST";

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, 100.0)
}

/// What `on_fill` does on the run's first fill.
#[derive(Clone, Copy)]
enum React {
    Nothing,
    SubmitSell,
    CancelAll,
}

#[derive(Clone, Copy)]
enum Rest {
    Market,
    /// a sell stop at this price
    Stop(f64),
    /// a buy limit at this price
    Limit(f64),
}

/// Rests `orders` on bar 0 and reacts to the first fill.
struct Reactor {
    orders: Vec<Rest>,
    react: React,
    resting_seen_on_first_fill: Option<usize>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Reactor {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if ctx.index != 0 {
            return;
        }
        for order in &self.orders {
            match *order {
                Rest::Market => ctx.submit(SYM, 1, 1.0, 0.0, true, None),
                Rest::Stop(px) => ctx.submit_stop(SYM, -1, 1.0, px, 0.0, true),
                Rest::Limit(px) => ctx.submit_limit(SYM, 1, 1.0, px, 0.0, true, None),
            }
        }
    }
    fn on_fill(&mut self, ctx: &mut OrderBrokerSimulator, fill: &Fill) {
        let first = self.fills.is_empty();
        self.fills.push(fill.clone());
        if !first {
            return;
        }
        self.resting_seen_on_first_fill = Some(ctx.pending_of(SYM).len());
        match self.react {
            React::Nothing => {}
            React::SubmitSell => ctx.submit(SYM, -1, 1.0, 0.0, true, None),
            React::CancelAll => ctx.cancel_all(SYM),
        }
    }
}

/// Bar 0 is flat; step 1 is three sub-bars: 100 reaching 99, a dip to 96, then a fall through 95.
fn run(orders: Vec<Rest>, react: React) -> StrategyEngine<Reactor> {
    let bars = vec![bar(1_000, 100.0, 100.0, 100.0, 100.0), bar(2_000, 100.0, 101.0, 90.0, 91.0)];
    let p = StrategyEngineConfig {
        cash: 100_000.0,
        granular_by_symbol: vec![(
            SYM.into(),
            vec![
                bar(2_000, 100.0, 101.0, 99.0, 100.0),
                bar(2_250, 98.0, 98.0, 96.0, 97.0),
                bar(2_500, 97.0, 98.0, 90.0, 91.0),
            ],
        )],
        ..StrategyEngineConfig::default()
    };
    let strat = Reactor { orders, react, resting_seen_on_first_fill: None, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strat, p);
    engine.run();
    engine
}

fn fills(engine: &StrategyEngine<Reactor>) -> Vec<(i32, f64, i64)> {
    engine.strategy.fills.iter().map(|f| (f.side, f.price, f.timestamp)).collect()
}

#[test]
fn an_order_submitted_from_on_fill_fills_on_the_next_sub_bar() {
    let engine = run(vec![Rest::Market], React::SubmitSell);
    assert_eq!(
        fills(&engine),
        vec![(1, 100.0, 2_000), (-1, 98.0, 2_250)],
        "the buy on the first sub-bar, the sell it submitted at the second's open"
    );
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
}

#[test]
fn a_cancel_from_on_fill_removes_what_still_rests() {
    let engine = run(vec![Rest::Market, Rest::Stop(95.0)], React::CancelAll);
    assert_eq!(
        engine.strategy.resting_seen_on_first_fill,
        Some(1),
        "on_fill sees the stop resting"
    );
    assert_eq!(
        fills(&engine),
        vec![(1, 100.0, 2_000)],
        "and the third sub-bar's fall fires nothing"
    );
    assert!(engine.core.pending_of(SYM).is_empty());
    assert_eq!(engine.core.position_of(SYM).size, 1.0);
}

#[test]
fn an_order_triggered_on_the_same_sub_bar_is_past_a_cancels_reach() {
    let engine = run(vec![Rest::Market, Rest::Market], React::CancelAll);
    assert_eq!(fills(&engine), vec![(1, 100.0, 2_000), (1, 100.0, 2_000)]);
}

#[test]
fn a_strategy_that_leaves_the_book_alone_fills_in_pending_order() {
    // Listed limit first: this lane keeps pending order, it does not sort a stop or market ahead.
    let engine = run(vec![Rest::Limit(99.0), Rest::Market, Rest::Stop(95.0)], React::Nothing);
    assert_eq!(fills(&engine), vec![(1, 99.0, 2_000), (1, 100.0, 2_000), (-1, 95.0, 2_500)]);
    assert_eq!(engine.core.position_of(SYM).size, 1.0);
}
