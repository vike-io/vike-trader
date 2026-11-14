//! The adjustable-stop law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_stop_adjustable`] (IB's
//! adjustable stop; the auto trailing stop is its trailing target) driven through the engine's fill
//! lanes, over the ONE law `vike_model::order_fill_price` states (decision 0267, 2.10 and 2.11).
//!
//! Per event, a stop or stop-limit first takes its own law with its current terms (adverse first);
//! then, if it did not fire and the event reached the adjustment's level `at` (a sell stop's high at or
//! above it, a buy stop's low at or below), the order becomes its target IN PLACE, once: a stop moves,
//! a stop-limit takes its trigger and limit, a trailing stop takes its distance and the extreme whose
//! stop is the target's. The new terms are judged from the NEXT event: a bar has no time inside it.
//! Assumption: IB's "penetrated" is read as touched. Pinned here:
//!
//! 1. every target on coarse bars, from a stop and from a stop-limit, both sides, each fill a taker; a
//!    bar that fires the stop fires it as it stood; the bar that reaches `at` is not judged again;
//! 2. sub-bars: the adjustment on one sub-bar, its new terms filled on a later one of the same bar;
//! 3. the tick lane (the quote reaches the level) and the L2 lane (the book walk after the trigger);
//! 4. the opt-in stop-release emulator adjusts the stop once and judges the new terms from the next
//!    event, exactly as the plain lanes do;
//! 5. the intake: the doors' rules refuse in their own words.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_model::{OrderDrop, OrderKind, OrderStopAdjust, OrderStopAdjustTo, OrderType};
use vike_simulator::{FillModelKind, StrategyEngineConfig};

use crate::stop_limit_law::{
    FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
};

fn adjustable(
    side: i32,
    size: f64,
    stop: f64,
    limit: Option<f64>,
    at: f64,
    to: OrderStopAdjustTo,
) -> Once {
    let adjust = OrderStopAdjust { at, to };
    Once::new(move |b| b.submit_stop_adjustable(SYM, side, size, stop, limit, adjust, 0.0, true))
}

// ---- 1. the law on coarse bars ----

/// Submitted on bar 0 (the mark 100): bar 1 neither fires a sell stop at 95 nor reaches 105, bar 2
/// reaches 105, bar 3 falls to 97.
const QUIET: [f64; 4] = [101.0, 104.0, 99.0, 103.0];
const REACH: [f64; 4] = [104.0, 106.0, 103.0, 105.0];
const FALL: [f64; 4] = [104.0, 104.5, 97.0, 98.0];

fn sell_tape(upto: usize) -> Vec<vike_marketdata::Bar> {
    let bars = vec![bar(1_000, FLAT), bar(2_000, QUIET), bar(3_000, REACH), bar(4_000, FALL)];
    bars.into_iter().take(upto).collect()
}

/// A sell stop at 95 (a stop-limit at 95/94 too), adjusted at 105. Bar 2 makes it its target and fills
/// nothing; bar 3 fills every target at its new terms, a taker, where the stop at 95 never fills. A
/// percent trailing target starts from the extreme whose stop is its own (98 / (1 − 2 %)).
#[test]
fn every_target_takes_the_stop_over_in_place_once() {
    let percent_extreme: f64 = 98.0 / (1.0 - 2.0 / 100.0);
    let percent_stop = percent_extreme - percent_extreme * 2.0 / 100.0;
    let targets = [
        (
            "a stop",
            OrderStopAdjustTo::Stop { stop: 100.0 },
            (OrderKind::Stop, Some(100.0), None),
            100.0,
        ),
        (
            "a stop-limit",
            OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 },
            (OrderKind::StopLimit, Some(100.0), Some(99.0)),
            100.0,
        ),
        (
            "a trailing stop",
            OrderStopAdjustTo::Trailing { stop: 100.0, trail: Some(3.0), trail_percent: None },
            (OrderKind::Trailing, None, None),
            100.0,
        ),
        (
            "a percent trailing stop",
            OrderStopAdjustTo::Trailing { stop: 98.0, trail: None, trail_percent: Some(2.0) },
            (OrderKind::Trailing, None, None),
            percent_stop,
        ),
    ];
    for limit in [None, Some(94.0)] {
        for (case, to, after, px) in targets {
            let case = format!("{case} from {limit:?}");
            let adjusted =
                run_bars(params(), sell_tape(3), adjustable(-1, 1.0, 95.0, limit, 105.0, to));
            assert!(adjusted.strategy.fills.is_empty(), "{case}: {:?}", adjusted.strategy.fills);
            assert_eq!(resting(&adjusted), vec![after], "{case}: the target's terms");
            assert!(adjusted.core.pending_of(SYM)[0].adjust.is_none(), "{case}: once");

            let filled =
                run_bars(params(), sell_tape(4), adjustable(-1, 1.0, 95.0, limit, 105.0, to));
            assert_eq!(filled.strategy.fills.len(), 1, "{case}: {:?}", filled.strategy.fills);
            assert_eq!(filled.strategy.fills[0].price.to_bits(), px.to_bits(), "{case}");
            assert_fill(&case, &filled.strategy.fills[0], 4_000, px, false);
        }
    }
    let plain = run_bars(
        params(),
        sell_tape(4),
        Once::new(|b| b.submit_stop(SYM, -1, 1.0, 95.0, 0.0, true)),
    );
    assert!(
        plain.strategy.fills.is_empty(),
        "the stop at 95 never fills: {:?}",
        plain.strategy.fills
    );
}

