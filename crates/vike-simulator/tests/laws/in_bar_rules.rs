//! The simulator's in-bar fill rules (`vike_simulator::InBarRules`, decision 0269 item 5), each pinned ON against OFF on a tape
//! small enough to reason about: every rule is off by default, and each test shows what the one it names changes.
//!
//! - `path_walk`: a bar's stop and target fill in the order its path reaches them, stamped with the bar;
//! - `fill_at_close`, and `OrderBrokerSimulator::place_at_close` without it: a market order fills at the close of the bar that placed
//!   it, stamped with that bar;
//! - `price_order`: two stops in one stretch fill nearer first; an exit's stop goes ahead of its target at an open past both;
//! - `ties_in_placement_order`: the stops of three exits on one level fill in the order they were placed, not the exit with
//!   the lowest target first;
//! - `queue_order`: a new buy goes ahead of a resting sell; an exit's if-touched leg ahead of its limit;
//! - `committed_stops`: a lone exit stop gapped with a reversing entry fills after it, and still does when raised
//!   under the opt-in queue model, which gives it a new identity its exit follows;
//! - `trail_from_fill`: a trailing order placed on a fill waits for the walk, not the bar before's close;
//! - `reduce_only_at_fill`: a reduce-only stop placed flat rests until a position meets it.

use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy};
use vike_simulator::{
    InBarRules, OrderBrokerSimulator, QueueModelKind, StrategyEngine, StrategyEngineConfig,
};

const SYM: &str = "TEST";
const BAR: i64 = 60_000;

fn bar(k: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(k * BAR, o, h, l, c, 100.0)
}

fn flat(k: i64, px: f64) -> Bar {
    bar(k, px, px, px, px)
}

/// One action at one bar index, or on the first fill inside one.
#[derive(Clone, Copy)]
enum Act {
    Place(&'static str, i32, f64, OrderPlacement, bool),
    Exit(&'static str, &'static str, i32, f64, OrderPlacement),
    AtClose(&'static str, i32, f64),
    /// a new size by tag
    Resize(&'static str, f64),
}

fn act(b: &mut OrderBrokerSimulator, a: Act) {
    match a {
        Act::Place(tag, side, qty, order, reduce) => {
            OrderBroker::place(b, "", tag, side, qty, order, reduce)
        }
        Act::Exit(exit, tag, side, qty, order) => b.place_exit_leg("", exit, tag, side, qty, order),
        Act::AtClose(tag, side, qty) => b.place_at_close("", tag, side, qty, false),
        Act::Resize(tag, qty) => OrderBroker::amend(b, tag, Some(qty), None, None),
    }
}

struct Scripted {
    on_bar: Vec<(usize, Act)>,
    on_fill: Vec<Act>,
    fills: Vec<Fill>,
}

impl Strategy<OrderBrokerSimulator> for Scripted {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        for (at, a) in self.on_bar.clone() {
            if at == b.index {
                act(b, a);
            }
        }
    }

    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
        for a in std::mem::take(&mut self.on_fill) {
            act(b, a);
        }
    }
}

fn run(
    bars: Vec<Bar>,
    rules: InBarRules,
    on_bar: Vec<(usize, Act)>,
    on_fill: Vec<Act>,
) -> StrategyEngine<Scripted> {
    run_with(
        bars,
        StrategyEngineConfig { in_bar: rules, ..StrategyEngineConfig::default() },
        on_bar,
        on_fill,
    )
}

fn run_with(
    bars: Vec<Bar>,
    params: StrategyEngineConfig,
    on_bar: Vec<(usize, Act)>,
    on_fill: Vec<Act>,
) -> StrategyEngine<Scripted> {
    let params = StrategyEngineConfig { cash: 1_000_000.0, ..params };
    let strategy = Scripted { on_bar, on_fill, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), bars)], strategy, params);
    engine.run();
    engine
}

