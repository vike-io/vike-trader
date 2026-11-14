//! The market-to-limit law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_market_to_limit`] driven
//! through every fill lane of the engine, over the ONE law `vike_model::order_fill_price` states
//! (and `vike_fills::fill_model`'s tick and L2 tiers carry).
//!
//! A market-to-limit fills as a market order does on its first event, a TAKER. What the lane
//! leaves of it rests, in place, as the plain `Limit` at the price that first fill booked at, and
//! fills by the limit law on a later event as a MAKER; when nothing filled yet it waits as it was.
//! On bars the one thing that leaves a remainder is the volume cap (`StrategyEngineConfig::volume_limit`):
//! with no cap a bar fills it whole, so there it IS a market order. Pinned here:
//!
//! 1. no cap: the market order's fill, to the bit, on bars, sub-bars and the tick lane;
//! 2. the volume cap: the remainder rests as the `Limit` at the first fill's booked price (a fixed
//!    slippage included) and is not dropped as `volume_cap`, on the coarse, granular and cash-gated
//!    lanes; a bar that fills none of it leaves it waiting as the market-to-limit;
//! 3. the L2 lane: the best opposite level only, at its price, and a level that cannot cover it
//!    rests the whole order as the `Limit` there;
//! 4. the SL/TP cap: a market-to-limit exit cut by the cap in a bar with a stop fills what the cap
//!    leaves, exactly as a market order does there, and nothing rests (the paper book's answer);
//! 5. the pre-trade gate judges it as the unpriced taker live sends.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_marketdata::Bar;
use vike_model::{Fill, OrderKind, RiskLimits, Strategy};
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
};

fn mtl(side: i32, size: f64) -> Once {
    Once::new(move |b| b.submit_market_to_limit(SYM, side, size, 0.0, true))
}

fn market(side: i32, size: f64) -> Once {
    Once::new(move |b| b.submit(SYM, side, size, 0.0, true, None))
}

/// One unit per bar of 100: the volume cap these tapes run under.
fn capped() -> StrategyEngineConfig {
    StrategyEngineConfig { volume_limit: Some(0.01), ..params() }
}

/// The reasons in the run's `dropped` list.
fn dropped(engine: &StrategyEngine<Once>) -> Vec<String> {
    engine.core.dropped.iter().map(|d| d.reason.clone()).collect()
}

// ---- 1. no cap: a market order ----

/// With no cap every lane fills a market-to-limit whole on its first event, at the market
/// order's price and fee, so the two runs' fills are identical to the bit.
#[test]
fn with_no_cap_it_fills_exactly_as_a_market_order_does() {
    let bars = || vec![bar(1_000, FLAT), bar(2_000, [101.0, 102.0, 99.0, 100.5]), bar(3_000, FLAT)];
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 101.0, 101.5, 100.5, 101.2, 50.0),
        vike_simulator::test_support::bar(2_500, 101.2, 102.0, 99.0, 100.5, 50.0),
    ];
    let granular = || StrategyEngineConfig {
        granular_by_symbol: vec![(SYM.into(), subs.clone())],
        ..params()
    };
    let ticks = || vec![q(1, 99.0, 100.0), q(2, 100.0, 101.0)];
    for side in [1, -1] {
        let a = run_bars(params(), bars(), mtl(side, 2.0));
        let b = run_bars(params(), bars(), market(side, 2.0));
        assert_eq!(a.strategy.fills.len(), 1, "{:?}", a.strategy.fills);
        assert_fill("the open, a taker", &a.strategy.fills[0], 2_000, 101.0, false);
        assert_eq!(a.strategy.fills, b.strategy.fills, "side {side}: bars");
        assert!(resting(&a).is_empty());

        let a = run_bars(granular(), bars(), mtl(side, 2.0));
        let b = run_bars(granular(), bars(), market(side, 2.0));
        assert_fill("the first sub-bar's open", &a.strategy.fills[0], 2_000, 101.0, false);
        assert_eq!(a.strategy.fills, b.strategy.fills, "side {side}: the first sub-bar");

        let a = run_ticks(FillModelKind::Tick, ticks(), mtl(side, 2.0));
        let b = run_ticks(FillModelKind::Tick, ticks(), market(side, 2.0));
        assert_eq!(a.strategy.fills.len(), 1, "{:?}", a.strategy.fills);
        assert_eq!(a.strategy.fills, b.strategy.fills, "side {side}: the quote it crosses");
    }
}

