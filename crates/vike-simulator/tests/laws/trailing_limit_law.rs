//! The trailing-limit law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_trailing_limit`] driven
//! through the engine's fill lanes, over the ONE law `vike_model::order_fill_price` states.
//!
//! A trailing limit fires by the trailing law at the stop level P (`extreme - trail` for a sell,
//! `extreme + trail` for a buy), at the fire price A: P, or the open when a gap carried the price
//! past it. Its limit L is then P less the offset for a sell, P plus it for a buy, and it takes the
//! stop-limit's limit part: the better of L and A when the event reaches L, as a TAKER; else it
//! rests, in place, as the plain `Limit` at L and fills later as a MAKER (decision 0257). With an
//! activation it waits for that level first and is walked along the bar's path, as the activated
//! trailing stop is (`trailing_activation.rs`). Pinned here:
//!
//! 1. the law on coarse bars, both sides, each fill with its fee class: the activation not reached,
//!    the activation and the fire in one bar, a gap past P within L, a gap through L that comes back
//!    and one that never does, and an offset of 0 (the limit IS the stop); none pays fixed slippage;
//! 2. a fire INSIDE a bar that rests: on one coarse bar A is a price the path traded and L is no
//!    better than P, so only a gapped open can leave the order resting there. Sub-bars and ticks show
//!    a hole inside the bar, and there it rests as the `Limit` and fills later as a maker;
//! 3. no activation: the model's plain law, bit for bit;
//! 4. the intake: the order-terms rule refuses in its own words, and the pre-trade gate judges the
//!    request live sends, the activation its trigger price.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_marketdata::Bar;
use vike_model::{OrderDrop, OrderKind, OrderResting, OrderType, RiskLimits, order_fill_price};
use vike_simulator::{FillModelKind, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, params, q, resting, run_bars, run_ticks,
};

const TRAIL: f64 = 2.0;

fn trailing_limit(side: i32, offset: f64, activation: Option<f64>) -> Once {
    Once::new(move |b| {
        b.submit_trailing_limit(SYM, side, 1.0, TRAIL, offset, activation, 0.0, true);
    })
}

// ---- 1. the law on coarse bars, with the fee class ----

/// A sell waiting for 104: this bar activates it at the open and leaves the extreme at 106, so the
/// next bar's stop P is 104 (L 103 at an offset of 1), with nothing fired.
const ARM_SELL: [f64; 4] = [104.5, 106.0, 104.5, 105.5];
/// A buy waiting for 96: this bar activates it at the open and leaves the extreme at 94, so the
/// next bar's stop P is 96 (L 97 at an offset of 1), with nothing fired.
const ARM_BUY: [f64; 4] = [95.5, 95.5, 94.0, 94.5];

/// (case, side, offset, activation, bars 1 to 3, fill `(ts, price, maker)`, resting `(kind, price)`)
type Row = (
    &'static str,
    i32,
    f64,
    f64,
    [[f64; 4]; 3],
    Option<(i64, f64, bool)>,
    Option<(OrderKind, Option<f64>)>,
);

/// Trail 2. Submitted on bar 0 (ts 1 000, the mark 100), so bar 1 (ts 2 000) is the first that can
/// move it; the sell waits for 104, the buy for 96. Every row is the law's own consequence, not a
/// measurement: no trailing limit has a reference list.
#[rustfmt::skip]
const ROWS: &[Row] = &[
    // from the mark, a plain trailing limit would fire on this bar at 98
    ("sell, the activation not reached: nothing fires however far it falls", -1, 1.0, 104.0, [[100.0, 103.0, 90.0, 95.0], FLAT, FLAT], None, Some((OrderKind::TrailingLimit, None))),
    // 104.5 (active) -> 106 -> 100, through P 104: A = P, within L 103
    ("sell, activated and fired in one bar: a taker at P", -1, 1.0, 104.0, [[104.5, 106.0, 100.0, 101.0], FLAT, FLAT], Some((2_000, 104.0, false)), None),
    ("sell, a gap past P within L: A = the open", -1, 1.0, 104.0, [ARM_SELL, [103.5, 103.8, 101.0, 102.0], FLAT], Some((3_000, 103.5, false)), None),
    ("sell, a gap through L that comes back: filled at L", -1, 1.0, 104.0, [ARM_SELL, [102.0, 103.2, 100.0, 101.0], FLAT], Some((3_000, 103.0, false)), None),
    ("sell, a gap through L never back: rests as the Limit at L, a maker next bar", -1, 1.0, 104.0, [ARM_SELL, [102.0, 102.5, 100.0, 101.0], [101.0, 103.5, 100.5, 103.0]], Some((4_000, 103.0, true)), None),
    ("sell, offset 0, no gap: the limit is the stop, filled there", -1, 0.0, 104.0, [[104.5, 106.0, 100.0, 101.0], FLAT, FLAT], Some((2_000, 104.0, false)), None),
    // the open 103.5 that fills at an offset of 1 is under a limit at P itself
    ("sell, offset 0, a gap past P: rests as the Limit at P", -1, 0.0, 104.0, [ARM_SELL, [103.5, 103.8, 101.0, 102.0], FLAT], None, Some((OrderKind::Limit, Some(104.0)))),
    ("buy, the activation not reached: nothing fires however far it rises", 1, 1.0, 96.0, [[100.0, 110.0, 97.0, 105.0], FLAT, FLAT], None, Some((OrderKind::TrailingLimit, None))),
    // 95.5 (active) -> 94 -> 100, through P 96: A = P, within L 97
    ("buy, activated and fired in one bar: a taker at P", 1, 1.0, 96.0, [[95.5, 100.0, 94.0, 99.0], FLAT, FLAT], Some((2_000, 96.0, false)), None),
    ("buy, a gap past P within L: A = the open", 1, 1.0, 96.0, [ARM_BUY, [96.5, 99.0, 96.2, 98.0], FLAT], Some((3_000, 96.5, false)), None),
    ("buy, a gap through L that comes back: filled at L", 1, 1.0, 96.0, [ARM_BUY, [98.0, 99.0, 96.8, 98.5], FLAT], Some((3_000, 97.0, false)), None),
    ("buy, a gap through L never back: rests as the Limit at L, a maker next bar", 1, 1.0, 96.0, [ARM_BUY, [98.0, 99.0, 97.5, 98.5], [97.5, 98.0, 96.5, 97.0]], Some((4_000, 97.0, true)), None),
    ("buy, offset 0, a gap past P: rests as the Limit at P", 1, 0.0, 96.0, [ARM_BUY, [96.5, 99.0, 96.2, 98.0], FLAT], None, Some((OrderKind::Limit, Some(96.0)))),
];