/// `(tag, side, price, ts)` of each fill.
fn fills(engine: &StrategyEngine<Scripted>) -> Vec<(Option<String>, i32, f64, i64)> {
    engine.strategy.fills.iter().map(|f| (f.tag.clone(), f.side, f.price, f.timestamp)).collect()
}

fn tags(engine: &StrategyEngine<Scripted>) -> Vec<String> {
    engine.strategy.fills.iter().map(|f| f.tag.clone().unwrap_or_default()).collect()
}

const WALK: InBarRules = InBarRules {
    path_walk: true,
    fill_at_close: false,
    price_order: false,
    ties_in_placement_order: false,
    queue_order: false,
    committed_stops: false,
    trail_from_fill: false,
    reduce_only_at_fill: false,
};

const LONG: Act = Act::Place("long", 1, 1.0, OrderPlacement::Market, false);

#[test]
fn every_rule_is_off_by_default() {
    assert_eq!(StrategyEngineConfig::default().in_bar, InBarRules::default());
    assert_eq!(InBarRules::default(), InBarRules { path_walk: false, ..WALK });
}

#[test]
fn the_path_walk_fills_a_stop_and_a_target_in_the_order_the_path_reaches_them() {
    // Long from bar 1's open; bar 2 is nearer its high, so its path meets the target before the stop.
    let bars =
        vec![flat(0, 100.0), flat(1, 100.0), bar(2, 100.0, 104.0, 94.0, 99.0), flat(3, 99.0)];
    let exits = vec![
        (0, LONG),
        (1, Act::Place("stop", -1, 1.0, OrderPlacement::Stop { trigger: 95.0 }, true)),
        (1, Act::Place("take", -1, 1.0, OrderPlacement::Limit { price: 103.0 }, true)),
    ];
    let walked = run(bars.clone(), WALK, exits.clone(), vec![]);
    assert_eq!(
        fills(&walked)[1],
        (Some("take".into()), -1, 103.0, 2 * BAR),
        "the high comes first"
    );
    assert_eq!(walked.core.intrabar_both_hit, 0, "the walk observes the order");
    let coarse = run(bars, InBarRules::default(), exits, vec![]);
    assert_eq!(
        coarse.strategy.fills[1].tag.as_deref(),
        Some("stop"),
        "the coarse bar's adverse guess"
    );
}

#[test]
fn a_market_order_fills_at_the_close_of_the_bar_that_placed_it() {
    let bars = vec![flat(0, 100.0), bar(1, 102.0, 103.0, 101.0, 102.0), flat(2, 102.0)];
    let at_close = InBarRules { fill_at_close: true, ..InBarRules::default() };
    let now = run(bars.clone(), at_close, vec![(0, LONG)], vec![]);
    assert_eq!(
        fills(&now),
        [(Some("long".into()), 1, 100.0, 0)],
        "bar 0's close, stamped with bar 0"
    );
    let next = run(bars.clone(), InBarRules::default(), vec![(0, LONG)], vec![]);
    assert_eq!(fills(&next), [(Some("long".into()), 1, 102.0, BAR)], "the next open without it");
    // One order at this close, the other at the next open, with the rule off.
    let both = vec![
        (0, Act::AtClose("now", 1, 1.0)),
        (0, Act::Place("later", 1, 1.0, OrderPlacement::Market, false)),
    ];
    let mixed = run(bars, WALK, both, vec![]);
    assert_eq!(
        fills(&mixed),
        [(Some("now".into()), 1, 100.0, 0), (Some("later".into()), 1, 102.0, BAR)],
        "only the order placed for this close meets it"
    );
}