// ---- 2. the volume cap ----

/// Bar 1 lets one of two units fill at its open, a taker; the other rests as the `Limit` at 101,
/// which bar 2 does not reach and bar 3 fills, a maker. A market order of two loses its second
/// unit to `volume_cap` instead.
#[test]
fn the_capped_remainder_rests_as_the_limit_at_the_first_fill() {
    let tape = |upto: usize| {
        let bars = vec![
            bar(1_000, FLAT),
            bar(2_000, [101.0, 102.0, 100.0, 101.5]),
            bar(3_000, [102.0, 103.0, 101.5, 102.5]),
            bar(4_000, [101.5, 102.0, 100.5, 101.0]),
        ];
        bars.into_iter().take(upto).collect::<Vec<Bar>>()
    };
    let after_first = run_bars(capped(), tape(3), mtl(1, 2.0));
    assert_eq!(after_first.strategy.fills.len(), 1, "{:?}", after_first.strategy.fills);
    assert_fill("one unit at the open", &after_first.strategy.fills[0], 2_000, 101.0, false);
    assert_eq!(after_first.strategy.fills[0].size, 1.0);
    assert_eq!(resting(&after_first), vec![(OrderKind::Limit, Some(101.0), None)]);
    assert!(dropped(&after_first).is_empty(), "the remainder is not dropped");

    let full = run_bars(capped(), tape(4), mtl(1, 2.0));
    assert_eq!(full.strategy.fills.len(), 2, "{:?}", full.strategy.fills);
    assert_fill("the Limit at the first fill's price", &full.strategy.fills[1], 4_000, 101.0, true);
    assert_eq!(full.core.position_of(SYM).size, 2.0);
    assert!(resting(&full).is_empty());

    let plain = run_bars(capped(), tape(4), market(1, 2.0));
    assert_eq!(plain.strategy.fills.len(), 1, "{:?}", plain.strategy.fills);
    assert_eq!(dropped(&plain), ["volume_cap"], "a market order loses it");
}

/// The limit is the price the first fill BOOKED at: with a fixed slippage the taker fill pays it
/// (a market-to-limit is not limit-bounded) and the `Limit` rests there, then fills paying none.
#[test]
fn the_limit_is_the_booked_price_of_the_first_fill() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [101.0, 102.0, 100.0, 101.5]),
        bar(3_000, [101.0, 101.8, 100.5, 101.2]),
    ];
    let engine =
        run_bars(StrategyEngineConfig { slippage_amount: AMOUNT, ..capped() }, bars, mtl(1, 2.0));
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_fill("the open plus the amount", &fills[0], 2_000, 101.0 + AMOUNT, false);
    assert_fill("the Limit's gap improvement, no amount", &fills[1], 3_000, 101.0, true);
}

