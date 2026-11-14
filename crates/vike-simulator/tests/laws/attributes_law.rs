//! The order-attributes law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_limit_attributed`] (all-or-none,
//! sweep-to-fill, a discretionary amount) and the HIDDEN order, [`vike_simulator::OrderBrokerSimulator::submit_limit_iceberg`]
//! with a display of zero, driven through the fill lanes (decision 0267, 2.1 to 2.4).
//!
//! - SWEEP-TO-FILL is the limit on every lane, the queue model included: one book already fills a
//!   marketable limit within its price.
//! - ALL-OR-NONE is the limit, except that a volume cap which would cut it holds it WHOLE: no fill, nothing
//!   in `dropped`, and it fills whole on the first event whose volume lets all of it through (coarse,
//!   granular and cash-gated lanes). An L1 quote has no depth and the L2 lane fills a limit whole or
//!   rests it, so there it is the limit too. The SL/TP cap never cuts it either
//!   (`vike_fills::fill_resolution`'s rule): an exit the cap cannot leave whole fills nothing in that
//!   bar and rests on whole, as in the paper book.
//! - A DISCRETIONARY limit at L with an amount d is judged at L + d for a buy and L − d for a sell, and
//!   books as a TAKER (a falling price meets L + d before L: the limit law's range optimism, stated as
//!   an assumption). The pre-trade gate prices it at L ± d, its worst price.
//! - A HIDDEN order is the full-size limit on every lane (assumption: its lost priority behind the size
//!   displayed at its own price is not modelled, which is optimistic).
//! - The queue model refuses hidden, all-or-none and discretionary orders in words: no queue reading
//!   states their law.
//! - A release whose price condition is read on ANOTHER venue (2.13) is refused in words: the simulator
//!   names an instrument by its symbol alone.
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_marketdata::Bar;
use vike_model::{
    OrderAttributes, OrderDrop, OrderKind, OrderRelease, OrderReleasePrice, OrderReleasePriceSide,
    OrderType, RiskLimits,
};
use vike_simulator::{FillModelKind, QueueModelKind, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{
    AMOUNT, FLAT, Once, SYM, assert_fill, bar, book, params, q, resting, run_bars, run_ticks,
};

fn attributed(side: i32, size: f64, price: f64, a: OrderAttributes) -> Once {
    Once::new(move |b| b.submit_limit_attributed(SYM, side, size, price, a, 0.0, true))
}

fn limit(side: i32, size: f64, price: f64) -> Once {
    Once::new(move |b| b.submit_limit(SYM, side, size, price, 0.0, true, None))
}

fn hidden(side: i32, size: f64, price: f64) -> Once {
    Once::new(move |b| b.submit_limit_iceberg(SYM, side, size, price, 0.0, 0.0, true, None))
}

const AON: OrderAttributes =
    OrderAttributes { all_or_none: true, sweep_to_fill: false, discretionary_amount: None };
const SWEEP: OrderAttributes =
    OrderAttributes { all_or_none: false, sweep_to_fill: true, discretionary_amount: None };

fn discretion(d: f64) -> OrderAttributes {
    OrderAttributes { discretionary_amount: Some(d), ..OrderAttributes::default() }
}

/// The run's `dropped` list as reasons.
fn dropped(engine: &StrategyEngine<Once>) -> Vec<String> {
    engine.core.dropped.iter().map(|d| d.reason.clone()).collect()
}

// ---- 1. sweep, all-or-none with no cap, hidden: the limit on every lane ----

/// (case, side, limit): reached on bar 1, improved by its gap, never reached, and a sell.
const ROWS: &[(&str, i32, f64)] = &[
    ("buy, reached", 1, 100.0),
    ("buy, the gap improves it", 1, 102.0),
    ("buy, never reached", 1, 98.0),
    ("sell, reached", -1, 101.5),
];

/// Every lane's run of `make(side, price)`: coarse bars, sub-bars, the tick lane and the L2 lane.
fn every_lane(make: &dyn Fn(i32, f64) -> Once, side: i32, px: f64) -> Vec<StrategyEngine<Once>> {
    let bars = || vec![bar(1_000, FLAT), bar(2_000, [101.0, 102.0, 99.5, 101.0]), bar(3_000, FLAT)];
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 101.0, 101.2, 100.5, 101.0, 50.0),
        vike_simulator::test_support::bar(2_500, 101.0, 102.0, 99.5, 100.0, 50.0),
    ];
    let granular =
        StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    let quotes = vec![q(1, 101.0, 102.0), q(2, 99.0, 100.0), q(3, 98.0, 99.0)];
    let tape = vec![
        book(1, &[(99.0, 5.0)], &[(101.0, 5.0)]),
        q(2, 99.0, 101.0),
        book(3, &[(99.0, 5.0)], &[(100.0, 2.0), (101.0, 5.0)]),
        q(4, 99.0, 100.0),
    ];
    vec![
        run_bars(params(), bars(), make(side, px)),
        run_bars(granular, bars(), make(side, px)),
        run_ticks(FillModelKind::Tick, quotes, make(side, px)),
        run_ticks(FillModelKind::L2Book, tape, make(side, px)),
    ]
}

