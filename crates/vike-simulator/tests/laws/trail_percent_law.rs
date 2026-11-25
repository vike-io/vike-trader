//! The percent-trail law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_trailing_percent`] on the three
//! trailing kinds (`trailing`, `trailing_limit`, `trailing_take_profit`) driven through the engine's
//! fill lanes (decision 0267, 2.12).
//!
//! The distance at every step is extreme × p / 100 (`vike_model::OrderResting::trail_distance`, the ONE
//! place a percent becomes a price): a sell stop sits at extreme × (1 − p/100), a buy at extreme × (1 +
//! p/100), the trailing take-profit's trigger mirrors it. Everything else is each kind's own law, the
//! activation walk included. Assumption (each venue's text agrees): the distance is measured from the
//! extreme since activation. Pinned here:
//!
//! 1. on coarse bars, every kind and side, bit for bit the model's own law walked by hand (the path walk
//!    when the order waits for an activation), each fill with its fee class;
//! 2. a percent of the extreme IS the price trail of its distance while the extreme stands;
//! 3. the distance moves with the extreme: on the quote, a percent trail fires where a price trail of
//!    the distance at submit does not, and later;
//! 4. sub-bars (the granular law) and the L2 lane (the book walk after the quote's trigger);
//! 5. the intake: the order-terms rule refuses in its own words.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_fills::fill_model::trailing_path_fill;
use vike_marketdata::Bar;
use vike_model::{
    OrderDrop, OrderKind, OrderResting, OrderType, order_fill_price, order_fill_price_granular,
};
use vike_simulator::{FillModelKind, StrategyEngineConfig};

use crate::stop_limit_law::{
    FLAT, Once, SYM, assert_fill, bar, book, params, q, run_bars, run_ticks,
};

/// The percent of the bar cases.
const PERCENT: f64 = 2.0;

fn percent(
    order_type: OrderType,
    side: i32,
    size: f64,
    pct: f64,
    activation: Option<f64>,
    offset: Option<f64>,
) -> Once {
    Once::new(move |b| {
        let order_type = order_type.clone();
        b.submit_trailing_percent(SYM, order_type, side, size, pct, activation, offset, 0.0, true);
    })
}

// ---- 1. the law on coarse bars, walked by hand ----

/// Bar 0 is the submit bar (the mark 100); every kind below fires on bar 1 or 2.
fn tape() -> Vec<Bar> {
    [FLAT, [100.0, 105.0, 100.0, 104.0], [104.0, 104.5, 102.0, 103.0], [103.0, 106.0, 101.0, 105.0]]
        .into_iter()
        .enumerate()
        .map(|(i, b)| bar(1_000 * (i as i64 + 1), b))
        .collect()
}

/// The order the verb rests, walked over `bars` (bar 0 the submit bar) by the model's own law, as the
/// engine's `fill_price_for` hands it: the path walk while a trailing stop or trailing limit carries an
/// activation, the plain law otherwise. The first fill's `(ts, price, maker)`.
fn oracle(
    kind: OrderKind,
    side: i32,
    activation: Option<f64>,
    offset: Option<f64>,
    bars: &[Bar],
) -> Option<(i64, f64, bool)> {
    let mut o = OrderResting {
        trail_percent: Some(PERCENT),
        limit_offset: offset,
        activation,
        // the mark 100 activates none of the levels below, so a waiting order has no extreme yet
        extreme: activation.is_none().then_some(100.0),
        ..OrderResting::new(kind, side, 1.0)
    };
    for b in &bars[1..] {
        let walked = o.activation.is_some()
            && matches!(o.kind, OrderKind::Trailing | OrderKind::TrailingLimit);
        let fill = if walked { trailing_path_fill(&mut o, b) } else { order_fill_price(&mut o, b) };
        if let Some(px) = fill {
            return Some((b.timestamp, px, o.books_as_maker()));
        }
    }
    None
}

