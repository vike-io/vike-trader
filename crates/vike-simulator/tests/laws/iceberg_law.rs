//! The iceberg law, backtest side: [`vike_simulator::OrderBrokerSimulator::submit_limit_iceberg`], a limit that
//! shows `display` of its size (`vike_model::OrderRequest::display_quantity`) and hides the rest.
//!
//! No bar and no tick shows a queue, and a trade through the price takes the hidden part as well,
//! so an iceberg rests and fills EXACTLY as the full-size limit does: the same fills, prices, sizes
//! and fee class, on bars, sub-bars, the tick lane and the L2 lane. The one model that would read
//! a hidden part differently is the queue model, and it has no law for one (only an L3 feed could
//! say where a hidden part stands, decision 0045): under it the iceberg is REFUSED, in words.
//! Pinned here, with the refusal of a display below zero (a display of zero is a hidden order).
//!
//! The helpers are `stop_limit_law.rs`'s.

use vike_model::OrderDrop;
use vike_simulator::{FillModelKind, QueueModelKind, StrategyEngine, StrategyEngineConfig};

use crate::stop_limit_law::{FLAT, Once, SYM, bar, book, params, q, resting, run_bars, run_ticks};

fn iceberg(side: i32, price: f64, display: f64) -> Once {
    Once::new(move |b| b.submit_limit_iceberg(SYM, side, 3.0, price, display, 0.0, true, None))
}

fn limit(side: i32, price: f64) -> Once {
    Once::new(move |b| b.submit_limit(SYM, side, 3.0, price, 0.0, true, None))
}

/// The two runs agree on every fill and on what rests.
fn assert_same(case: &str, a: &StrategyEngine<Once>, b: &StrategyEngine<Once>) {
    assert_eq!(a.strategy.fills, b.strategy.fills, "{case}: the fills");
    assert_eq!(resting(a), resting(b), "{case}: what rests");
    assert!(a.core.dropped.is_empty(), "{case}: {:?}", a.core.dropped);
}

/// (case, side, limit): a buy and a sell reached on bar 1, one improved by bar 1's gap, and one
/// never reached (it rests).
const ROWS: &[(&str, i32, f64)] = &[
    ("buy, reached", 1, 100.0),
    ("buy, the gap improves it", 1, 102.0),
    ("buy, never reached", 1, 98.0),
    ("sell, reached", -1, 101.5),
];

#[test]
fn on_bars_and_sub_bars_an_iceberg_is_the_full_size_limit() {
    let bars = || vec![bar(1_000, FLAT), bar(2_000, [101.0, 102.0, 99.5, 101.0]), bar(3_000, FLAT)];
    let subs = vec![
        vike_simulator::test_support::bar(2_000, 101.0, 101.2, 100.5, 101.0, 50.0),
        vike_simulator::test_support::bar(2_500, 101.0, 102.0, 99.5, 100.0, 50.0),
    ];
    let granular = || StrategyEngineConfig {
        granular_by_symbol: vec![(SYM.into(), subs.clone())],
        ..params()
    };
    for &(case, side, px) in ROWS {
        let a = run_bars(params(), bars(), iceberg(side, px, 1.0));
        assert_same(case, &a, &run_bars(params(), bars(), limit(side, px)));
        let a = run_bars(granular(), bars(), iceberg(side, px, 1.0));
        assert_same(case, &a, &run_bars(granular(), bars(), limit(side, px)));
    }
}

#[test]
fn on_the_tick_and_l2_lanes_an_iceberg_is_the_full_size_limit() {
    let quotes = || vec![q(1, 101.0, 102.0), q(2, 99.0, 100.0), q(3, 98.0, 99.0)];
    for &(case, side, px) in ROWS {
        let a = run_ticks(FillModelKind::Tick, quotes(), iceberg(side, px, 1.0));
        assert_same(case, &a, &run_ticks(FillModelKind::Tick, quotes(), limit(side, px)));
    }
    let tape = || {
        vec![
            book(1, &[(99.0, 5.0)], &[(101.0, 5.0)]),
            q(2, 99.0, 101.0),
            book(3, &[(99.0, 5.0)], &[(100.0, 2.0), (101.0, 5.0)]),
            q(4, 99.0, 100.0),
        ]
    };
    let a = run_ticks(FillModelKind::L2Book, tape(), iceberg(1, 100.5, 1.0));
    let b = run_ticks(FillModelKind::L2Book, tape(), limit(1, 100.5));
    assert_same("the L2 walk", &a, &b);
}

/// Under the queue model the iceberg is refused in words and nothing rests; the full-size limit
/// beside it rests as ever.
#[test]
fn under_the_queue_model_an_iceberg_is_refused_in_words() {
    let run = |submit: Once| {
        let p = StrategyEngineConfig { queue_model: Some(QueueModelKind::RiskAverse), ..params() };
        let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], submit, p);
        engine.run_ticks(&[(SYM.to_string(), vec![q(1, 101.0, 102.0), q(2, 101.0, 102.0)])]);
        engine
    };
    let refused = run(iceberg(1, 100.0, 1.0));
    let why = "an iceberg has no queue law: its hidden part needs L3 (decision 0045)";
    assert_eq!(
        refused.core.dropped,
        vec![OrderDrop {
            symbol: SYM.to_string(),
            reason: why.to_string(),
            size: 3.0,
            weight: 0.0
        }]
    );
    assert!(refused.core.pending_of(SYM).is_empty());
    assert_eq!(run(limit(1, 100.0)).core.pending_of(SYM).len(), 1, "a plain limit rests");
}

/// A display below zero (or not a number), or not below the order's size (it hides nothing), is
/// refused in the doors' words (`vike_model::display_refusal`). A display of ZERO is a hidden order
/// since the attributes wave, admitted by that rule: its law is the attributes wave's sim row.
#[test]
fn a_display_not_above_zero_or_not_below_the_size_is_refused() {
    for display in [-1.0, f64::NAN, 3.0, 4.0] {
        let bars = vec![bar(1_000, FLAT), bar(2_000, FLAT)];
        let engine = run_bars(params(), bars, iceberg(1, 100.0, display));
        let reasons: Vec<_> = engine.core.dropped.iter().map(|d| d.reason.clone()).collect();
        let want = vike_model::display_refusal(&vike_model::OrderType::Limit, 3.0, Some(display));
        assert_eq!(reasons, [want.expect("refused")], "{display}");
        assert!(engine.core.pending_of(SYM).is_empty());
    }
}