/// `a` and the plain limit agree on every fill (price, size, fee class) and on what rests, lane by
/// lane, and refuse nothing; the reached rows really fill, so the comparison is not vacuous.
fn assert_the_limit(what: &str, a: &dyn Fn(i32, f64) -> Once) {
    let plain = |side, px| limit(side, 2.0, px);
    for &(case, side, px) in ROWS {
        let lanes = every_lane(a, side, px).into_iter().zip(every_lane(&plain, side, px));
        for (lane, (a, b)) in lanes.enumerate() {
            let case = format!("{what}, {case}, lane {lane}");
            assert_eq!(a.strategy.fills, b.strategy.fills, "{case}: the fills");
            assert_eq!(resting(&a), resting(&b), "{case}: what rests");
            assert!(a.core.dropped.is_empty(), "{case}: {:?}", a.core.dropped);
        }
    }
    let reached = run_bars(
        params(),
        vec![bar(1_000, FLAT), bar(2_000, [101.0, 102.0, 99.5, 101.0])],
        a(1, 100.0),
    );
    assert_eq!(reached.strategy.fills.len(), 1, "{what}: {:?}", reached.strategy.fills);
}

#[test]
fn sweep_to_fill_is_the_limit_on_every_lane() {
    assert_the_limit("sweep", &|side, px| attributed(side, 2.0, px, SWEEP));
}

#[test]
fn with_no_volume_cap_an_all_or_none_limit_is_the_limit_on_every_lane() {
    assert_the_limit("all-or-none", &|side, px| attributed(side, 2.0, px, AON));
}

#[test]
fn a_hidden_order_is_the_full_size_limit_on_every_lane() {
    assert_the_limit("hidden", &|side, px| hidden(side, 2.0, px));
}

// ---- 2. all-or-none under the volume cap ----

/// One unit per bar of 100.
fn capped() -> StrategyEngineConfig {
    StrategyEngineConfig { volume_limit: Some(0.01), ..params() }
}

/// Bar 1 reaches 100 and lets one unit through; bar 2 reaches it again and lets three through.
fn cap_tape(upto: usize) -> Vec<Bar> {
    let bars = vec![
        bar(1_000, FLAT),
        bar(2_000, [101.0, 102.0, 99.5, 101.0]),
        vike_simulator::test_support::bar(3_000, 100.5, 101.0, 99.0, 100.0, 300.0),
    ];
    bars.into_iter().take(upto).collect()
}