/// The shared-cash lane (`cash_gate`, `decide = "simultaneous"`) keeps `fill_at_close` too (review FIX, finding 5):
/// its admission phase meets the close of the bar that placed the order at the head of the next step, as the bar
/// lane walks it, stamped with that bar; without the rule the order fills at the next open there as well.
#[test]
fn the_shared_cash_lane_fills_a_market_order_at_the_close_of_the_bar_that_placed_it() {
    let bars = vec![flat(0, 100.0), bar(1, 102.0, 103.0, 101.0, 102.0), flat(2, 102.0)];
    let at_close = InBarRules { fill_at_close: true, ..InBarRules::default() };
    let lanes: [fn() -> StrategyEngineConfig; 2] = [
        || StrategyEngineConfig { cash_gate: true, ..StrategyEngineConfig::default() },
        || StrategyEngineConfig {
            decide: vike_model::run_request::decide::DecideMode::Simultaneous,
            ..StrategyEngineConfig::default()
        },
    ];
    for lane in lanes {
        let gated = |in_bar| {
            let params = StrategyEngineConfig { in_bar, ..lane() };
            run_with(bars.clone(), params, vec![(0, LONG)], vec![])
        };
        assert_eq!(
            fills(&gated(at_close)),
            [(Some("long".into()), 1, 100.0, 0)],
            "bar 0's close, stamped with bar 0"
        );
        assert_eq!(
            fills(&gated(InBarRules::default())),
            [(Some("long".into()), 1, 102.0, BAR)],
            "the next open without it"
        );
    }
}

#[test]
fn the_price_order_fills_the_nearer_of_two_stops_in_one_stretch_first() {
    // Long 2; two free sell stops, the farther placed first; bar 2 falls through both in one stretch.
    let bars =
        vec![flat(0, 100.0), flat(1, 100.0), bar(2, 100.0, 100.0, 90.0, 91.0), flat(3, 91.0)];
    let script = vec![
        (0, Act::Place("long", 1, 2.0, OrderPlacement::Market, false)),
        (1, Act::Place("far", -1, 0.5, OrderPlacement::Stop { trigger: 94.0 }, false)),
        (1, Act::Place("near", -1, 1.5, OrderPlacement::Stop { trigger: 96.0 }, false)),
    ];
    let ordered =
        run(bars.clone(), InBarRules { price_order: true, ..WALK }, script.clone(), vec![]);
    assert_eq!(tags(&ordered), ["long", "near", "far"]);
    assert_eq!(
        tags(&run(bars, WALK, script, vec![])),
        ["long", "far", "near"],
        "placement order without it"
    );
}

#[test]
fn the_price_order_puts_an_exits_stop_ahead_of_its_target_at_an_open_past_both() {
    // A long's exit with its stop (103) above its target (102), and an open between them.
    let bars = vec![flat(0, 100.0), flat(1, 100.0), flat(2, 102.5), flat(3, 102.5)];
    let legs = |exit: bool| {
        let leg = |tag, order| match exit {
            true => Act::Exit("x", tag, -1, 1.0, order),
            false => Act::Place(tag, -1, 1.0, order, true),
        };
        vec![
            (0, LONG),
            (1, leg("take", OrderPlacement::Limit { price: 102.0 })),
            (1, leg("stop", OrderPlacement::Stop { trigger: 103.0 })),
        ]
    };
    let ordered = InBarRules { price_order: true, ..WALK };
    assert_eq!(
        tags(&run(bars.clone(), ordered, legs(true), vec![])),
        ["long", "stop"],
        "one exit: its stop first"
    );
    assert_eq!(
        tags(&run(bars, ordered, legs(false), vec![])),
        ["long", "take"],
        "two exits: the lower level first"
    );
}

