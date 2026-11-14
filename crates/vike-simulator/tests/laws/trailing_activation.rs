//! `OrderBrokerSimulator::submit_trailing_activated`: a trailing stop that waits for a price level, then trails,
//! with both able to fall in one bar (`OrderBrokerSimulator::fill_price_for`, `trailing_path_fill`).
//!
//! The long cases below rest a sell with the level at 110 and a trail of 5, over a position entered at
//! 100, on hand-computed bars walked open, nearer extreme, farther extreme, close. What each test pins:
//!
//! - before the level nothing fires, however far the price falls;
//! - activation and the stop in one bar, whichever extreme comes first;
//! - a bar that activates and closes above the stop keeps trailing into the next;
//! - a gap through the level trails from the open, not from the level;
//! - a gap through the stop fills at the open;
//! - a live order ratchets along the path inside a bar, which the plain law cannot;
//! - a mark already past the level at the submit activates at once, from that mark;
//! - the buy side mirrors all of it;
//! - the sub-bar, cash-gated and tick lanes agree;
//! - `OrderBrokerSimulator::submit_trailing` carries no level and stays the frozen law, bit for bit.

use vike_marketdata::{Bar, TickQuote};
use vike_model::{Fill, OrderKind, OrderResting, Strategy, order_fill_price};
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "TEST";
const TRAIL: f64 = 5.0;
const LEVEL: f64 = 110.0;

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, 100.0)
}

fn params() -> StrategyEngineConfig {
    StrategyEngineConfig { cash: 100_000.0, ..StrategyEngineConfig::default() }
}

#[derive(Clone, Copy)]
enum Act {
    Enter(i32),
    /// the activated verb: `(side, trail, level)`
    Activated(i32, f64, f64),
    /// the plain verb: `(side, trail)`
    Plain(i32, f64),
}