/// A cap that would cut an all-or-none buy of two holds it WHOLE: no fill, nothing dropped, the order
/// resting as it was; the next bar lets both units through and it fills whole, a maker. A plain limit
/// fills the unit the cap leaves and loses the other to `volume_cap`.
#[test]
fn a_volume_cap_that_would_cut_an_all_or_none_limit_holds_it_whole() {
    let held = run_bars(capped(), cap_tape(2), attributed(1, 2.0, 100.0, AON));
    assert!(held.strategy.fills.is_empty(), "{:?}", held.strategy.fills);
    assert!(held.core.dropped.is_empty(), "{:?}", held.core.dropped);
    assert_eq!(resting(&held), vec![(OrderKind::Limit, Some(100.0), None)]);
    let o = &held.core.pending_of(SYM)[0];
    assert_eq!((o.size, o.all_or_none), (2.0, true), "whole, and still all-or-none");

    for cash_gate in [false, true] {
        let p = StrategyEngineConfig { cash_gate, ..capped() };
        let whole = run_bars(p, cap_tape(3), attributed(1, 2.0, 100.0, AON));
        let fills = &whole.strategy.fills;
        assert_eq!(fills.len(), 1, "cash gate {cash_gate}: {fills:?}");
        assert_fill("whole, on the bar that lets it through", &fills[0], 3_000, 100.0, true);
        assert_eq!(fills[0].size, 2.0);
        assert!(whole.core.dropped.is_empty() && resting(&whole).is_empty(), "{cash_gate}");
    }

    let plain = run_bars(capped(), cap_tape(3), limit(1, 2.0, 100.0));
    assert_eq!(plain.strategy.fills.len(), 1, "{:?}", plain.strategy.fills);
    assert_eq!((plain.strategy.fills[0].timestamp, plain.strategy.fills[0].size), (2_000, 1.0));
    assert_eq!(dropped(&plain), ["volume_cap"]);
}

/// On sub-bars the cap is per sub-bar: the first lets half a unit through and holds the order whole,
/// the second lets four through and fills it, inside the same coarse bar.
#[test]
fn on_sub_bars_an_all_or_none_limit_fills_on_the_sub_bar_that_can_fill_it() {
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 101.0, 101.5, 99.8, 100.0, 50.0),
        vike_simulator::test_support::bar(2_500, 100.0, 100.5, 99.5, 100.2, 400.0),
    ];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..capped() };
    let engine = run_bars(p, cap_tape(2), attributed(1, 2.0, 100.0, AON));
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_fill("the second sub-bar, whole", &fills[0], 2_500, 100.0, true);
    assert_eq!(fills[0].size, 2.0);
    assert!(engine.core.dropped.is_empty(), "{:?}", engine.core.dropped);
}

/// Long 3 from bar 1 with two exits, a stop-limit selling 2 (T 95, L 94) and an all-or-none limit
/// selling 2 at 105; bar 2 reaches both. The stop-limit is adverse and takes 2, and the cap would leave
/// the all-or-none exit 1 of its 2: it fills NOTHING in that bar and rests on whole, and the next bar
/// that reaches 105 fills all of it (the paper book's `an_all_or_none_exit_the_bracket_cap_would_cut_rests_on_whole`).
/// The same exit without the flag is cut to 1 and nothing rests.
#[test]
fn the_stop_loss_take_profit_cap_never_cuts_an_all_or_none_exit_it_rests_on_whole() {
    let run = |a: OrderAttributes, upto: usize| {
        let bars = vec![
            bar(1_000, FLAT),
            bar(2_000, FLAT),
            bar(3_000, [100.0, 106.0, 93.0, 100.0]),
            bar(4_000, [104.0, 106.0, 103.0, 105.0]),
        ];
        let exits = Once::new(move |b| {
            b.submit(SYM, 1, 3.0, 0.0, true, None);
            b.submit_stop_limit(SYM, -1, 2.0, 95.0, 94.0, 0.0, true);
            b.submit_limit_attributed(SYM, -1, 2.0, 105.0, a, 0.0, true);
        });
        run_bars(params(), bars.into_iter().take(upto).collect(), exits)
    };
    let sized = |engine: &StrategyEngine<Once>| -> Vec<(i32, f64, f64)> {
        engine.strategy.fills.iter().map(|f| (f.side, f.size, f.price)).collect()
    };
    let held = run(AON, 3);
    assert_eq!(
        sized(&held),
        [(1, 3.0, 100.0), (-1, 2.0, 95.0)],
        "the stop fills; the exit nothing"
    );
    assert_eq!(held.core.intrabar_both_hit, 1);
    let o = &held.core.pending_of(SYM)[0];
    assert_eq!(
        (o.kind, o.price, o.size, o.all_or_none),
        (OrderKind::Limit, Some(105.0), 2.0, true)
    );

    let later = run(AON, 4);
    assert_eq!(
        sized(&later)[2..],
        [(-1, 2.0, 105.0)],
        "all of it, on the next bar that reaches it"
    );

    let plain = run(OrderAttributes::default(), 4);
    assert_eq!(sized(&plain)[1..], [(-1, 2.0, 95.0), (-1, 1.0, 105.0)], "cut to what is left");
    assert!(resting(&plain).is_empty());
}