/// A bar the cap lets nothing through (no volume) fills none of it: it waits as the market-to-limit
/// and fills on the next bar that trades. A market order is dropped whole there.
#[test]
fn a_bar_that_fills_none_of_it_leaves_it_waiting_as_it_was() {
    let quiet = vike_simulator::test_support::bar(2_000, 101.0, 102.0, 100.0, 101.5, 0.0);
    let tape = |upto: usize| {
        let bars = vec![bar(1_000, FLAT), quiet.clone(), bar(3_000, [102.0, 103.0, 101.0, 102.5])];
        bars.into_iter().take(upto).collect::<Vec<Bar>>()
    };
    let waiting = run_bars(capped(), tape(2), mtl(1, 1.0));
    assert!(waiting.strategy.fills.is_empty(), "{:?}", waiting.strategy.fills);
    assert_eq!(resting(&waiting), vec![(OrderKind::MarketToLimit, None, None)]);

    let filled = run_bars(capped(), tape(3), mtl(1, 1.0));
    assert_eq!(filled.strategy.fills.len(), 1, "{:?}", filled.strategy.fills);
    assert_fill("the next bar's open", &filled.strategy.fills[0], 3_000, 102.0, false);

    let plain = run_bars(capped(), tape(3), market(1, 1.0));
    assert!(plain.strategy.fills.is_empty(), "{:?}", plain.strategy.fills);
    assert_eq!(dropped(&plain), ["volume_cap"]);
}

/// The granular lane caps per sub-bar: the first sub-bar fills one unit at its open, and the
/// `Limit` the remainder became fills on a LATER sub-bar of the same coarse bar, a maker.
#[test]
fn on_sub_bars_the_remainder_fills_on_a_later_sub_bar() {
    let bars = vec![bar(1_000, FLAT), bar(2_000, [101.0, 102.0, 100.0, 101.0]), bar(3_000, FLAT)];
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 101.0, 101.8, 101.0, 101.5, 100.0),
        vike_simulator::test_support::bar(2_500, 101.5, 101.6, 100.0, 101.0, 100.0),
    ];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..capped() };
    let engine = run_bars(p, bars, mtl(1, 2.0));
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_fill("the first sub-bar's open", &fills[0], 2_000, 101.0, false);
    assert_fill("the Limit on the next sub-bar", &fills[1], 2_500, 101.0, true);
}

/// The cash-gated lane (`StrategyEngineConfig::cash_gate`) applies the same remainder law, on its opens.
#[test]
fn the_cash_gated_lane_rests_the_remainder_too() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [101.0, 102.0, 100.0, 101.5]),
        bar(3_000, [101.5, 102.0, 100.5, 101.0]),
    ];
    let engine = run_bars(StrategyEngineConfig { cash_gate: true, ..capped() }, bars, mtl(1, 2.0));
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_fill("one unit at the open", &fills[0], 2_000, 101.0, false);
    assert_fill("the Limit", &fills[1], 3_000, 101.0, true);
    assert!(dropped(&engine).is_empty(), "{:?}", engine.core.dropped);
}

// ---- 3. the L2 lane ----

/// The tape: a book of asks 1 at 101 and 5 at 102, then a quote, then a book whose best ask is 3
/// at 101. Two units cannot be covered by the best level: the WHOLE order rests as the `Limit` at
/// 101 and fills when the book later shows 101 deep enough, a maker. One unit fills at 101 at
/// once. `upto` cuts the tape.
fn l2_run(size: f64, upto: usize) -> StrategyEngine<Once> {
    let tape = vec![
        book(1, &[(99.0, 5.0)], &[(101.0, 1.0), (102.0, 5.0)]),
        q(2, 99.0, 101.0),
        q(3, 99.0, 101.0),
        book(4, &[(99.0, 5.0)], &[(101.0, 3.0)]),
        q(5, 99.0, 101.0),
    ];
    run_ticks(FillModelKind::L2Book, tape.into_iter().take(upto).collect(), mtl(1, size))
}

#[test]
fn the_l2_lane_takes_the_best_level_only_or_rests_whole_as_its_limit() {
    let fits = l2_run(1.0, 3);
    assert_eq!(fits.strategy.fills.len(), 1, "{:?}", fits.strategy.fills);
    assert_fill("the best ask", &fits.strategy.fills[0], 3, 101.0, false);

    let deep = l2_run(2.0, 3);
    assert!(deep.strategy.fills.is_empty(), "never the second level: {:?}", deep.strategy.fills);
    assert_eq!(resting(&deep), vec![(OrderKind::Limit, Some(101.0), None)]);

    let later = l2_run(2.0, 5);
    assert_eq!(later.strategy.fills.len(), 1, "{:?}", later.strategy.fills);
    assert_fill("the Limit, once 101 covers it", &later.strategy.fills[0], 5, 101.0, true);
}