/// A buy stop at 105 adjusted at 95 mirrors it: bar 2's low reaches 95, and bar 3 rises through the
/// new stop at 100 (a stop target, and a trailing one whose extreme is 100 − 3).
#[test]
fn a_buy_stop_mirrors_it() {
    let bars = || {
        vec![
            bar(1_000, FLAT),
            bar(2_000, [99.0, 101.0, 96.0, 97.0]),
            bar(3_000, [96.0, 97.0, 94.0, 95.0]),
            bar(4_000, [96.0, 101.0, 95.5, 100.0]),
        ]
    };
    for to in [
        OrderStopAdjustTo::Stop { stop: 100.0 },
        OrderStopAdjustTo::Trailing { stop: 100.0, trail: Some(3.0), trail_percent: None },
    ] {
        let engine = run_bars(params(), bars(), adjustable(1, 1.0, 105.0, None, 95.0, to));
        assert_eq!(engine.strategy.fills.len(), 1, "{to:?}: {:?}", engine.strategy.fills);
        assert_fill("the new stop at 100", &engine.strategy.fills[0], 4_000, 100.0, false);
    }
}

/// A bar that reaches the level AND fires the stop fires the stop as it stood (adverse first), the
/// adjustment never taken; a bar that reaches the level and then trades through the TARGET's stop is
/// not judged again: the target fills on the next bar.
#[test]
fn the_stop_fires_first_and_the_new_terms_wait_for_the_next_bar() {
    for limit in [None, Some(94.0)] {
        let both = vec![bar(1_000, FLAT), bar(2_000, [100.0, 106.0, 93.0, 94.0])];
        let to = OrderStopAdjustTo::Stop { stop: 100.0 };
        let engine = run_bars(params(), both, adjustable(-1, 1.0, 95.0, limit, 105.0, to));
        assert_eq!(engine.strategy.fills.len(), 1, "{limit:?}: {:?}", engine.strategy.fills);
        assert_fill("the stop at 95, as it stood", &engine.strategy.fills[0], 2_000, 95.0, false);
    }
    let through = vec![bar(1_000, FLAT), bar(2_000, [104.0, 106.0, 99.0, 100.0]), bar(3_000, FLAT)];
    let engine = run_bars(
        params(),
        through,
        adjustable(-1, 1.0, 95.0, None, 105.0, OrderStopAdjustTo::Stop { stop: 100.0 }),
    );
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the next bar", &engine.strategy.fills[0], 3_000, 100.0, false);
}

// ---- 2. sub-bars ----

/// The coarse bar `[104, 106, 99, 100]` only adjusts the stop. Its sub-bars say it reached 105 first and
/// fell through 100 after: the second sub-bar fills the new stop, inside the same coarse bar.
#[test]
fn on_sub_bars_the_new_terms_fill_on_a_later_sub_bar() {
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 104.0, 106.0, 104.0, 105.5, 50.0),
        vike_simulator::test_support::bar(2_500, 105.5, 105.5, 99.0, 100.0, 50.0),
    ];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    let bars = vec![bar(1_000, FLAT), bar(2_000, [104.0, 106.0, 99.0, 100.0])];
    let engine = run_bars(
        p,
        bars,
        adjustable(-1, 1.0, 95.0, None, 105.0, OrderStopAdjustTo::Stop { stop: 100.0 }),
    );
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the second sub-bar", &engine.strategy.fills[0], 2_500, 100.0, false);
}