/// Two IDENTICAL all-or-none exits but for their size (sell 1 and sell 3, both at 105) beside a
/// stop-limit selling 2 of a long of 4: the stop takes 2, the 1-lot fits and fills, the 3-lot does not
/// and rests on. The one that rests is the 3-lot, whole, found by its order id: a match by terms would
/// put the 1-lot back and lose the 3-lot. The next bar fills the 3-lot, all of it.
#[test]
fn of_two_identical_all_or_none_exits_the_one_the_cap_held_rests_by_its_order_id() {
    let run = |upto: usize| {
        let bars = vec![
            bar(1_000, FLAT),
            bar(2_000, FLAT),
            bar(3_000, [100.0, 106.0, 93.0, 100.0]),
            bar(4_000, [104.0, 106.0, 103.0, 105.0]),
        ];
        let exits = Once::new(|b| {
            b.submit(SYM, 1, 4.0, 0.0, true, None);
            b.submit_stop_limit(SYM, -1, 2.0, 95.0, 94.0, 0.0, true);
            b.submit_limit_attributed(SYM, -1, 1.0, 105.0, AON, 0.0, true);
            b.submit_limit_attributed(SYM, -1, 3.0, 105.0, AON, 0.0, true);
        });
        run_bars(params(), bars.into_iter().take(upto).collect(), exits)
    };
    let sized = |engine: &StrategyEngine<Once>| -> Vec<(i32, f64, f64)> {
        engine.strategy.fills.iter().map(|f| (f.side, f.size, f.price)).collect()
    };
    let held = run(3);
    assert_eq!(sized(&held), [(1, 4.0, 100.0), (-1, 2.0, 95.0), (-1, 1.0, 105.0)]);
    let rests: Vec<_> = held.core.pending_of(SYM).iter().map(|o| (o.size, o.all_or_none)).collect();
    assert_eq!(rests, [(3.0, true)], "the 3-lot, whole");

    let later = run(4);
    assert_eq!(sized(&later)[3..], [(-1, 3.0, 105.0)], "the 3-lot, all of it");
    assert_eq!(later.core.position_of(SYM).size, -2.0);
    assert!(resting(&later).is_empty());
}

// ---- 3. a discretionary limit ----

/// (case, side, L, d, bar 1, fill `(ts, price)`)
type DiscretionRow = (&'static str, i32, f64, f64, [f64; 4], Option<(i64, f64)>);

#[rustfmt::skip]
const DISCRETION_ROWS: &[DiscretionRow] = &[
    ("buy: L + d reached, L not", 1, 100.0, 1.0, [102.0, 103.0, 100.5, 101.0], Some((2_000, 101.0))),
    ("buy: the gap improves it below L + d", 1, 100.0, 1.0, [100.8, 101.5, 100.2, 101.0], Some((2_000, 100.8))),
    ("buy: L + d not reached", 1, 100.0, 1.0, [102.0, 103.0, 101.5, 102.0], None),
    ("sell: L − d reached, L not", -1, 100.0, 1.0, [98.0, 99.5, 97.0, 99.0], Some((2_000, 99.0))),
];

/// Every fill is a TAKER at L ± d or the better open; the plain limit at L fills none of them. A
/// discretionary limit is limit-bounded, so the fixed slippage moves none of its fills.
#[test]
fn a_discretionary_limit_fills_at_its_discretion_as_a_taker_on_bars() {
    for &(case, side, l, d, b1, fill) in DISCRETION_ROWS {
        for p in [params(), StrategyEngineConfig { slippage_amount: AMOUNT, ..params() }] {
            let bars = vec![bar(1_000, FLAT), bar(2_000, b1)];
            let engine = run_bars(p, bars.clone(), attributed(side, 1.0, l, discretion(d)));
            match fill {
                Some((ts, px)) => {
                    assert_eq!(
                        engine.strategy.fills.len(),
                        1,
                        "{case}: {:?}",
                        engine.strategy.fills
                    );
                    assert_fill(case, &engine.strategy.fills[0], ts, px, false);
                }
                None => {
                    assert_eq!(resting(&engine), vec![(OrderKind::Limit, Some(l), None)], "{case}")
                }
            }
            let plain = run_bars(params(), bars, limit(side, 1.0, l));
            assert!(plain.strategy.fills.is_empty(), "{case}: the plain limit at L");
        }
    }
}

/// Sub-bars walk it: the first sub-bar stays above L + d, the second reaches it.
#[test]
fn on_sub_bars_a_discretionary_limit_fills_where_the_path_reaches_its_discretion() {
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 102.0, 103.0, 101.5, 102.5, 50.0),
        vike_simulator::test_support::bar(2_500, 102.5, 102.5, 100.5, 101.0, 50.0),
    ];
    let p = StrategyEngineConfig { granular_by_symbol: vec![(SYM.into(), subs)], ..params() };
    let bars = vec![bar(1_000, FLAT), bar(2_000, [102.0, 103.0, 100.5, 101.0])];
    let engine = run_bars(p, bars, attributed(1, 1.0, 100.0, discretion(1.0)));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the second sub-bar", &engine.strategy.fills[0], 2_500, 101.0, false);
}

