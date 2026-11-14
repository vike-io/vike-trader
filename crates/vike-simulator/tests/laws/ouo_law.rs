//! The OUO law, backtest side: an OUO group ([`vike_simulator::OrderBrokerSimulator::submit_group`]) resolved by
//! `vike_execution::ContingencyBook::on_fill`, the paper book's law to the letter (`vike-paper`'s
//! `apply_contingency` and `resolve_in_pass`). Pinned:
//!
//! 1. a LADDER-shaped group (a reduce-only stop for the whole long, OUO-linked to three targets that
//!    each link the stop alone): a target's fill of q REDUCES the stop by q, in place; the stop's
//!    (partial) fill of q reduces the targets by q IN TOTAL, the FARTHEST from its fill first
//!    (`vike_execution::spend_ouo_reduce`, wave 4's ruling 2), never every target by q; on the tick lane
//!    too;
//! 2. any OTHER OUO group (`ContingencyBook::ouo_shares_its_fill` says no): a fill of q takes q off
//!    EACH resting sibling;
//! 3. a plain OUO pair hit in ONE bar: the later leg fills what the earlier fill left of its size
//!    BEFORE the one-bar cap (within the cap), or is cancelled when nothing is left.

use vike_marketdata::Bar;
use vike_model::{ContingencyType, OrderKind, OrderList, OrderListLeg, OrderType};
use vike_simulator::{FillModelKind, OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

use crate::oco_law::{LongThen, exit, leg};
use crate::stop_limit_law::{FLAT, Once, SYM, bar, params, q, run_bars};

/// Places the OUO group `legs`, each `(leg, the legs it links)`.
fn place_ouo(b: &mut OrderBrokerSimulator, legs: Vec<(OrderListLeg, Vec<usize>)>) {
    let n = legs.len();
    let legs = legs
        .into_iter()
        .map(|(l, linked)| OrderListLeg { contingency: Some(ContingencyType::Ouo), linked, ..l })
        .collect();
    let spec = OrderList { venue: String::new(), symbol: SYM.into(), legs, account: None };
    assert_eq!(b.submit_group(&spec).len(), n);
}

/// A reduce-only stop at 95 for the whole long of 3, OUO-linked to three targets of 1 at 110, 120
/// and 130, each linking the stop alone: a ladder's shape.
fn ladder_shaped(b: &mut OrderBrokerSimulator) {
    let target = |px: f64| (exit(1.0, OrderType::Limit, Some(px), None), vec![0]);
    let stop = (exit(3.0, OrderType::Stop, None, Some(95.0)), vec![1, 2, 3]);
    place_ouo(b, vec![stop, target(110.0), target(120.0), target(130.0)]);
}

/// A bar with its own volume (the volume cap's base).
fn bar_v(ts: i64, [o, h, l, c]: [f64; 4], volume: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, volume)
}

/// Long 3 at 100 on the second bar, the group placed from that fill, then `last`.
fn run(
    p: StrategyEngineConfig,
    group: fn(&mut OrderBrokerSimulator),
    last: Bar,
) -> StrategyEngine<LongThen> {
    let bars = vec![bar_v(1_000, FLAT, 1_000.0), bar_v(2_000, FLAT, 1_000.0), last];
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], LongThen::new(3.0, group), p);
    engine.run();
    engine
}

/// What rests, as `(kind, price, size)`.
fn book(pending: &[vike_model::OrderResting]) -> Vec<(OrderKind, Option<f64>, f64)> {
    pending.iter().map(|o| (o.kind, o.price, o.size)).collect()
}

#[test]
fn a_targets_fill_reduces_the_stop_by_what_it_filled_and_cancels_nothing() {
    let last = bar_v(3_000, [100.0, 112.0, 99.0, 111.0], 1_000.0); // the 110 target only
    let engine = run(params(), ladder_shaped, last);
    assert_eq!(engine.strategy.fills(), vec![(1, 100.0, 3.0), (-1, 110.0, 1.0)]);
    assert_eq!(
        book(engine.core.pending_of(SYM)),
        vec![
            (OrderKind::Stop, Some(95.0), 2.0),
            (OrderKind::Limit, Some(120.0), 1.0),
            (OrderKind::Limit, Some(130.0), 1.0),
        ],
        "the stop is the position again; the other targets untouched"
    );
}

#[test]
fn a_stops_partial_fill_spends_its_size_off_the_farthest_targets_first() {
    // the volume cap lets the stop fill 1.5 of its 3 on a bar of 100 (its remainder is the cap's)
    let p = StrategyEngineConfig { volume_limit: Some(0.015), ..params() };
    let last = bar_v(3_000, [100.0, 101.0, 94.0, 96.0], 100.0); // the stop at 95 fills 1.5
    let engine = run(p, ladder_shaped, last);
    assert_eq!(engine.strategy.fills(), vec![(1, 100.0, 3.0), (-1, 95.0, 1.5)]);
    let resting = book(engine.core.pending_of(SYM));
    assert_eq!(
        resting,
        vec![(OrderKind::Limit, Some(110.0), 1.0), (OrderKind::Limit, Some(120.0), 0.5)],
        "130 cancelled, 120 halved, 110 untouched: 1.5 in total, never 1.5 off each"
    );
    let left: f64 = resting.iter().map(|r| r.2).sum();
    assert_eq!(left, engine.core.position_of(SYM).size, "the targets sum to the position");
}