// ---- 3. the tick lane and the L2 lane ----

/// On the quote a sell stop is adjusted by a BID at its level (105) and fires on a bid at or under the
/// new stop (100), at that bid. On L2 the same trigger walks the book: two units take 99 and 98.
#[test]
fn the_tick_and_l2_lanes_adjust_on_the_quote() {
    let to = OrderStopAdjustTo::Stop { stop: 100.0 };
    let quotes =
        vec![q(1, 100.0, 101.0), q(2, 105.0, 106.0), q(3, 100.5, 101.5), q(4, 99.5, 100.5)];
    let engine = run_ticks(FillModelKind::Tick, quotes, adjustable(-1, 1.0, 95.0, None, 105.0, to));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the bid under the new stop", &engine.strategy.fills[0], 4, 99.5, false);

    let tape = vec![
        book(1, &[(100.0, 5.0)], &[(101.0, 5.0)]),
        q(2, 100.0, 101.0),
        q(3, 105.0, 106.0),
        book(4, &[(99.0, 1.0), (98.0, 5.0)], &[(100.0, 5.0)]),
        q(5, 99.0, 100.0),
    ];
    let engine = run_ticks(FillModelKind::L2Book, tape, adjustable(-1, 2.0, 95.0, None, 105.0, to));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the walk", &engine.strategy.fills[0], 5, 98.5, false);
}

// ---- 4. the stop-release emulator ----

/// With the emulator's stop release on, bar 1 reaches 105 and trades through 100: the stop is adjusted
/// once and its new terms are not judged on that bar. Bar 2 trades through 100 and releases it as a
/// market child, which fills at bar 3's open.
#[test]
fn the_stop_release_emulator_judges_the_new_terms_from_the_next_bar() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [104.0, 106.0, 99.0, 100.0]),
        bar(3_000, [100.0, 100.0, 99.0, 99.5]),
        bar(4_000, [98.0, 99.0, 97.0, 98.0]),
    ];
    let p = StrategyEngineConfig { emulator_release_stops: true, ..params() };
    let engine = run_bars(
        p,
        bars,
        adjustable(-1, 1.0, 95.0, None, 105.0, OrderStopAdjustTo::Stop { stop: 100.0 }),
    );
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the market child at bar 3's open", &engine.strategy.fills[0], 4_000, 98.0, false);
}

// ---- 5. the intake ----

/// Terms `vike_model::stop_adjust_refusal` refuses never rest: the verb drops them in that rule's own words.
#[test]
fn adjustment_terms_the_rule_refuses_are_dropped_in_its_words() {
    let stop = |stop| OrderStopAdjustTo::Stop { stop };
    let cases = [
        ("a sell stop's level below it", -1, 95.0, None, 90.0, stop(80.0)),
        ("a buy stop's level above it", 1, 105.0, None, 110.0, stop(120.0)),
        ("a level that is not a number", -1, 95.0, None, f64::NAN, stop(100.0)),
        ("a target past its level: it would fire at once", -1, 95.0, None, 105.0, stop(106.0)),
        (
            "a stop-limit target with no limit above zero",
            -1,
            95.0,
            Some(94.0),
            105.0,
            OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 0.0 },
        ),
        (
            "a trailing target with no distance",
            -1,
            95.0,
            None,
            105.0,
            OrderStopAdjustTo::Trailing { stop: 100.0, trail: None, trail_percent: None },
        ),
        (
            "a trailing target with both distances",
            -1,
            95.0,
            None,
            105.0,
            OrderStopAdjustTo::Trailing { stop: 100.0, trail: Some(1.0), trail_percent: Some(1.0) },
        ),
    ];
    for (case, side, stop, limit, at, to) in cases {
        let order_type = if limit.is_some() { OrderType::StopLimit } else { OrderType::Stop };
        let why = vike_model::stop_adjust_refusal(
            &order_type,
            side,
            Some(stop),
            Some(&OrderStopAdjust { at, to }),
        );
        let engine = run_bars(
            params(),
            vec![bar(1_000, FLAT), bar(2_000, FLAT)],
            adjustable(side, 1.0, stop, limit, at, to),
        );
        assert!(resting(&engine).is_empty(), "{case}");
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop {
                symbol: SYM.to_string(),
                reason: why.expect(case),
                size: 1.0,
                weight: 0.0
            }],
            "{case}"
        );
    }
}