#[test]
fn exits_tied_on_one_level_fill_in_the_order_they_were_placed() {
    // Short 1 from bar 1's open; three exits share a buy stop at 105, their targets 99, 98 and 96, placed in that order;
    // bar 2 rises through the stop and never reaches a target.
    let bars =
        vec![flat(0, 100.0), flat(1, 100.0), bar(2, 100.0, 106.0, 100.0, 105.5), flat(3, 105.5)];
    let mut script = vec![(0, Act::Place("short", -1, 1.0, OrderPlacement::Market, false))];
    for (exit, take, stop, target) in [
        ("a", "a take", "a stop", 99.0),
        ("b", "b take", "b stop", 98.0),
        ("c", "c take", "c stop", 96.0),
    ] {
        script.push((1, Act::Exit(exit, take, 1, 0.3, OrderPlacement::Limit { price: target })));
        script.push((1, Act::Exit(exit, stop, 1, 0.3, OrderPlacement::Stop { trigger: 105.0 })));
    }
    let ordered = InBarRules { price_order: true, ..WALK };
    let placed = InBarRules { ties_in_placement_order: true, ..ordered };
    assert_eq!(
        tags(&run(bars.clone(), placed, script.clone(), vec![])),
        ["short", "a stop", "b stop", "c stop"]
    );
    assert_eq!(
        tags(&run(bars, ordered, script, vec![])),
        ["short", "c stop", "b stop", "a stop"],
        "without it each exit ranks at its lowest level, its target"
    );
}

#[test]
fn the_queue_order_puts_a_new_buy_ahead_of_a_resting_sell() {
    // A sell stop and then a buy stop at one level, both through bar 1's open.
    let bars = vec![flat(0, 100.0), flat(1, 100.0), flat(2, 100.0)];
    let script = vec![
        (0, Act::Place("sell", -1, 1.0, OrderPlacement::Stop { trigger: 100.0 }, false)),
        (0, Act::Place("buy", 1, 1.0, OrderPlacement::Stop { trigger: 100.0 }, false)),
    ];
    let queued =
        run(bars.clone(), InBarRules { queue_order: true, ..WALK }, script.clone(), vec![]);
    assert_eq!(tags(&queued), ["buy", "sell"]);
    assert_eq!(
        tags(&run(bars, WALK, script, vec![])),
        ["sell", "buy"],
        "placement order without it"
    );
}

#[test]
fn the_queue_order_puts_an_exits_if_touched_leg_ahead_of_its_limit() {
    // A long's exit: its target placed first, then an if-touched leg at a nearer level; bar 2 rises through both in one
    // stretch, so the queue decides which fills.
    let bars =
        vec![flat(0, 100.0), flat(1, 100.0), bar(2, 100.0, 104.0, 100.0, 104.0), flat(3, 104.0)];
    let script = vec![
        (0, LONG),
        (1, Act::Exit("x", "take", -1, 1.0, OrderPlacement::Limit { price: 103.0 })),
        (
            1,
            Act::Exit(
                "x",
                "touch",
                -1,
                1.0,
                OrderPlacement::TakeProfit { trigger: 102.0, limit: Some(102.0) },
            ),
        ),
    ];
    let queued =
        run(bars.clone(), InBarRules { queue_order: true, ..WALK }, script.clone(), vec![]);
    assert_eq!(fills(&queued)[1], (Some("touch".into()), -1, 102.0, 2 * BAR));
    assert_eq!(tags(&run(bars, WALK, script, vec![]))[1], "take", "placement order without it");
}

#[test]
fn a_lone_stop_gapped_with_a_reversing_entry_is_committed_to_that_open() {
    // Long 1 with its exit's stop at 101 and a sell stop entry of 2 at 101; bar 3 opens at 98, through both.
    let bars = vec![flat(0, 100.0), flat(1, 100.0), flat(2, 100.0), flat(3, 98.0), flat(4, 98.0)];
    let script = vec![
        (0, LONG),
        (2, Act::Exit("x", "stop", -1, 1.0, OrderPlacement::Stop { trigger: 101.0 })),
        (2, Act::Place("short", -1, 2.0, OrderPlacement::Stop { trigger: 101.0 }, false)),
    ];
    let ordered = InBarRules { price_order: true, ..WALK };
    let committed =
        run(bars.clone(), InBarRules { committed_stops: true, ..ordered }, script.clone(), vec![]);
    assert_eq!(
        tags(&committed),
        ["long", "short", "stop"],
        "the entry first, then the committed stop"
    );
    assert_eq!(committed.core.position(SYM), -2.0, "the stop opened its size the other way");
    let plain = run(bars, ordered, script, vec![]);
    assert_eq!(tags(&plain), ["long", "short"], "reduce-only, the stop finds nothing to reduce");
    assert_eq!(plain.core.position(SYM), -1.0);
}