fn run_row(
    p: StrategyEngineConfig,
    &(_, side, offset, activation, then, ..): &Row,
) -> StrategyEngine<Once> {
    let mut bars = vec![bar(1_000, FLAT)];
    bars.extend(then.iter().enumerate().map(|(i, &b)| bar(2_000 + 1_000 * i as i64, b)));
    run_bars(p, bars, trailing_limit(side, offset, Some(activation)))
}

#[test]
fn the_trailing_limit_law_on_bars_with_its_fee_class() {
    for row @ &(case, .., fill, rest) in ROWS {
        let engine = run_row(params(), row);
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
        let want: Vec<_> = rest.map(|(kind, px)| (kind, px, None)).into_iter().collect();
        assert_eq!(resting(&engine), want, "{case}: what rests");
        if rest.is_some_and(|(kind, _)| kind == OrderKind::TrailingLimit) {
            assert_eq!(engine.core.pending_of(SYM)[0].extreme, None, "{case}: not activated");
        }
    }
}

/// **A trailing limit pays no fixed slippage**, as a taker on its fire and as the maker it turns
/// into: every row fills at the price it fills at with no amount, to the bit.
#[test]
fn a_trailing_limit_pays_no_fixed_slippage_on_either_fee_class() {
    for row @ &(case, .., fill, _) in ROWS {
        let engine = run_row(StrategyEngineConfig { slippage_amount: AMOUNT, ..params() }, row);
        let fills = &engine.strategy.fills;
        match fill {
            Some((ts, px, maker)) => {
                assert_eq!(fills.len(), 1, "{case}: {fills:?}");
                assert_fill(case, &fills[0], ts, px, maker);
            }
            None => assert!(fills.is_empty(), "{case}: {fills:?}"),
        }
    }
}

// ---- 2. a fire inside a bar that rests: sub-bars and ticks ----

/// A sell waiting for 104 over the coarse bar 1 `[104.5, 106, 100, 101]` (`subs` its detail tape;
/// none: the coarse lane). Bar 2 never comes back to 103.
fn granular_run(subs: Vec<Bar>) -> StrategyEngine<Once> {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [104.5, 106.0, 100.0, 101.0]),
        bar(3_000, [101.0, 102.0, 100.5, 101.5]),
    ];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    run_bars(p, bars, trailing_limit(-1, 1.0, Some(104.0)))
}

/// The coarse path is continuous, so it fires at P 104 and fills there. Its sub-bars say the price
/// jumped from 105.5 to 102.5, past P and past L 103: the fire inside the coarse bar is at 102.5
/// and rests as the `Limit` at 103, which the next sub-bar fills as a maker. Without that sub-bar
/// it rests on.
#[test]
fn a_fire_inside_a_bar_rests_where_its_sub_bars_gap_past_the_limit() {
    let coarse = granular_run(Vec::new());
    assert_eq!(coarse.strategy.fills.len(), 1, "{:?}", coarse.strategy.fills);
    assert_fill("coarse: the path at P", &coarse.strategy.fills[0], 2_000, 104.0, false);

    let arm = bar(2_000, ARM_SELL);
    let gap = bar(2_300, [102.5, 102.8, 100.0, 101.0]); // fires at its open, never reaches 103
    let back = bar(2_600, [101.0, 103.5, 100.5, 101.0]); // the Limit at 103 fills
    let walked = granular_run(vec![arm.clone(), gap.clone(), back]);
    assert_eq!(walked.strategy.fills.len(), 1, "{:?}", walked.strategy.fills);
    assert_fill("granular: the Limit fills later", &walked.strategy.fills[0], 2_600, 103.0, true);

    let never = granular_run(vec![arm, gap]);
    assert!(never.strategy.fills.is_empty(), "{:?}", never.strategy.fills);
    assert_eq!(resting(&never), vec![(OrderKind::Limit, Some(103.0), None)]);
}