/// Plays `script` (bar index, action) in order and collects fills.
struct Scripted {
    script: Vec<(usize, Act)>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Scripted {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, act) in self.script.clone() {
            if at != ctx.index {
                continue;
            }
            match act {
                Act::Enter(side) => ctx.submit(SYM, side, 1.0, 0.0, true, None),
                Act::Activated(side, trail, level) => {
                    ctx.submit_trailing_activated(SYM, side, 1.0, trail, level, 0.0, true)
                }
                Act::Plain(side, trail) => ctx.submit_trailing(SYM, side, 1.0, trail, 0.0, true),
            }
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn run(
    script: Vec<(usize, Act)>,
    bars: Vec<Bar>,
    p: StrategyEngineConfig,
) -> StrategyEngine<Scripted> {
    let strat = Scripted { script, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strat, p);
    engine.run();
    engine
}

/// `then` as bars stamped 3 000, 4 000, ... after the two flat bars at 100 (entry at the second's open).
fn after_entry(then: &[(f64, f64, f64, f64)]) -> Vec<Bar> {
    let mut bars =
        vec![bar(1_000, 100.0, 100.0, 100.0, 100.0), bar(2_000, 100.0, 100.0, 100.0, 100.0)];
    for (i, &(o, h, l, c)) in then.iter().enumerate() {
        bars.push(bar(3_000 + 1_000 * i as i64, o, h, l, c));
    }
    bars
}

/// A long entered on bar 0 (fills at bar 1's open), the activated sell rested on bar 1.
fn long_then_exit() -> Vec<(usize, Act)> {
    vec![(0, Act::Enter(1)), (1, Act::Activated(-1, TRAIL, LEVEL))]
}

/// The exit's `(side, price, ts)`, or `None` when only the entry filled.
fn exit(engine: &StrategyEngine<Scripted>) -> Option<(i32, f64, i64)> {
    let fills = &engine.strategy.fills;
    assert!(fills.len() <= 2, "one exit at most: {fills:?}");
    fills.get(1).map(|f| (f.side, f.price, f.timestamp))
}

fn long_exit_over(then: &[(f64, f64, f64, f64)]) -> Option<(i32, f64, i64)> {
    exit(&run(long_then_exit(), after_entry(then), params()))
}

#[test]
fn before_the_level_nothing_fires_however_far_the_price_falls() {
    // A plain trailing stop would fire here: 100 - 5 = 95 is well above the low.
    let engine = run(long_then_exit(), after_entry(&[(100.0, 105.0, 80.0, 90.0)]), params());
    assert_eq!(exit(&engine), None);
    let resting = engine.core.pending_of(SYM);
    assert_eq!(resting.len(), 1, "it still rests");
    assert_eq!(resting[0].extreme, None, "not activated");
}

#[test]
fn activation_and_stop_in_one_bar_with_the_high_first() {
    // 108 -> 112 (active, extreme 112) -> 100 (through 107) -> 101.
    assert_eq!(long_exit_over(&[(108.0, 112.0, 100.0, 101.0)]), Some((-1, 107.0, 3_000)));
}

#[test]
fn activation_and_stop_in_one_bar_with_the_low_first() {
    // 100 -> 99 (not active) -> 112 (active, extreme 112) -> close 100 (through 107).
    assert_eq!(long_exit_over(&[(100.0, 112.0, 99.0, 100.0)]), Some((-1, 107.0, 3_000)));
}

#[test]
fn a_bar_that_activates_and_closes_above_the_stop_keeps_trailing_into_the_next() {
    // Bar 1: extreme 112, close 110 stays above 107. Bar 2: 109 -> 109 -> 100 (through 107).
    assert_eq!(
        long_exit_over(&[(100.0, 112.0, 99.0, 110.0), (109.0, 109.0, 100.0, 101.0)]),
        Some((-1, 107.0, 4_000))
    );
}

#[test]
fn a_gap_through_the_level_trails_from_the_open_not_the_level() {
    // Opens at 115, past 110: extreme 115 at once, so the stop is at 110, not at 105.
    assert_eq!(long_exit_over(&[(115.0, 115.0, 100.0, 101.0)]), Some((-1, 110.0, 3_000)));
    // Over two bars: 115 -> 116 -> 112 -> 113 holds a stop at 111; the next bar reaches 110.
    assert_eq!(
        long_exit_over(&[(115.0, 116.0, 112.0, 113.0), (112.0, 112.0, 110.0, 110.5)]),
        Some((-1, 111.0, 4_000))
    );
}

#[test]
fn a_gap_through_the_stop_fills_at_the_open() {
    // Extreme 112, stop 107; the next bar opens at 100, below it.
    assert_eq!(
        long_exit_over(&[(100.0, 112.0, 99.0, 110.0), (100.0, 101.0, 95.0, 96.0)]),
        Some((-1, 100.0, 4_000))
    );
}

#[test]
fn a_live_order_ratchets_along_the_path_inside_a_bar() {
    // Extreme 112. Next bar 118 -> 120 -> 112: the stop rises to 115 on the way up, and is hit coming
    // down. The plain law would test only the prior 107, against the low 112, and not fire.
    assert_eq!(
        long_exit_over(&[(100.0, 112.0, 99.0, 110.0), (118.0, 120.0, 112.0, 113.0)]),
        Some((-1, 115.0, 4_000))
    );
}

#[test]
fn a_mark_already_past_the_level_activates_at_the_submit_from_that_mark() {
    // Bar 1 closes at 112, past the level, so the order is live with extreme 112 before the next bar,
    // which never reaches 110 and fires the stop at 107. Unactivated it would rest untouched.
    let bars = vec![
        bar(1_000, 100.0, 100.0, 100.0, 100.0),
        bar(2_000, 100.0, 112.0, 100.0, 112.0),
        bar(3_000, 109.0, 109.0, 100.0, 101.0),
    ];
    assert_eq!(exit(&run(long_then_exit(), bars, params())), Some((-1, 107.0, 3_000)));
}

/// A short entered on bar 0, the activated buy rested on bar 1: level 90, trail 5.
fn short_exit_over(then: &[(f64, f64, f64, f64)]) -> Option<(i32, f64, i64)> {
    let script = vec![(0, Act::Enter(-1)), (1, Act::Activated(1, TRAIL, 90.0))];
    exit(&run(script, after_entry(then), params()))
}

#[test]
fn the_buy_side_mirrors_it() {
    // Before the level: a rally does nothing.
    assert_eq!(short_exit_over(&[(100.0, 120.0, 95.0, 110.0)]), None);
    // One bar: 92 -> 88 (active, extreme 88) -> 100 (through 93).
    assert_eq!(short_exit_over(&[(92.0, 100.0, 88.0, 99.0)]), Some((1, 93.0, 3_000)));
    // A gap through the level trails from the open: 85 -> 85 -> 92 (through 90).
    assert_eq!(short_exit_over(&[(85.0, 92.0, 85.0, 91.0)]), Some((1, 90.0, 3_000)));
    // A gap through the stop fills at the open: extreme 88, stop 93, next bar opens at 100.
    assert_eq!(
        short_exit_over(&[(92.0, 92.0, 88.0, 89.0), (100.0, 101.0, 99.0, 100.0)]),
        Some((1, 100.0, 4_000))
    );
}

#[test]
fn the_sub_bar_and_cash_gated_lanes_agree() {
    let coarse = (108.0, 112.0, 100.0, 101.0);
    let tape = StrategyEngineConfig {
        // the same bar as its path: one price, then one leg per sub-bar
        granular_by_symbol: vec![(
            SYM.into(),
            vec![
                bar(3_000, 108.0, 108.0, 108.0, 108.0),
                bar(3_001, 108.0, 112.0, 108.0, 112.0),
                bar(3_002, 112.0, 112.0, 100.0, 100.0),
                bar(3_003, 100.0, 101.0, 100.0, 101.0),
            ],
        )],
        ..params()
    };
    let walked = run(long_then_exit(), after_entry(&[coarse]), tape);
    assert_eq!(exit(&walked), Some((-1, 107.0, 3_002)));

    let gated = StrategyEngineConfig { cash_gate: true, ..params() };
    let engine = run(long_then_exit(), after_entry(&[coarse]), gated);
    assert_eq!(exit(&engine), Some((-1, 107.0, 3_000)));
}

/// On the first quote: the activated sell, level 110, trail 5.
struct TickArm {
    armed: bool,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for TickArm {
    fn on_quote_tick(&mut self, ctx: &mut OrderBrokerSimulator, _q: &TickQuote) {
        if !self.armed {
            self.armed = true;
            ctx.submit_trailing_activated(SYM, -1, 1.0, TRAIL, LEVEL, 0.0, true);
        }
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

#[test]
fn the_tick_lane_activates_and_trails_on_the_bid_and_fills_at_it() {
    // bid 111 activates with extreme 111 (the mid 111.5 would put the stop at 106.5), bid 106.2 holds
    // above the stop at 106, and bid 105 fires at the bid, the tick tier's own price.
    let quote = |ts, bid, ask| vike_simulator::test_support::quote(SYM, ts, bid, ask, 1.0, 1.0);
    let ticks = vec![(
        SYM.to_string(),
        vec![
            quote(1_000, 108.0, 109.0),
            quote(2_000, 111.0, 112.0),
            quote(3_000, 106.2, 107.2),
            quote(4_000, 105.0, 106.0),
        ],
    )];
    let p = StrategyEngineConfig { fill_model: FillModelKind::Tick, ..params() };
    let strat = TickArm { armed: false, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strat, p);
    engine.run_ticks(&ticks);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!((fills[0].side, fills[0].price, fills[0].timestamp), (-1, 105.0, 4_000));
}

#[test]
fn the_plain_verb_carries_no_level_and_stays_the_frozen_law() {
    let script = vec![(0, Act::Enter(1)), (1, Act::Plain(-1, TRAIL))];

    let resting = run(script.clone(), after_entry(&[]), params());
    let o = &resting.core.pending_of(SYM)[0];
    assert_eq!((o.price, o.extreme), (None, Some(100.0)), "no level, trailing from the mark");

    // The bars the activated order stops on at 107 in one bar: the plain law trails from 100 and
    // fires a bar later, at that bar's open. The oracle walked by hand is the frozen law itself.
    let then = [(108.0, 112.0, 100.0, 101.0), (101.0, 102.0, 90.0, 91.0)];
    let engine = run(script, after_entry(&then), params());
    let mut oracle = OrderResting::new(OrderKind::Trailing, -1, 1.0);
    oracle.trail = Some(TRAIL);
    oracle.extreme = Some(100.0);
    let first = bar(3_000, then[0].0, then[0].1, then[0].2, then[0].3);
    let second = bar(4_000, then[1].0, then[1].1, then[1].2, then[1].3);
    assert_eq!(order_fill_price(&mut oracle, &first), None);
    let want = order_fill_price(&mut oracle, &second).expect("fires");
    let fired = &engine.strategy.fills[1];
    assert_eq!((fired.side, fired.timestamp), (-1, 4_000));
    assert_eq!(fired.price.to_bits(), want.to_bits());
    assert_eq!(fired.price, 101.0);
}
