//! The if-touched law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_take_profit`] driven through
//! every fill lane of the engine, over the ONE law `vike_model::order_fill_price` states.
//!
//! A take-profit is touched from the FAVOURABLE side: a buy at or below its trigger T, a sell at or
//! above. A is the activation price, T or the open when the bar gapped past T (a buy's
//! `min(T, open)`: a gap down is an improvement). With no limit it fills at A as a TAKER
//! (market-if-touched); with a limit L it takes the stop-limit's limit part (limit-if-touched): the
//! better of L and A when the touch reaches L, else it rests, in place, as the `Limit` at L and
//! fills later as a MAKER. Pinned here:
//!
//! 1. every case of the law on coarse bars, each fill with its fee class;
//! 2. granular sub-bars: a limit-if-touched converted on one sub-bar fills on the next;
//! 3. the tick lane (the touch on the quote) and the L2 lane. ⚠ On the L2 lane a market-if-touched
//!    the book cannot cover STAYS ARMED as a take-profit, as a stop does there: it fills on a later
//!    touch with depth, and NOT when the price has moved back away, though the touch that armed it
//!    has passed (`vike_fills::fill_model`'s `FillModelL2Book`; pinned, not judged here);
//! 4. the SL/TP cap: a take-profit is FAVOURABLE, so a stop-limit exit in the same bar fills first;
//! 5. the implicit bracket: a protective stop that flattens the position cancels its closing-side
//!    take-profit (`vike_execution::is_protective_exit_sibling`), as it cancels a closing limit;
//! 6. the session deferral, and the pre-trade gate judging a market-if-touched on its trigger.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_marketdata::Bar;
use vike_model::{OrderKind, RiskLimits};
use vike_simulator::test_support::ms;
use vike_simulator::{FillModelKind, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
    session_params, weekend_tape,
};

fn take_profit(side: i32, size: f64, trigger: f64, limit: Option<f64>) -> Once {
    Once::new(move |b| b.submit_take_profit(SYM, side, size, trigger, limit, 0.0, true))
}

// ---- 1. the law on coarse bars, with the fee class ----

/// (case, side, T, L, touch bar, next bar, fill `(ts, price, maker)`, resting `(kind, price)`)
type Row = (
    &'static str,
    i32,
    f64,
    Option<f64>,
    [f64; 4],
    [f64; 4],
    Option<(i64, f64, bool)>,
    Option<(OrderKind, f64)>,
);

/// Submitted on bar 0 (ts 1 000, at 100), so bar 1 (ts 2 000) is the first that can touch it. No
/// row is a measurement: each is the law's own case.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    ("mit buy, touched: fills at T", 1, 99.0, None, [101.0, 102.0, 98.0, 100.0], FLAT, Some((2_000, 99.0, false)), None),
    ("mit buy, gap down past T: A = open, an improvement", 1, 99.0, None, [97.0, 98.0, 96.0, 97.0], FLAT, Some((2_000, 97.0, false)), None),
    ("mit buy, not touched: still armed", 1, 99.0, None, [101.0, 102.0, 99.5, 100.0], [100.5, 101.0, 99.2, 100.0], None, Some((OrderKind::TakeProfit, 99.0))),
    ("mit sell, touched: fills at T", -1, 101.0, None, [99.0, 102.0, 98.0, 100.0], FLAT, Some((2_000, 101.0, false)), None),
    ("mit sell, gap up past T: A = open", -1, 101.0, None, [103.0, 104.0, 102.0, 103.0], FLAT, Some((2_000, 103.0, false)), None),
    ("lit buy, L below T reached", 1, 99.0, Some(98.5), [101.0, 102.0, 98.0, 100.0], FLAT, Some((2_000, 98.5, false)), None),
    ("lit buy, L above T: A = T", 1, 99.0, Some(100.0), [101.0, 102.0, 98.0, 100.0], FLAT, Some((2_000, 99.0, false)), None),
    ("lit buy, L not reached: rests as the Limit, a maker next bar", 1, 99.0, Some(98.0), [101.0, 102.0, 98.5, 100.0], [99.0, 99.5, 97.5, 98.0], Some((3_000, 98.0, true)), None),
    ("lit sell, gap up past T and L: A = open", -1, 101.0, Some(101.5), [103.0, 104.0, 102.0, 103.0], FLAT, Some((2_000, 103.0, false)), None),
    ("lit sell, L not reached: rests as the Limit, a maker next bar", -1, 101.0, Some(103.0), [100.0, 102.0, 99.0, 101.0], [102.0, 103.5, 101.5, 103.0], Some((3_000, 103.0, true)), None),
    ("lit sell, L never reached: rests as the Limit at L", -1, 101.0, Some(103.0), [100.0, 102.0, 99.0, 101.0], [101.0, 102.5, 100.5, 102.0], None, Some((OrderKind::Limit, 103.0))),
];