/// On the quote a buy at L 99 with a discretion of 1 takes an ask of 100, at the ask, a taker; the
/// plain limit at 99 never does. On L2 it walks the book capped at L + d: the one unit at 100.
#[test]
fn on_the_tick_and_l2_lanes_a_discretionary_limit_takes_up_to_its_discretion() {
    let quotes = || vec![q(1, 100.0, 101.0), q(2, 99.5, 100.5), q(3, 99.0, 100.0)];
    let engine =
        run_ticks(FillModelKind::Tick, quotes(), attributed(1, 1.0, 99.0, discretion(1.0)));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the ask within L + d", &engine.strategy.fills[0], 3, 100.0, false);
    let plain = run_ticks(FillModelKind::Tick, quotes(), limit(1, 1.0, 99.0));
    assert!(plain.strategy.fills.is_empty(), "{:?}", plain.strategy.fills);

    let tape = || {
        vec![
            book(1, &[(98.0, 5.0)], &[(100.0, 1.0), (101.0, 5.0)]),
            q(2, 98.0, 100.0),
            q(3, 98.0, 100.0),
        ]
    };
    let engine =
        run_ticks(FillModelKind::L2Book, tape(), attributed(1, 1.0, 99.0, discretion(1.0)));
    assert_eq!(engine.strategy.fills.len(), 1, "{:?}", engine.strategy.fills);
    assert_fill("the book within L + d", &engine.strategy.fills[0], 3, 100.0, false);
    let deep = run_ticks(FillModelKind::L2Book, tape(), attributed(1, 2.0, 99.0, discretion(1.0)));
    assert!(deep.strategy.fills.is_empty(), "one unit within L + d: {:?}", deep.strategy.fills);
    assert_eq!(resting(&deep), vec![(OrderKind::Limit, Some(99.0), None)]);
}

/// The pre-trade gate prices a discretionary buy at L + d, its worst price: one unit at L 100 with a
/// discretion of 1 breaks a 100.5 notional cap that the plain limit at 100 passes.
#[test]
fn the_pre_trade_gate_prices_a_discretionary_limit_at_its_worst_price() {
    let run = |strategy: Once| {
        let cap = RiskLimits { max_notional_per_order: Some(100.5), ..RiskLimits::new() };
        let p = StrategyEngineConfig {
            default_venue: Some("TEST".into()),
            risk_limits: Some(cap),
            ..params()
        };
        run_bars(p, vec![bar(1_000, FLAT), bar(2_000, [102.0, 103.0, 101.5, 102.0])], strategy)
    };
    let refused = run(attributed(1, 1.0, 100.0, discretion(1.0)));
    assert!(refused.core.pending_of(SYM).is_empty());
    assert_eq!(dropped(&refused), ["over-max-notional"]);
    let plain = run(limit(1, 1.0, 100.0));
    assert_eq!(resting(&plain), vec![(OrderKind::Limit, Some(100.0), None)]);
}

// ---- 4. the queue model ----