// ---- 4. the SL/TP cap ----

/// Bar `i` runs step `i`'s submits.
struct Steps {
    steps: Vec<fn(&mut OrderBrokerSimulator)>,
    at: usize,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Steps {
    fn on_bar(&mut self, ctx: &mut OrderBrokerSimulator, _bar: &Bar) {
        if let Some(step) = self.steps.get(self.at) {
            step(ctx);
        }
        self.at += 1;
    }
    fn on_fill(&mut self, _context: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

/// Long 3 from bar 1 with a stop selling 2 at 95; on bar 1 the exit `exit` sells 2 more. Bar 2
/// reaches the stop and opens the exit: the stop is adverse and fills first, and the cap cuts the
/// exit to the one unit left. A market-to-limit there fills that unit at the open as a market
/// order does, and the unit the cap cut does NOT rest: the cap is not the lane's liquidity, and the
/// paper book, which fills whole, leaves a cut market-to-limit as a market order too.
#[test]
fn the_stop_loss_take_profit_cap_cuts_it_as_a_market_order_and_nothing_rests() {
    let run = |exit: fn(&mut OrderBrokerSimulator)| {
        let entry: fn(&mut OrderBrokerSimulator) = |b| {
            b.submit(SYM, 1, 3.0, 0.0, true, None);
            b.submit_stop(SYM, -1, 2.0, 95.0, 0.0, true);
        };
        let steps = vec![entry, exit];
        let bars = vec![
            bar(1_000, FLAT),
            bar(2_000, FLAT),
            bar(3_000, [100.0, 101.0, 90.0, 95.0]),
            bar(4_000, FLAT),
        ];
        let steps = Steps { steps, at: 0, fills: Vec::new() };
        let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], steps, params());
        engine.run();
        engine
    };
    let cut = run(|b| b.submit_market_to_limit(SYM, -1, 2.0, 0.0, true));
    let fills = &cut.strategy.fills;
    assert_eq!(fills.len(), 3, "{fills:?}");
    assert_fill("the stop, first", &fills[1], 3_000, 95.0, false);
    assert_fill("the exit, the unit the cap leaves", &fills[2], 3_000, 100.0, false);
    assert_eq!((fills[1].size, fills[2].size), (2.0, 1.0));
    assert_eq!(cut.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(cut.core.pending_of(SYM).is_empty(), "the cut unit does not rest");
    assert_eq!(cut.core.intrabar_both_hit, 1);

    let as_market = run(|b| b.submit(SYM, -1, 2.0, 0.0, true, None));
    assert_eq!(cut.strategy.fills, as_market.strategy.fills, "a market order's answer");
}

// ---- 5. the intake ----

/// The pre-trade gate judges the unpriced taker live sends, on the mark: two units at 100 break a
/// 150 notional cap, one passes.
#[test]
fn the_pre_trade_gate_judges_it_as_an_unpriced_taker() {
    let run = |size: f64| {
        let cap = RiskLimits { max_notional_per_order: Some(150.0), ..RiskLimits::new() };
        let p = StrategyEngineConfig {
            default_venue: Some("TEST".into()),
            risk_limits: Some(cap),
            ..params()
        };
        run_bars(p, vec![bar(1_000, FLAT), bar(2_000, FLAT)], mtl(1, size))
    };
    let refused = run(2.0);
    assert!(refused.strategy.fills.is_empty());
    let reasons: Vec<_> = refused.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["over-max-notional"]);
    assert_eq!(run(1.0).strategy.fills.len(), 1);
}