/// (case, kind word, kind, side, activation, offset)
type Row = (&'static str, OrderType, OrderKind, i32, Option<f64>, Option<f64>);

fn rows() -> Vec<Row> {
    use OrderKind as K;
    use OrderType as T;
    vec![
        ("trailing sell", T::Trailing, K::Trailing, -1, None, None),
        ("trailing buy", T::Trailing, K::Trailing, 1, None, None),
        ("trailing sell waiting for 104", T::Trailing, K::Trailing, -1, Some(104.0), None),
        (
            "trailing limit sell, offset 0.5",
            T::TrailingLimit,
            K::TrailingLimit,
            -1,
            None,
            Some(0.5),
        ),
        (
            "trailing limit sell waiting for 104, offset 0.5",
            T::TrailingLimit,
            K::TrailingLimit,
            -1,
            Some(104.0),
            Some(0.5),
        ),
        ("trailing take-profit sell", T::TrailingTakeProfit, K::TrailingTakeProfit, -1, None, None),
        ("trailing take-profit buy", T::TrailingTakeProfit, K::TrailingTakeProfit, 1, None, None),
        (
            "trailing take-profit buy, offset 0.5",
            T::TrailingTakeProfit,
            K::TrailingTakeProfit,
            1,
            None,
            Some(0.5),
        ),
    ]
}

#[test]
fn on_bars_every_kind_trails_its_percent_bit_for_bit_by_the_models_law() {
    for (case, order_type, kind, side, activation, offset) in rows() {
        let engine =
            run_bars(params(), tape(), percent(order_type, side, 1.0, PERCENT, activation, offset));
        let (ts, px, maker) = oracle(kind, side, activation, offset, &tape()).expect(case);
        let fills = &engine.strategy.fills;
        assert_eq!(fills.len(), 1, "{case}: {fills:?}");
        assert_eq!(fills[0].price.to_bits(), px.to_bits(), "{case}: {} vs {px}", fills[0].price);
        assert_fill(case, &fills[0], ts, px, maker);
        assert!(engine.core.dropped.is_empty(), "{case}: {:?}", engine.core.dropped);
    }
}

// ---- 2. a percent of the extreme is the price trail of its distance ----

/// At the mark 100 a sell stop trailing 2 % and one trailing 2 in price stand at 98: a bar that falls
/// through it fills both at the same price, on the same bar, to the bit.
#[test]
fn a_percent_trail_is_the_price_trail_of_its_distance_while_the_extreme_stands() {
    let bars = || vec![bar(1_000, FLAT), bar(2_000, [100.0, 100.0, 97.0, 98.0])];
    let by_percent =
        run_bars(params(), bars(), percent(OrderType::Trailing, -1, 1.0, PERCENT, None, None));
    let by_price =
        run_bars(params(), bars(), Once::new(|b| b.submit_trailing(SYM, -1, 1.0, 2.0, 0.0, true)));
    assert_eq!(by_percent.strategy.fills.len(), 1, "{:?}", by_percent.strategy.fills);
    assert_eq!(by_percent.strategy.fills, by_price.strategy.fills);
    assert_fill("98", &by_percent.strategy.fills[0], 2_000, 98.0, false);
}

// ---- 3. the distance moves with the extreme ----

/// A sell trailing 10 % of the BID from the mark 100: the bid of 109 lifts the extreme, and the
/// distance with it, to 10.9, so its stop is 98.1. A bid of 98.5 does not reach it (a price trail of 10,
/// the distance at submit, fires there, at 99); the bid of 98 does.
#[test]
fn on_the_quote_the_distance_moves_with_the_extreme() {
    let tape = || vec![q(1, 99.0, 101.0), q(2, 109.0, 111.0), q(3, 98.5, 99.5), q(4, 98.0, 99.0)];
    let engine = run_ticks(
        FillModelKind::Tick,
        tape(),
        percent(OrderType::Trailing, -1, 1.0, 10.0, None, None),
    );
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the stop 98.1", &engine.strategy.fills[0], 4, 98.0, false);

    let price = Once::new(|b| b.submit_trailing(SYM, -1, 1.0, 10.0, 0.0, true));
    let by_price = run_ticks(FillModelKind::Tick, tape(), price);
    assert_eq!(by_price.strategy.fills.len(), 1, "{:?}", by_price.strategy.fills);
    assert_fill("the stop 99", &by_price.strategy.fills[0], 3, 98.5, false);
}

// ---- 4. sub-bars and the L2 lane ----

/// On the coarse bar 1 the stop at 98 is not reached (its low is 100), and its high of 105 only lifts
/// the stop for the NEXT bar. Its sub-bars say the high came first: the first sub-bar lifts the stop to
/// 102.9 and the second falls through it, inside the same coarse bar: the granular law's fill, to the
/// bit.
#[test]
fn sub_bars_ratchet_the_percent_per_sub_bar() {
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 100.0, 105.0, 101.0, 104.0, 50.0),
        vike_simulator::test_support::bar(2_500, 104.0, 104.0, 102.5, 103.0, 50.0),
    ];
    let p =
        StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs.clone())], ..params() };
    let bars = vec![bar(1_000, FLAT), bar(2_000, [100.0, 105.0, 100.0, 104.0])];
    let engine = run_bars(p, bars, percent(OrderType::Trailing, -1, 1.0, PERCENT, None, None));
    let mut o = OrderResting {
        trail_percent: Some(PERCENT),
        extreme: Some(100.0),
        ..OrderResting::new(OrderKind::Trailing, -1, 1.0)
    };
    let (px, k) = order_fill_price_granular(&mut o, &subs).expect("the second sub-bar fires it");
    assert_eq!(k, 1);
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_eq!(engine.strategy.fills[0].price.to_bits(), px.to_bits());
    assert_fill("the second sub-bar", &engine.strategy.fills[0], 2_500, px, false);
}