#[test]
fn the_if_touched_law_on_bars_with_its_fee_class() {
    for &(case, side, trigger, limit, b1, b2, fill, rest) in ROWS {
        let bars = vec![bar(1_000, FLAT), bar(2_000, b1), bar(3_000, b2)];
        let engine = run_bars(params(), bars, take_profit(side, 1.0, trigger, limit));
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
        let want: Vec<_> = rest
            .map(|(kind, px)| (kind, Some(px), if kind == OrderKind::Limit { None } else { limit }))
            .into_iter()
            .collect();
        assert_eq!(resting(&engine), want, "{case}: what rests");
    }
}

/// **A limit-if-touched pays no fixed slippage**, on its touch or as the `Limit` it rests as: its
/// limit bounds it (`vike_model::OrderResting::is_limit_bounded`). A market-if-touched has no limit
/// and pays it against the trader, as a market fill does.
#[test]
fn a_limit_if_touched_pays_no_fixed_slippage_and_a_market_if_touched_does() {
    let amount = || StrategyEngineConfig { slippage_amount: AMOUNT, ..params() };
    for &(case, side, trigger, limit, b1, b2, fill, _) in ROWS {
        let bars = vec![bar(1_000, FLAT), bar(2_000, b1), bar(3_000, b2)];
        let engine = run_bars(amount(), bars, take_profit(side, 1.0, trigger, limit));
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                let paid = if limit.is_some() { px } else { px + side as f64 * AMOUNT };
                assert_fill(case, &fills[0], ts, paid, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
    }
}

// ---- 2. granular sub-bars ----

/// A limit-if-touched buy (T 99, L 98). The coarse bar 1 reaches L on its range and fills as a
/// taker; its sub-bars touch T without reaching L first, so the order rests as the `Limit`, and
/// the next sub-bar fills that `Limit` as a maker.
#[test]
fn granular_sub_bars_keep_a_limit_if_touched_conversion() {
    let run = |subs: Vec<Bar>| {
        let bars = vec![
            bar(1_000, FLAT),
            bar(2_000, [100.0, 100.5, 97.5, 98.2]),
            bar(3_000, [98.2, 98.5, 98.1, 98.3]),
        ];
        let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
        run_bars(p, bars, take_profit(1, 1.0, 99.0, Some(98.0)))
    };
    let coarse = run(Vec::new());
    assert_eq!(coarse.strategy.fills.len(), 1, "{:?}", coarse.strategy.fills);
    assert_fill("coarse: the range rule", &coarse.strategy.fills[0], 2_000, 98.0, false);

    let touch = bar(2_000, [100.0, 100.5, 98.5, 98.6]); // touches 99, stops short of 98
    let reach = bar(2_500, [98.6, 98.8, 97.5, 98.2]); // the Limit at 98 fills
    let walked = run(vec![touch, reach]);
    assert_eq!(walked.strategy.fills.len(), 1, "{:?}", walked.strategy.fills);
    assert_fill("granular: the Limit fills later", &walked.strategy.fills[0], 2_500, 98.0, true);
}

// ---- 3. the tick lane and the L2 lane ----

/// The quote law: a buy is touched by an ask at or below T, a sell by a bid at or above it. A
/// market-if-touched fills at that quote; a limit-if-touched whose quote is past L rests as the
/// `Limit` and fills on a later quote within it, as a maker.
#[test]
fn the_tick_lane_touches_on_the_quote() {
    let cases = [
        (
            "mit buy, the ask comes down to T",
            take_profit(1, 1.0, 100.5, None),
            vec![q(1, 101.0, 102.0), q(2, 100.5, 101.0), q(3, 99.5, 100.5)],
            (3, 100.5, false),
        ),
        (
            "lit sell, the touching bid short of L: the Limit fills later",
            take_profit(-1, 1.0, 101.0, Some(101.5)),
            vec![q(1, 100.0, 101.0), q(2, 101.2, 102.2), q(3, 101.5, 102.5)],
            (3, 101.5, true),
        ),
    ];
    for (case, strategy, ticks, (ts, px, maker)) in cases {
        let engine = run_ticks(FillModelKind::Tick, ticks, strategy);
        assert_eq!(engine.strategy.fills.len(), 1, "{case}: {:?}", engine.strategy.fills);
        assert_fill(case, &engine.strategy.fills[0], ts, px, maker);
    }
}

/// A limit-if-touched buy (T 100.5, L 100, three units): the quote at ts 4 touches it with only two
/// units at L, so it rests as the `Limit` at L; the book at ts 5 fills that `Limit` as a maker.
#[test]
fn the_l2_lane_rests_a_limit_if_touched_the_book_cannot_cover() {
    let run = |upto: usize| {
        let tape = vec![
            book(1, &[(99.0, 5.0)], &[(101.0, 5.0)]),
            q(2, 99.0, 101.0),
            book(3, &[(99.0, 5.0)], &[(100.0, 2.0), (101.0, 5.0)]),
            q(4, 99.0, 100.0),
            book(5, &[(99.0, 5.0)], &[(100.0, 5.0)]),
            q(6, 99.0, 100.0),
        ];
        let tape = tape.into_iter().take(upto).collect();
        run_ticks(FillModelKind::L2Book, tape, take_profit(1, 3.0, 100.5, Some(100.0)))
    };
    let short = run(4);
    assert!(short.strategy.fills.is_empty(), "{:?}", short.strategy.fills);
    assert_eq!(resting(&short), vec![(OrderKind::Limit, Some(100.0), None)], "rests as the Limit");

    let later = run(6);
    assert_eq!(later.strategy.fills.len(), 1, "{:?}", later.strategy.fills);
    assert_fill("the Limit's walk", &later.strategy.fills[0], 6, 100.0, true);
}

/// ⚠ THE PIN (step 0's choice, not a measurement). A market-if-touched buy (T 100.5, three units)
/// is touched at ts 4 with two units on the book: it is not filled and STAYS ARMED as the
/// take-profit, as a stop the book cannot cover does. At ts 6 the price is back above T: the book
/// holds the depth, but nothing touches the order, so it does not fill. At ts 8 it is touched again
/// with depth and fills as a taker.
#[test]
fn the_l2_lane_keeps_an_uncovered_market_if_touched_armed() {
    let run = |upto: usize| {
        let tape = vec![
            book(1, &[(99.0, 5.0)], &[(101.0, 5.0)]),
            q(2, 99.0, 101.0),
            book(3, &[(99.0, 5.0)], &[(100.0, 2.0)]),
            q(4, 99.0, 100.0),
            book(5, &[(100.0, 5.0)], &[(101.0, 5.0)]),
            q(6, 100.0, 101.0),
            book(7, &[(99.0, 5.0)], &[(100.0, 5.0)]),
            q(8, 99.0, 100.0),
        ];
        let tape = tape.into_iter().take(upto).collect();
        run_ticks(FillModelKind::L2Book, tape, take_profit(1, 3.0, 100.5, None))
    };
    let away = run(6);
    assert!(away.strategy.fills.is_empty(), "{:?}", away.strategy.fills);
    assert_eq!(resting(&away), vec![(OrderKind::TakeProfit, Some(100.5), None)], "still armed");

    let again = run(8);
    assert_eq!(again.strategy.fills.len(), 1, "{:?}", again.strategy.fills);
    assert_fill("the next touch with depth", &again.strategy.fills[0], 8, 100.0, false);
}

// ---- 4. the SL/TP cap, 5. the implicit bracket ----

/// Bars for the two bracket tests: the entry fills at bar 1's open (100); bar 2 spans 90 to 115.
fn bracket_bars() -> Vec<Bar> {
    vec![
        bar(1_000, FLAT),
        bar(2_000, [100.0, 101.0, 99.0, 100.0]),
        bar(3_000, [100.0, 115.0, 90.0, 100.0]),
        bar(4_000, FLAT),
    ]
}

/// Long 1, with a stop-limit sell (T 95, L 94) and a market-if-touched sell at 110 as its exits.
/// The take-profit is favourable, so the stop-limit fills first in the wide bar and the
/// take-profit is capped to the nothing left.
#[test]
fn a_take_profit_exit_is_capped_behind_a_stop_limit_in_one_bar() {
    let bracket = Once::new(|b| {
        b.submit(SYM, 1, 1.0, 0.0, true, None);
        b.submit_stop_limit(SYM, -1, 1.0, 95.0, 94.0, 0.0, true);
        b.submit_take_profit(SYM, -1, 1.0, 110.0, None, 0.0, true);
    });
    let engine = run_bars(params(), bracket_bars(), bracket);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "the entry and the stop-limit only: {fills:?}");
    assert_fill("the stop-limit exit", &fills[1], 3_000, 95.0, false);
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(engine.core.pending_of(SYM).is_empty(), "the capped take-profit left the book");
    assert_eq!(engine.core.intrabar_both_hit, 1);
}

