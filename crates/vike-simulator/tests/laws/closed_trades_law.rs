//! Closed-trade accounting on the bar path (`StrategyEngineConfig::closed_trades`): the engine hands the ledger
//! every bar's walk and close, so the run's block carries each trade's excursions, the account-wide
//! figures, buy and hold and the margin calls it counted; arming it moves nothing else in the result.
//!
//! One symbol, 1,000 of cash, no costs. 2 units bought at bar 1's open of 100 and sold at bar 3's open
//! of 104. Bar 1 walks 100, 97, 104, 103 (the low is nearer the open), bar 2 103 .. 101 .. 106, bar 3
//! opens at 104 and only then falls to 95, which the trade, out at the open, never meets.

use vike_analytics::BacktestResult;
use vike_analytics::closed_trades::TradeClosedTerms;
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_simulator::{OrderBrokerSimulator, ShortfallCover, StrategyEngine, StrategyEngineConfig};

const SYM: &str = "AAA";
const T0: i64 = 1_700_000_000_000;

fn bar(i: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(T0 + i * 60_000, o, h, l, c, 0.0)
}

/// Submits each `(step, side, size)` market order on its step, filled at the next bar's open.
struct Orders {
    plan: Vec<(usize, i32, f64)>,
}

impl Strategy<OrderBrokerSimulator> for Orders {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        for &(step, side, size) in &self.plan {
            if step == ctx.index {
                ctx.submit(SYM, side, size, 0.0, true, None);
            }
        }
    }
}

fn run(
    plan: Vec<(usize, i32, f64)>,
    tape: Vec<Bar>,
    params: StrategyEngineConfig,
) -> BacktestResult {
    let params = StrategyEngineConfig { cash: 1_000.0, ..params };
    StrategyEngine::new(vec![(SYM.to_string(), tape)], Orders { plan }, params).run()
}

fn tape() -> Vec<Bar> {
    vec![
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 104.0, 97.0, 103.0),
        bar(2, 103.0, 106.0, 101.0, 105.0),
        bar(3, 104.0, 108.0, 95.0, 96.0),
        bar(4, 96.0, 96.0, 96.0, 96.0),
    ]
}

fn armed() -> StrategyEngineConfig {
    StrategyEngineConfig { closed_trades: Some(TradeClosedTerms::default()), ..Default::default() }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn the_engine_marks_every_bar_for_the_excursions_and_the_account() {
    let r = run(vec![(0, 1, 2.0), (2, -1, 2.0)], tape(), armed());
    let c = r.closed_trades.expect("armed");
    let [t] = &c.trades[..] else { panic!("{:?}", c.trades) };
    // Met 97 .. 106 while held; bar 3's 95 and 108 came after the exit at its open.
    assert!(close(t.net, 8.0) && close(t.runup, 12.0) && close(t.drawdown, -6.0), "{t:?}");
    assert_eq!(t.bars, 2);
    let a = &c.account;
    // From the capital (no trade closed yet) to bar 1's low, and to bar 2's high.
    assert!(close(a.drawdown, 6.0) && close(a.drawdown_percent, 0.006), "{a:?}");
    assert!(close(a.runup, 12.0) && close(a.runup_percent, 0.012));
    assert!(close(a.total_pnl, 8.0) && close(a.net_profit_percent, 0.008));
    // The first entry's 100 to the last close's 96.
    assert!(close(a.buy_hold_percent, -0.04) && close(a.buy_hold, -40.0));
    assert!(close(a.outperformance, 0.048));
    assert_eq!(a.margin_calls, 0);
    assert_eq!(c.buy_hold_curve.len(), 4, "a point at each close from the entry's bar");
}

#[test]
fn arming_it_moves_nothing_else() {
    let plan = vec![(0, 1, 2.0), (2, -1, 2.0)];
    let mut on = run(plan.clone(), tape(), armed());
    let off = run(plan, tape(), StrategyEngineConfig::default());
    assert!(on.closed_trades.take().is_some() && off.closed_trades.is_none());
    assert_eq!(format!("{on:?}"), format!("{off:?}"));
}

#[test]
fn the_block_counts_the_engines_margin_calls() {
    // 45 units at 100 on 1,000 at a 20% margin; bar 2's low of 90 leaves 550 against 810.
    let rule = ShortfallCover { cover: 4.0, margin_long: 0.2, margin_short: 0.2, step: 0.01 };
    let params = StrategyEngineConfig { shortfall_cover: Some(rule), ..armed() };
    let tape = vec![
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 99.0, 99.0, 90.0, 99.0),
    ];
    let mut engine = StrategyEngine::new(
        vec![(SYM.to_string(), tape)],
        Orders { plan: vec![(0, 1, 45.0)] },
        StrategyEngineConfig { cash: 1_000.0, ..params },
    );
    let r = engine.run();
    assert!(engine.core.shortfall_cover_fills > 0, "the scenario margin-calls");
    let calls = r.closed_trades.expect("armed").account.margin_calls;
    assert_eq!(calls as u64, engine.core.shortfall_cover_fills);
}