fn queued(strategy: Once) -> StrategyEngine<Once> {
    let p = StrategyEngineConfig { queue_model: Some(QueueModelKind::RiskAverse), ..params() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strategy, p);
    engine.run_ticks(&[(SYM.to_string(), vec![q(1, 101.0, 102.0), q(2, 101.0, 102.0)])]);
    engine
}

/// Under the queue model a hidden, an all-or-none and a discretionary limit are refused in words and
/// nothing rests; a sweep-to-fill limit rests as the plain limit does.
#[test]
fn the_queue_model_refuses_hidden_all_or_none_and_discretionary_in_words() {
    let cases = [
        (
            "hidden",
            hidden(1, 3.0, 100.0),
            "a hidden order has no queue law: where it stands needs L3 (decision 0045)",
        ),
        (
            "all-or-none",
            attributed(1, 3.0, 100.0, AON),
            "an all-or-none limit has no queue law: no reading of one FIFO queue proves an execution \
             that fills it whole",
        ),
        (
            "discretionary",
            attributed(1, 3.0, 100.0, discretion(1.0)),
            "a discretionary limit has no queue law: its displayed queue is at its limit while its \
             executions take liquidity inside its discretion",
        ),
    ];
    for (case, strategy, why) in cases {
        let engine = queued(strategy);
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop {
                symbol: SYM.to_string(),
                reason: why.to_string(),
                size: 3.0,
                weight: 0.0
            }],
            "{case}"
        );
        assert!(engine.core.pending_of(SYM).is_empty(), "{case}");
    }
    let sweep = queued(attributed(1, 3.0, 100.0, SWEEP));
    let plain = queued(limit(1, 3.0, 100.0));
    assert_eq!(resting(&sweep), resting(&plain));
    assert_eq!(resting(&sweep), vec![(OrderKind::Limit, Some(100.0), None)]);
}

// ---- 5. the intake ----

/// Terms `vike_model::attributes_refusal` refuses never rest: the verb drops them in its words.
#[test]
fn attribute_terms_the_rule_refuses_are_dropped_in_its_words() {
    for d in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let engine = run_bars(
            params(),
            vec![bar(1_000, FLAT), bar(2_000, FLAT)],
            attributed(1, 1.0, 100.0, discretion(d)),
        );
        let tif = vike_model::TimeInForce::Gtc;
        let why = vike_model::attributes_refusal(&OrderType::Limit, tif, Some(&discretion(d)));
        assert_eq!(
            engine.core.dropped,
            vec![OrderDrop {
                symbol: SYM.to_string(),
                reason: why.expect("refused"),
                size: 1.0,
                weight: 0.0
            }],
            "{d}"
        );
        assert!(engine.core.pending_of(SYM).is_empty(), "{d}");
    }
}

// ---- 6. a price condition read on another venue (2.13) ----

/// A release whose price condition names a venue is refused at the submit, in words, and nothing is
/// held: the simulator names an instrument by its symbol alone. The same condition on the order's own
/// venue (`venue: None`) is held as ever.
#[test]
fn a_price_condition_read_on_another_venue_is_refused_in_words() {
    let release = |venue: Option<&str>| OrderRelease {
        at: 5_000,
        price: Some(OrderReleasePrice {
            symbol: SYM.into(),
            level: 100.0,
            side: OrderReleasePriceSide::Above,
            venue: venue.map(Into::into),
        }),
    };
    let run = |r: OrderRelease| {
        let held =
            Once::new(move |b| b.submit_released(r, |b| b.submit(SYM, 1, 1.0, 0.0, true, None)));
        run_bars(params(), vec![bar(1_000, FLAT), bar(2_000, FLAT)], held)
    };
    let refused = run(release(Some("binance")));
    let why = "a price condition read on another venue (binance) is not simulated: the simulator names \
               an instrument by its symbol alone";
    assert_eq!(
        refused.core.dropped,
        vec![OrderDrop {
            symbol: SYM.to_string(),
            reason: why.to_string(),
            size: 1.0,
            weight: 0.0
        }]
    );
    assert!(refused.strategy.fills.is_empty() && refused.core.pending_of(SYM).is_empty());

    let own = run(release(None));
    assert!(own.core.dropped.is_empty(), "{:?}", own.core.dropped);
}