/// On L2 the quote's trigger (the bid of 98 under the stop 98.1, as above) then walks the book: two
/// units take 1 at 98 and 1 at 97.
#[test]
fn the_l2_lane_fires_on_the_quote_and_walks_the_book() {
    let tape = vec![
        book(1, &[(99.0, 5.0)], &[(101.0, 5.0)]),
        q(2, 99.0, 101.0),
        q(3, 109.0, 111.0),
        book(4, &[(98.0, 1.0), (97.0, 5.0)], &[(99.0, 5.0)]),
        q(5, 98.0, 99.0),
    ];
    let engine = run_ticks(
        FillModelKind::L2Book,
        tape,
        percent(OrderType::Trailing, -1, 2.0, 10.0, None, None),
    );
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the walk", &engine.strategy.fills[0], 5, 97.5, false);
}

// ---- 5. the intake ----

/// Terms `vike_model::order_terms_refusal` refuses never rest: the verb drops them in that rule's own
/// words.
#[test]
fn percent_terms_the_order_rule_refuses_are_dropped_in_its_words() {
    let cases = [
        ("a percent of zero", OrderType::Trailing, 0.0, None, None),
        ("a percent of 100", OrderType::Trailing, 100.0, None, None),
        ("a percent below zero", OrderType::Trailing, -1.0, None, None),
        ("a percent that is not a number", OrderType::Trailing, f64::NAN, None, None),
        ("a stop takes no percent", OrderType::Stop, PERCENT, None, None),
        ("a trailing limit needs an offset", OrderType::TrailingLimit, PERCENT, None, None),
        (
            "a trailing take-profit takes no activation",
            OrderType::TrailingTakeProfit,
            PERCENT,
            Some(104.0),
            None,
        ),
    ];
    for (case, order_type, pct, activation, offset) in cases {
        let why = vike_model::order_terms_refusal(
            &order_type,
            None,
            activation,
            None,
            offset,
            None,
            Some(pct),
        );
        let once = percent(order_type, -1, 1.0, pct, activation, offset);
        let engine = run_bars(params(), vec![bar(1_000, FLAT), bar(2_000, FLAT)], once);
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
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