#[test]
fn an_exit_leg_raised_under_the_queue_model_stays_a_leg_of_its_exit() {
    // The committed stop's tape with the stop raised to 1.5 before bar 3's open: under the opt-in queue model the raise
    // gives the order a new identity, and its exit follows it, so it is still that exit's lone stop and is committed.
    let bars = vec![flat(0, 100.0), flat(1, 100.0), flat(2, 100.0), flat(3, 98.0), flat(4, 98.0)];
    let script = vec![
        (0, LONG),
        (2, Act::Exit("x", "stop", -1, 1.0, OrderPlacement::Stop { trigger: 101.0 })),
        (2, Act::Place("short", -1, 2.0, OrderPlacement::Stop { trigger: 101.0 }, false)),
        (2, Act::Resize("stop", 1.5)),
    ];
    let in_bar = InBarRules { committed_stops: true, price_order: true, ..WALK };
    for queue_model in [None, Some(QueueModelKind::RiskAverse)] {
        let params =
            StrategyEngineConfig { in_bar, queue_model, ..StrategyEngineConfig::default() };
        let engine = run_with(bars.clone(), params, script.clone(), vec![]);
        assert_eq!(tags(&engine), ["long", "short", "stop"], "{queue_model:?}");
        assert_eq!(engine.core.position(SYM), -2.5, "{queue_model:?}: 1.5 the other way");
    }
}

#[test]
fn a_trailing_order_placed_on_a_fill_waits_for_the_walk() {
    // Bar 0 closes at 103; bar 1 dips (its high nearer) and a buy limit at 99 fills on the way down. On that fill a sell
    // trailing stop from 102 is placed: the bar before's close is past 102, the rest of bar 1's walk is not.
    let bars = vec![flat(0, 103.0), bar(1, 100.0, 100.5, 97.0, 98.0), flat(2, 98.0)];
    let script = vec![(0, Act::Place("buy", 1, 1.0, OrderPlacement::Limit { price: 99.0 }, false))];
    let trail = Act::Place(
        "trail",
        -1,
        1.0,
        OrderPlacement::Trailing { trail: 0.5, activation: Some(102.0) },
        true,
    );
    let walked = run(
        bars.clone(),
        InBarRules { trail_from_fill: true, ..WALK },
        script.clone(),
        vec![trail],
    );
    assert_eq!(tags(&walked), ["buy"], "never activated on the walk");
    let marked = run(bars, WALK, script, vec![trail]);
    assert_eq!(
        tags(&marked),
        ["buy", "trail"],
        "activated by the bar before's close, it fires in bar 1"
    );
}

#[test]
fn a_reduce_only_stop_placed_flat_rests_until_a_position_meets_it() {
    // Flat at bar 0: a reduce-only sell stop at 95, then a long from bar 1's open; bar 2 falls through 95.
    let bars =
        vec![flat(0, 100.0), flat(1, 100.0), bar(2, 100.0, 100.0, 94.0, 94.0), flat(3, 94.0)];
    let script = vec![
        (0, Act::Place("stop", -1, 1.0, OrderPlacement::Stop { trigger: 95.0 }, true)),
        (0, LONG),
    ];
    let at_fill =
        run(bars.clone(), InBarRules { reduce_only_at_fill: true, ..WALK }, script.clone(), vec![]);
    assert_eq!(tags(&at_fill), ["long", "stop"]);
    let at_place = run(bars, WALK, script, vec![]);
    assert_eq!(
        tags(&at_place),
        ["long"],
        "refused when placed, with nothing on its side to reduce"
    );
    assert!(at_place.core.dropped.iter().any(|d| d.reason.starts_with("reduce-only")));
}