#[test]
fn the_tick_lane_reduces_the_stop_on_a_targets_fill() {
    let ticks = vec![
        q(1_000, 99.0, 100.0),  // the long is sent
        q(2_000, 99.0, 100.0),  // it fills at the ask; the group rests after it
        q(3_000, 111.0, 112.0), // the bid crosses the 110 target only
    ];
    let p = StrategyEngineConfig { fill_model: FillModelKind::Tick, ..params() };
    let strategy = LongThen::new(3.0, ladder_shaped);
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], strategy, p);
    engine.run_ticks(&[(SYM.to_string(), ticks)]);
    let fills = engine.strategy.fills();
    assert_eq!(fills.len(), 2, "the long and one target: {fills:?}");
    assert_eq!((fills[1].0, fills[1].2), (-1, 1.0), "one target of 1");
    let stop =
        engine.core.pending_of(SYM).iter().find(|o| o.kind == OrderKind::Stop).map(|o| o.size);
    assert_eq!(stop, Some(2.0), "the stop shrank by the target's fill");
}

/// Three reduce-only exits that all link one another: a stop of 3 at 95, targets of 1 at 110 and 2
/// at 120. Not a ladder's shape, so a fill reduces EACH sibling.
fn all_linked(b: &mut OrderBrokerSimulator) {
    place_ouo(
        b,
        vec![
            (exit(3.0, OrderType::Stop, None, Some(95.0)), vec![1, 2]),
            (exit(1.0, OrderType::Limit, Some(110.0), None), vec![0, 2]),
            (exit(2.0, OrderType::Limit, Some(120.0), None), vec![0, 1]),
        ],
    );
}

#[test]
fn any_other_ouo_groups_fill_reduces_each_resting_sibling_by_the_fill() {
    let last = bar_v(3_000, [100.0, 112.0, 99.0, 111.0], 1_000.0); // the 110 target only
    let engine = run(params(), all_linked, last);
    assert_eq!(engine.strategy.fills(), vec![(1, 100.0, 3.0), (-1, 110.0, 1.0)]);
    assert_eq!(
        book(engine.core.pending_of(SYM)),
        vec![(OrderKind::Stop, Some(95.0), 2.0), (OrderKind::Limit, Some(120.0), 1.0)],
        "1 off each (a spend would leave the 120 target at 2)"
    );
}

/// A plain OUO pair over the long of 3: a reduce-only stop of 1 at 95 and a reduce-only target of 3
/// at 110, each linking the other.
fn pair(b: &mut OrderBrokerSimulator) {
    let stop = (exit(1.0, OrderType::Stop, None, Some(95.0)), vec![1]);
    place_ouo(b, vec![stop, (exit(3.0, OrderType::Limit, Some(110.0), None), vec![0])]);
}

#[test]
fn a_plain_pair_hit_in_one_bar_fills_the_later_leg_from_its_size_before_the_cap() {
    let last = bar_v(3_000, [100.0, 111.0, 94.0, 100.0], 1_000.0); // the stop and the target
    let engine = run(params(), pair, last);
    // the cap: the stop first, takes 1, leaves the target 2; the stop's fill leaves the target
    // 3 - 1 = 2 of its 3, so it fills 2 (reckoned from the capped 2 it would fill 1)
    let fills = engine.strategy.fills();
    assert_eq!(fills, vec![(1, 100.0, 3.0), (-1, 95.0, 1.0), (-1, 110.0, 2.0)]);
    assert!(engine.core.pending_of(SYM).is_empty());
    assert_eq!(engine.core.position_of(SYM).size, 0.0);
}

#[test]
fn a_plain_pair_of_entries_in_one_bar_fills_the_later_ones_remainder_or_cancels_it() {
    // a buy stop of 1 at 105 and a sell stop of `sell` at 95, each linking the other; the bar's
    // high is nearer, so the buy fills first (the path order of two entries)
    for (sell, want) in
        [(1.0, vec![(1, 105.0, 1.0)]), (2.0, vec![(1, 105.0, 1.0), (-1, 95.0, 1.0)])]
    {
        let strategy = Once::new(move |b| {
            let buy = (leg(1, 1.0, OrderType::Stop, None, Some(105.0)), vec![1]);
            place_ouo(b, vec![buy, (leg(-1, sell, OrderType::Stop, None, Some(95.0)), vec![0])]);
        });
        let bars = vec![bar(1_000, FLAT), bar(2_000, [100.0, 106.0, 90.0, 100.0])];
        let engine = run_bars(params(), bars, strategy);
        let fills: Vec<(i32, f64, f64)> =
            engine.strategy.fills.iter().map(|f| (f.side, f.price, f.size)).collect();
        assert_eq!(fills, want, "sell of {sell}: 1 off it, the rest fills, or nothing is left");
        assert!(engine.core.pending_of(SYM).is_empty(), "sell of {sell}: nothing rests");
    }
}