/// The entry carries a protective stop at 95 (the sim's implicit bracket) and a market-if-touched
/// sell at 110 rests as its target. The wide bar fires the stop before the fill phase; the stop
/// flattens the position and cancels the take-profit, its kind-linked sibling. Before
/// `vike_execution::is_protective_exit_sibling` named `TakeProfit`, the same bar then filled it into a
/// spurious short.
#[test]
fn a_protective_stop_cancels_its_take_profit_sibling() {
    let bracket = Once::new(|b| {
        b.submit(SYM, 1, 1.0, 0.0, true, Some(95.0));
        b.submit_take_profit(SYM, -1, 1.0, 110.0, None, 0.0, true);
    });
    let engine = run_bars(params(), bracket_bars(), bracket);
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 2, "the entry and the protective stop only: {fills:?}");
    assert_fill("the protective stop", &fills[1], 3_000, 95.0, false);
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "flat, never reversed");
    assert!(engine.core.pending_of(SYM).is_empty(), "the take-profit was cancelled");
}

// ---- 6. the session deferral and the pre-trade gate ----

/// A market-if-touched sell at 125 is touched on the shut Saturday bar; the gate defers it to the
/// reopen, which gaps up to 150: an improvement, so it fills at that open. The gate off fills it on
/// the Saturday at that bar's open.
#[test]
fn a_shut_market_defers_a_take_profit_to_the_reopen() {
    let gated = run_bars(session_params(true), weekend_tape(), take_profit(-1, 1.0, 125.0, None));
    assert_eq!(gated.strategy.fills.len(), 1, "{:?}", gated.strategy.fills);
    assert_fill("the reopen", &gated.strategy.fills[0], ms(2024, 1, 7, 21), 150.0, false);
    assert_eq!(gated.core.session_deferrals, 3);

    let open = run_bars(session_params(false), weekend_tape(), take_profit(-1, 1.0, 125.0, None));
    assert_eq!(open.strategy.fills.len(), 1, "{:?}", open.strategy.fills);
    assert_fill("ungated: the Saturday", &open.strategy.fills[0], ms(2024, 1, 6, 12), 130.0, false);
}

/// A market-if-touched carries no limit, so the pre-trade gate judges it on its TRIGGER: one unit
/// at T 300 (a sell above the mark of 100) breaks a 200 notional cap the mark would pass.
#[test]
fn the_pre_trade_gate_judges_a_market_if_touched_on_its_trigger() {
    let run = |cap: Option<f64>| -> StrategyEngine<Once> {
        let p = StrategyEngineConfig {
            default_venue: Some("TEST".into()),
            risk_limits: cap
                .map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() }),
            ..params()
        };
        run_bars(p, vec![bar(1_000, FLAT), bar(2_000, FLAT)], take_profit(-1, 1.0, 300.0, None))
    };
    let refused = run(Some(200.0));
    let reasons: Vec<_> = refused.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["over-max-notional"]);
    assert!(refused.core.pending_of(SYM).is_empty());

    let rested = run(None);
    assert_eq!(resting(&rested), vec![(OrderKind::TakeProfit, Some(300.0), None)]);
}