/// On the quote the sell waiting for 104 activates on the bid (104.5), trails it (106), and fires on
/// the bid at or under P 104: at that bid when it is within L 103, a taker; a bid that jumped past
/// L rests it as the `Limit` at 103, which the next bid at 103 fills as a maker.
#[test]
fn the_tick_lane_fires_on_the_bid_and_rests_when_it_jumps_past_the_limit() {
    let run = |(bid, ask)| {
        let tape = vec![
            q(1, 99.0, 100.0),
            q(2, 104.5, 105.5),
            q(3, 106.0, 107.0),
            q(4, bid, ask),
            q(5, 103.0, 104.0),
        ];
        run_ticks(FillModelKind::Tick, tape, trailing_limit(-1, 1.0, Some(104.0)))
    };
    let within = run((103.5, 104.5));
    assert_eq!(within.strategy.fills.len(), 1, "{:?}", within.strategy.fills);
    assert_fill("the bid within L", &within.strategy.fills[0], 4, 103.5, false);

    let past = run((102.5, 103.5));
    assert_eq!(past.strategy.fills.len(), 1, "{:?}", past.strategy.fills);
    assert_fill("past L: the Limit fills later", &past.strategy.fills[0], 5, 103.0, true);
}

// ---- 3. no activation ----

/// With no activation it trails from the mark at once by the model's plain law: the prior bar's
/// extreme, ratcheted after the test. Walked by hand, the oracle is that law itself.
#[test]
fn with_no_activation_it_is_the_plain_law_bit_for_bit() {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [100.0, 103.0, 99.0, 102.0]),
        bar(3_000, [102.0, 102.5, 100.0, 100.5]),
    ];
    let engine = run_bars(params(), bars.clone(), trailing_limit(-1, 1.0, None));
    let mut oracle = OrderResting {
        trail: Some(TRAIL),
        extreme: Some(100.0),
        limit_offset: Some(1.0),
        ..OrderResting::new(OrderKind::TrailingLimit, -1, 1.0)
    };
    assert_eq!(order_fill_price(&mut oracle, &bars[1]), None, "extreme 103, P 98 not reached");
    let want = order_fill_price(&mut oracle, &bars[2]).expect("P 101 fires");
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!(fills[0].price.to_bits(), want.to_bits());
    assert_fill("the plain law", &fills[0], 3_000, 101.0, false);
}

// ---- 4. the intake ----

/// Terms `vike_model::order_terms_refusal` refuses never rest: the verb drops them in that rule's
/// own words, with its trail and its offset, as live refuses them.
#[test]
fn trailing_limit_terms_the_order_rule_refuses_are_dropped_in_its_words() {
    for (case, trail, offset, activation) in [
        ("a trail of zero", 0.0, 1.0, None),
        ("a trail that is not a number", f64::NAN, 1.0, None),
        ("an offset below zero", TRAIL, -1.0, None),
        ("an activation not above zero", TRAIL, 1.0, Some(0.0)),
    ] {
        let once = Once::new(move |b| {
            b.submit_trailing_limit(SYM, -1, 1.0, trail, offset, activation, 0.0, true);
        });
        let engine = run_bars(params(), vec![bar(1_000, FLAT), bar(2_000, FLAT)], once);
        let rule = &OrderType::TrailingLimit;
        let why = vike_model::order_terms_refusal(
            rule,
            None,
            activation,
            Some(trail),
            Some(offset),
            None,
            None,
        );
        let why = why.expect(case);
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop { symbol: SYM.to_string(), reason: why, size: 1.0, weight: 0.0 }],
            "{case}"
        );
    }
}

/// The pre-trade gate judges the request live sends: no price, the activation its trigger price.
/// One unit of a sell waiting for 300 breaks a 200 notional cap; with no activation it is judged
/// at the mark, 100, and rests.
#[test]
fn the_pre_trade_gate_judges_a_trailing_limit_on_its_activation() {
    let run = |cap: Option<f64>, activation: Option<f64>| {
        let p = StrategyEngineConfig {
            default_venue: Some("TEST".into()),
            risk_limits: cap
                .map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() }),
            ..params()
        };
        let bars = vec![bar(1_000, FLAT), bar(2_000, FLAT)];
        run_bars(p, bars, trailing_limit(-1, 1.0, activation))
    };
    let refused = run(Some(200.0), Some(300.0));
    assert!(refused.core.pending_of(SYM).is_empty());
    let reasons: Vec<_> = refused.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["over-max-notional"]);

    let rested = run(None, Some(300.0));
    assert_eq!(resting(&rested), vec![(OrderKind::TrailingLimit, None, None)]);
    let at_the_mark = run(Some(200.0), None);
    assert_eq!(resting(&at_the_mark), vec![(OrderKind::TrailingLimit, None, None)]);
}
