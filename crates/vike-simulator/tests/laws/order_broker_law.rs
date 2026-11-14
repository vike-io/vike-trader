//! `vike_model::OrderBroker` on `OrderBrokerSimulator` (decision 0269's first wave): every kind placed, amended and
//! cancelled by tag on the mounted symbol (`""`), a group's legs by theirs, and an order on the symbol it
//! names (decision 0277).
//!
//! What each test pins:
//!
//! - a tagged order of each kind fills EXACTLY as its untagged twin (the inherent verb) does, and its
//!   fill names its tag where the twin's names none; a pegged order and an iceberg take the emulation
//!   law exactly as their tagged twins (`submit_pegged_tagged`, `submit_limit_display_tagged`) do;
//! - an amend by tag moves a stop's trigger, a stop-limit's trigger and limit, and a size, as if the order
//!   had been placed so; a cancel by tag pulls it;
//! - a tagged group names each leg: an OCO pair's fill names its leg, and a cancel by tag reaches one leg
//!   and leaves the other resting;
//! - reduce-only is refused on a flat book in the core's words and capped to the position;
//! - a place under a tag whose order still works is refused by name (P1), and a cancel frees the tag;
//! - the average-price read is the position's, `0.0` flat;
//! - an amend and a cancel by tag take the untagged order's paths (decision 0269's W3c): under the
//!   opt-in latency gate each waits in flight, a cancel landing too late as its untagged twin's does and
//!   an amend reaching an order still in flight; under the opt-in queue model an amend up goes to the
//!   back of the queue and an amend down keeps its place; a spot sell raised past the holding is
//!   refused and keeps its terms;
//! - a trailing leg of a group fills as the trailing order placed alone, cancels its OCO sibling, and
//!   as an OTO child waits for its entry's fill;
//! - the pre-trade gate judges an amend as live's `gate_modify` does (`RiskGate::check_modify`), by tag
//!   at once, in flight under the latency gate, under the queue model, and on a tagged quote: one
//!   raised past a ceiling is refused and keeps its terms, one within it and one down pass, the
//!   amended order is not counted twice against buying power, and a run with no ceiling is unchanged.

use vike_marketdata::{Bar, L2Book, L2BookLevel, TickQuote, TickTrade};
use vike_model::{
    ContingencyType, Fill, OrderBroker, OrderEvent, OrderEventKind, OrderListLeg, OrderPlacement,
    OrderType, RiskLimits, Strategy, TimeInForce,
};
use vike_simulator::{
    LatencyModelKind, OrderBrokerSimulator, QueueModelKind, StrategyEngine, StrategyEngineConfig,
    TickEvent,
};

const SYM: &str = "TEST";
/// The live core's words for a reduce-only order that meets no position on its side.
const REFUSED: &str = "reduce-only: there is no position on its side to reduce";

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    vike_simulator::test_support::bar(ts, o, h, l, c, 100.0)
}

/// Bar 1 crosses 105 upward, bar 2 falls through 95, bar 3 comes back to 100.
fn bars() -> Vec<Bar> {
    vec![
        bar(1_000, 100.0, 101.0, 99.0, 100.0),
        bar(2_000, 100.0, 106.0, 99.0, 105.0),
        bar(3_000, 105.0, 105.0, 94.0, 95.0),
        bar(4_000, 95.0, 100.0, 95.0, 99.0),
        bar(5_000, 99.0, 100.0, 98.0, 99.0),
    ]
}

/// An untagged order, placed by its inherent verb.
type Verb = fn(&mut OrderBrokerSimulator);

/// One scripted action at one bar index.
#[derive(Clone)]
enum Act {
    Place(&'static str, i32, f64, OrderPlacement, bool),
    Group(Vec<(&'static str, OrderListLeg)>),
    Amend(&'static str, Option<f64>, Option<f64>, Option<f64>),
    Cancel(&'static str),
    /// an untagged order, by its inherent verb
    Twin(Verb),
}

/// Plays `script` (bar index, action) in order; collects fills, order ends and the average-price
/// read per bar.
struct Scripted {
    script: Vec<(usize, Act)>,
    fills: Vec<Fill>,
    avg: Vec<f64>,
    ends: Vec<OrderEvent>,
}

impl Strategy<OrderBrokerSimulator> for Scripted {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.avg.push(OrderBroker::average_price(b, ""));
        for (at, act) in self.script.clone() {
            if at != b.index {
                continue;
            }
            match act {
                Act::Place(tag, side, qty, order, reduce) => {
                    OrderBroker::place(b, "", tag, side, qty, order, reduce)
                }
                Act::Group(legs) => OrderBroker::place_group(
                    b,
                    "",
                    legs.into_iter().map(|(t, l)| (t.to_string(), l)).collect(),
                ),
                Act::Amend(tag, qty, price, trigger) => {
                    OrderBroker::amend(b, tag, qty, price, trigger)
                }
                Act::Cancel(tag) => OrderBroker::cancel(b, tag),
                Act::Twin(verb) => verb(b),
            }
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
    fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.ends.push(event.clone());
    }
}

fn run(script: Vec<(usize, Act)>) -> StrategyEngine<Scripted> {
    run_with(script, StrategyEngineConfig { cash: 100_000.0, ..StrategyEngineConfig::default() })
}

fn run_with(script: Vec<(usize, Act)>, params: StrategyEngineConfig) -> StrategyEngine<Scripted> {
    let mut engine = StrategyEngine::new(
        vec![(SYM.into(), bars())],
        Scripted { script, fills: Vec::new(), avg: Vec::new(), ends: Vec::new() },
        params,
    );
    engine.run();
    engine
}

/// The fills with their tags taken off, to compare a tagged run with its twin's.
fn untagged(fills: &[Fill]) -> Vec<Fill> {
    fills.iter().map(|f| Fill { tag: None, ..f.clone() }).collect()
}

fn tags(fills: &[Fill]) -> Vec<Option<&str>> {
    fills.iter().map(|f| f.tag.as_deref()).collect()
}

#[test]
fn a_tagged_order_fills_as_its_untagged_twin_and_its_fill_names_the_tag() {
    let cases: [(OrderPlacement, Verb); 6] = [
        (OrderPlacement::Market, |b| b.submit(SYM, 1, 1.0, 0.0, true, None)),
        (OrderPlacement::Limit { price: 95.0 }, |b| {
            OrderBrokerSimulator::submit_limit(b, SYM, 1, 1.0, 95.0, 0.0, true, None)
        }),
        (OrderPlacement::Stop { trigger: 105.0 }, |b| b.submit_stop(SYM, 1, 1.0, 105.0, 0.0, true)),
        (OrderPlacement::StopLimit { trigger: 105.0, limit: 106.0 }, |b| {
            b.submit_stop_limit(SYM, 1, 1.0, 105.0, 106.0, 0.0, true)
        }),
        (OrderPlacement::TakeProfit { trigger: 95.0, limit: None }, |b| {
            b.submit_take_profit(SYM, 1, 1.0, 95.0, None, 0.0, true)
        }),
        (OrderPlacement::Trailing { trail: 2.0, activation: Some(98.0) }, |b| {
            b.submit_trailing_activated(SYM, 1, 1.0, 2.0, 98.0, 0.0, true)
        }),
    ];
    for (order, twin) in cases {
        let tagged = run(vec![(0, Act::Place("e", 1, 1.0, order, false))]);
        let plain = run(vec![(0, Act::Twin(twin))]);
        assert!(!plain.strategy.fills.is_empty(), "{order:?}: the twin fills on this tape");
        assert_eq!(
            untagged(&tagged.strategy.fills),
            plain.strategy.fills,
            "{order:?}: the same fills as the twin"
        );
        assert!(tags(&tagged.strategy.fills).iter().all(|t| *t == Some("e")), "{order:?}");
        assert!(tags(&plain.strategy.fills).iter().all(Option::is_none), "{order:?}");
    }
}

/// An UNTAGGED seam order (tag `""`) is its inherent twin itself (decision 0277's rule 2): the same
/// fills, naming no tag, and no order event, so a strategy moved from `Broker::submit_market` onto
/// the seam backtests byte for byte; a kind with no untagged lane is refused into `dropped`, unheard.
#[test]
fn an_untagged_order_is_its_inherent_twin_and_is_heard_by_no_event() {
    let buy: Verb = |b| b.submit(SYM, 1, 1.0, 0.0, true, None);
    let cases: [(OrderPlacement, bool, Verb); 5] = [
        (OrderPlacement::Market, false, |b| b.submit(SYM, -1, 1.0, 0.0, true, None)),
        (OrderPlacement::Limit { price: 101.0 }, false, |b| {
            OrderBrokerSimulator::submit_limit(b, SYM, -1, 1.0, 101.0, 0.0, true, None)
        }),
        (OrderPlacement::Stop { trigger: 95.0 }, true, |b| {
            b.submit_stop_reduce(SYM, -1, 1.0, 95.0)
        }),
        (OrderPlacement::StopLimit { trigger: 95.0, limit: 94.5 }, false, |b| {
            b.submit_stop_limit(SYM, -1, 1.0, 95.0, 94.5, 0.0, true)
        }),
        (OrderPlacement::TakeProfit { trigger: 105.0, limit: None }, false, |b| {
            b.submit_take_profit(SYM, -1, 1.0, 105.0, None, 0.0, true)
        }),
    ];
    for (order, reduce, twin) in cases {
        // long 1 from bar 1's open, then the exit at bar 1
        let seam = run(vec![(0, Act::Twin(buy)), (1, Act::Place("", -1, 1.0, order, reduce))]);
        let plain = run(vec![(0, Act::Twin(buy)), (1, Act::Twin(twin))]);
        assert_eq!(plain.strategy.fills.len(), 2, "{order:?}: the twin's exit fills on this tape");
        assert_eq!(seam.strategy.fills, plain.strategy.fills, "{order:?}: the twin's fills");
        assert!(seam.strategy.ends.is_empty(), "{order:?}: {:?}", seam.strategy.ends);
        assert_eq!(seam.core.dropped, plain.core.dropped, "{order:?}");
    }
    let trail = OrderPlacement::Trailing { trail: 2.0, activation: None };
    let refused = run(vec![(0, Act::Place("", 1, 1.0, trail, false))]);
    assert!(refused.strategy.ends.is_empty() && refused.strategy.fills.is_empty());
    let reasons: Vec<&str> = refused.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, ["an untagged Trailing order has no lane: give it a tag"]);
}

/// A pegged order and an iceberg placed by tag take the ONE emulation law (decision 0275) on a venue
/// that holds neither (bybit): the same fills, under the same tag, and the same refusals as their
/// inherent tagged twins. A seam cancel ends the parent by its owner: the chase a bar run would end
/// in the law's words ends with none.
#[test]
fn a_tagged_peg_and_iceberg_take_the_emulation_law_as_their_twins_do() {
    let on_bybit = |script| {
        let p = StrategyEngineConfig {
            cash: 100_000.0,
            default_venue: Some("bybit".to_string()),
            ..StrategyEngineConfig::default()
        };
        run_with(script, p)
    };
    let peg = vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Best, offset: 0.0 };
    let cases: [(OrderPlacement, Verb); 2] = [
        (OrderPlacement::Iceberg { price: 95.0, display: 0.5 }, |b| {
            b.submit_limit_display_tagged("e", 1, 1.0, 95.0, 0.5)
        }),
        (OrderPlacement::Pegged { peg }, |b| {
            let peg =
                vike_model::OrderPeg { anchor: vike_model::OrderPegAnchor::Best, offset: 0.0 };
            b.submit_pegged_tagged("e", 1, 1.0, peg)
        }),
    ];
    for (order, twin) in cases {
        let tagged = on_bybit(vec![(0, Act::Place("e", 1, 1.0, order, false))]);
        let plain = on_bybit(vec![(0, Act::Twin(twin))]);
        assert_eq!(tagged.strategy.fills, plain.strategy.fills, "{order:?}: the twin's fills");
        assert_eq!(tagged.core.dropped, plain.core.dropped, "{order:?}: the twin's refusals");
    }
    let iceberg = OrderPlacement::Iceberg { price: 95.0, display: 0.5 };
    let sliced = on_bybit(vec![(0, Act::Place("e", 1, 1.0, iceberg, false))]);
    assert!(!sliced.strategy.fills.is_empty(), "the iceberg's slices fill on this tape");
    assert!(sliced.strategy.fills.iter().all(|f| f.size <= 0.5 && f.tag.as_deref() == Some("e")));
    let chase = OrderPlacement::Pegged { peg };
    let kept = on_bybit(vec![(0, Act::Place("e", 1, 1.0, chase, false))]);
    let pulled = on_bybit(vec![(0, Act::Place("e", 1, 1.0, chase, false)), (0, Act::Cancel("e"))]);
    assert!(!kept.core.dropped.is_empty(), "a bar run ends the chase in the law's words");
    assert!(pulled.core.dropped.is_empty(), "cancelled by its owner: {:?}", pulled.core.dropped);
}

#[test]
fn an_amend_by_tag_is_the_order_placed_so_and_a_cancel_pulls_it() {
    let stop = |trigger| OrderPlacement::Stop { trigger };
    // a stop at 110 never fires on this tape; amended to 105 and to a size of 2 it fills as one placed so
    let amended = run(vec![
        (0, Act::Place("s", 1, 1.0, stop(110.0), false)),
        (0, Act::Amend("s", Some(2.0), None, Some(105.0))),
    ]);
    let placed = run(vec![(0, Act::Place("s", 1, 2.0, stop(105.0), false))]);
    assert!(!placed.strategy.fills.is_empty());
    assert_eq!(amended.strategy.fills, placed.strategy.fills);

    // a stop's one price is its trigger: an amend naming only a price moves it too
    let by_price = run(vec![
        (0, Act::Place("s", 1, 2.0, stop(110.0), false)),
        (0, Act::Amend("s", None, Some(105.0), None)),
    ]);
    assert_eq!(by_price.strategy.fills, placed.strategy.fills);

    // a stop-limit takes a new trigger and a new limit
    let sl = |trigger, limit| OrderPlacement::StopLimit { trigger, limit };
    let amended = run(vec![
        (0, Act::Place("x", 1, 1.0, sl(110.0, 111.0), false)),
        (0, Act::Amend("x", None, Some(106.0), Some(105.0))),
    ]);
    let placed = run(vec![(0, Act::Place("x", 1, 1.0, sl(105.0, 106.0), false))]);
    assert!(!placed.strategy.fills.is_empty());
    assert_eq!(amended.strategy.fills, placed.strategy.fills);

    let cancelled =
        run(vec![(0, Act::Place("s", 1, 1.0, stop(105.0), false)), (0, Act::Cancel("s"))]);
    assert!(cancelled.strategy.fills.is_empty(), "{:?}", cancelled.strategy.fills);
    assert!(cancelled.core.symbol[0].pending.is_empty());
    // an unknown tag is a no-op
    let unknown = run(vec![
        (0, Act::Place("s", 1, 1.0, stop(105.0), false)),
        (0, Act::Cancel("other")),
        (0, Act::Amend("other", Some(9.0), None, Some(200.0))),
    ]);
    assert_eq!(unknown.strategy.fills, placed_stop_105().strategy.fills);
}

fn placed_stop_105() -> StrategyEngine<Scripted> {
    run(vec![(0, Act::Place("s", 1, 1.0, OrderPlacement::Stop { trigger: 105.0 }, false))])
}

/// A reduce-only sell leg of 1, linked OCO to `other`.
fn exit(
    order_type: OrderType,
    price: Option<f64>,
    trigger: Option<f64>,
    other: usize,
) -> OrderListLeg {
    OrderListLeg {
        side: -1,
        qty: 1.0,
        order_type,
        price,
        trigger_price: trigger,
        trail: None,
        reduce_only: true,
        time_in_force: TimeInForce::Gtc,
        parent: None,
        contingency: Some(ContingencyType::Oco),
        linked: vec![other],
        order_client_id: None,
    }
}

/// Long 1 at bar 1's open, then at bar 1 a stop at 95 (`sl`) and a target at 110 (`tp`), one cancelling
/// the other; then `more`.
fn guarded(more: Vec<(usize, Act)>) -> StrategyEngine<Scripted> {
    let guard = Act::Group(vec![
        ("sl", exit(OrderType::Stop, None, Some(95.0), 1)),
        ("tp", exit(OrderType::Limit, Some(110.0), None, 0)),
    ]);
    let mut script = vec![(0, Act::Place("e", 1, 1.0, OrderPlacement::Market, false)), (1, guard)];
    script.extend(more);
    run(script)
}

#[test]
fn a_tagged_groups_legs_are_named_and_reached_by_tag() {
    let engine = guarded(Vec::new());
    assert_eq!(
        tags(&engine.strategy.fills),
        vec![Some("e"), Some("sl")],
        "{:?}",
        engine.strategy.fills
    );
    assert_eq!(engine.core.position_of(SYM).size, 0.0, "the stop closed the long");
    assert!(engine.core.symbol[0].pending.is_empty(), "and its OCO target went with it");

    // the stop pulled by its tag: the long stays, guarded by the target alone
    let engine = guarded(vec![(1, Act::Cancel("sl"))]);
    assert_eq!(tags(&engine.strategy.fills), vec![Some("e")]);
    assert_eq!(engine.core.position_of(SYM).size, 1.0);
    assert_eq!(engine.core.symbol[0].pending.len(), 1, "the target still rests");
    let engine = guarded(vec![(1, Act::Cancel("sl")), (2, Act::Cancel("tp"))]);
    assert!(engine.core.symbol[0].pending.is_empty(), "and goes by its own tag");
}

#[test]
fn reduce_only_is_refused_flat_and_capped_to_the_position() {
    let flat =
        run(vec![(0, Act::Place("x", -1, 1.0, OrderPlacement::Stop { trigger: 95.0 }, true))]);
    assert!(flat.strategy.fills.is_empty());
    assert!(flat.core.dropped.iter().any(|d| d.reason == REFUSED), "{:?}", flat.core.dropped);

    let capped = run(vec![
        (0, Act::Place("e", 1, 1.0, OrderPlacement::Market, false)),
        (1, Act::Place("x", -1, 5.0, OrderPlacement::Market, true)),
    ]);
    let last = capped.strategy.fills.last().expect("the close filled");
    assert_eq!((last.side, last.size, last.tag.as_deref()), (-1, 1.0, Some("x")));
    assert_eq!(capped.core.position_of(SYM).size, 0.0, "flat, not short");
}

#[test]
fn a_place_under_a_busy_tag_is_refused_and_a_cancel_frees_it() {
    // P1 (the owner's ruling of 2026-11-10): the older order keeps its tag and fills as it would
    // alone; the second place is refused by name.
    let stop = |trigger| OrderPlacement::Stop { trigger };
    let alone = run(vec![(0, Act::Place("s", 1, 1.0, stop(105.0), false))]);
    let engine = run(vec![
        (0, Act::Place("s", 1, 1.0, stop(105.0), false)),
        (0, Act::Place("s", 1, 1.0, stop(104.0), false)),
    ]);
    assert_eq!(
        engine.strategy.fills, alone.strategy.fills,
        "the older order alone fills, as alone"
    );
    assert_eq!(tags(&engine.strategy.fills), vec![Some("s")]);
    let words = "s is busy: amend it, or cancel it first";
    assert!(engine.core.dropped.iter().any(|d| d.reason == words), "{:?}", engine.core.dropped);
    // the strategy hears it as the tagged `Denied` a maker or an executor slot adopts its side on
    let busy: Vec<_> = engine.strategy.ends.iter().filter(|e| e.is_tag_busy_refusal()).collect();
    assert_eq!(busy.len(), 1, "{:?}", engine.strategy.ends);
    assert_eq!(busy[0].tag.as_deref(), Some("s"));

    // a cancel frees the tag at once: the place after it in the same hook rests and fills
    let later = run(vec![(0, Act::Place("s", 1, 1.0, stop(104.0), false))]);
    let freed = run(vec![
        (0, Act::Place("s", 1, 1.0, stop(105.0), false)),
        (0, Act::Cancel("s")),
        (0, Act::Place("s", 1, 1.0, stop(104.0), false)),
    ]);
    assert_eq!(freed.strategy.fills, later.strategy.fills, "the newer order fills as alone");
    assert!(freed.core.dropped.iter().all(|d| d.reason != words));
}

#[test]
fn the_average_price_read_is_the_positions() {
    let engine = guarded(Vec::new());
    // bar 0: flat; bar 1: long 1 from bar 1's open; bar 2: the stop closed it
    let entry = engine.strategy.fills[0].price;
    assert_eq!(engine.strategy.avg[..3], [0.0, entry, 0.0]);
}

/// Trades the second symbol of a two-symbol run by name (decision 0277), once per step.
#[derive(Default)]
struct OnSecond {
    acted: Vec<usize>,
    fills: Vec<Fill>,
    ends: Vec<OrderEvent>,
    avg: Vec<(f64, f64)>,
}

impl Strategy<OrderBrokerSimulator> for OnSecond {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        if self.acted.contains(&b.index) {
            return;
        }
        self.acted.push(b.index);
        self.avg.push((OrderBroker::average_price(b, "B"), OrderBroker::average_price(b, "")));
        let rest = OrderPlacement::Limit { price: 900.0 };
        match b.index {
            0 => {
                OrderBroker::place(b, "B", "b", 1, 1.0, rest, false);
                OrderBroker::place(b, "B", "c", 1, 1.0, rest, false);
                OrderBroker::place(b, "NOPE", "x", 1, 1.0, rest, false);
            }
            1 => {
                OrderBroker::amend(b, "b", None, Some(950.0), None);
                OrderBroker::cancel(b, "c");
            }
            _ => {}
        }
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
    fn on_order_event(&mut self, _b: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.ends.push(event.clone());
    }
}

/// A seam order trades the symbol it names: on a two-symbol run it rests in that symbol's book, fills
/// on its bars under its tag and is amended and cancelled there by tag, while the mounted symbol
/// (`""`) stays flat; a symbol the run does not hold is refused by name, never traded as another.
#[test]
fn a_seam_order_trades_the_symbol_it_names() {
    // B is the first tape at ten times the price: 900 is never reached, 950 is at bar 2
    let b_bars: Vec<Bar> = bars()
        .iter()
        .map(|x| bar(x.timestamp, x.open * 10.0, x.high * 10.0, x.low * 10.0, x.close * 10.0))
        .collect();
    let mut engine = StrategyEngine::new(
        vec![(SYM.into(), bars()), ("B".into(), b_bars)],
        OnSecond::default(),
        StrategyEngineConfig { cash: 100_000.0, ..StrategyEngineConfig::default() },
    );
    engine.run();
    let fills = &engine.strategy.fills;
    assert_eq!(fills.len(), 1, "{fills:?}");
    assert_eq!((fills[0].symbol.as_str(), fills[0].tag.as_deref()), ("B", Some("b")));
    assert_eq!((fills[0].price, fills[0].size), (950.0, 1.0), "the amended limit filled");
    assert_eq!(
        engine.strategy.avg.last(),
        Some(&(950.0, 0.0)),
        "B is held, the mounted symbol flat"
    );
    let end =
        |tag: &str| engine.strategy.ends.iter().find(|e| e.tag.as_deref() == Some(tag)).cloned();
    assert!(matches!(end("c").map(|e| e.kind), Some(OrderEventKind::Canceled { .. })));
    let words = "NOPE is not a symbol of this run";
    assert_eq!(end("x").map(|e| e.kind), Some(OrderEventKind::Denied { reason: words.into() }));
    assert!(engine.core.dropped.iter().any(|d| d.symbol == "NOPE" && d.reason == words));
}

/// What each `OrderBroker` read answers for `""` and for the mounted symbol by name, per step.
#[derive(Default)]
struct Reads {
    rows: Vec<[(f64, f64, usize); 2]>,
}

impl Strategy<OrderBrokerSimulator> for Reads {
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        let read = |s: &str| {
            (OrderBroker::position(b, s), OrderBroker::price(b, s), OrderBroker::bars(b, s).len())
        };
        self.rows.push([read(""), read(SYM)]);
        if b.index == 0 {
            OrderBroker::place(b, "", "e", 1, 1.0, OrderPlacement::Market, false);
        }
    }
}

/// `""` is the mounted symbol (the run's first) on every `OrderBroker` read too, as the live broker reads
/// it (decision 0277's rule 1): the position, the price and the bars of `SYM`, never the second
/// symbol's. It panicked as an unknown symbol before.
#[test]
fn the_empty_symbol_reads_the_mounted_symbol() {
    let b_bars: Vec<Bar> = bars()
        .iter()
        .map(|x| bar(x.timestamp, x.open * 10.0, x.high * 10.0, x.low * 10.0, x.close * 10.0))
        .collect();
    let mut engine = StrategyEngine::new(
        vec![(SYM.into(), bars()), ("B".into(), b_bars)],
        Reads::default(),
        StrategyEngineConfig { cash: 100_000.0, ..StrategyEngineConfig::default() },
    );
    engine.run();
    let rows = &engine.strategy.rows;
    assert!(rows.iter().any(|[own, _]| own.0 != 0.0), "the market order filled: {rows:?}");
    assert!(rows.iter().all(|[empty, named]| empty == named), "{rows:?}");
}

// ---- an amend and a cancel by tag on the untagged order's paths (decision 0269's W3c) ----

/// One action at the tick stamped `ts` (a book, a quote or a trade), on the tick lane.
type TickAct = (i64, fn(&mut OrderBrokerSimulator));

/// Plays each [`TickAct`] on its tick and records every fill.
struct TickScript {
    acts: Vec<TickAct>,
    fills: Vec<Fill>,
}

impl TickScript {
    fn at(&mut self, b: &mut OrderBrokerSimulator) {
        for (ts, act) in self.acts.clone() {
            if ts == b.now {
                act(b);
            }
        }
    }
}

impl Strategy<OrderBrokerSimulator> for TickScript {
    fn on_order_book(&mut self, b: &mut OrderBrokerSimulator, _book: &L2Book) {
        self.at(b);
    }
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.at(b);
    }
    fn on_trade_tick(&mut self, b: &mut OrderBrokerSimulator, _t: &TickTrade) {
        self.at(b);
    }
    fn on_fill(&mut self, _b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.fills.push(fill.clone());
    }
}

fn run_ticks(
    acts: Vec<TickAct>,
    ticks: Vec<TickEvent>,
    params: StrategyEngineConfig,
) -> StrategyEngine<TickScript> {
    let script = TickScript { acts, fills: Vec::new() };
    let mut engine = StrategyEngine::new(vec![(SYM.into(), Vec::new())], script, params);
    engine.run_ticks(&[(SYM.to_string(), ticks)]);
    engine
}

/// Quotes well above 99, and one trade at 1_040 that touches a buy resting at 99.
fn touch_tape() -> Vec<TickEvent> {
    let q = |ts| vike_simulator::test_support::quote(SYM, ts, 100.0, 100.02, 100.0, 100.0);
    let touch = vike_simulator::test_support::trade(SYM, 1_040, 99.0, 5.0);
    vec![q(1_000), q(1_020), q(1_030), touch, q(1_050), q(1_100)]
}

/// A run under the latency gate with a 15 ms entry leg, or with none.
fn latency(on: bool) -> StrategyEngineConfig {
    let latency_model = on.then(|| LatencyModelKind::constant(15_000_000, 0));
    StrategyEngineConfig { cash: 100_000.0, latency_model, ..StrategyEngineConfig::default() }
}

#[test]
fn under_the_latency_gate_a_cancel_by_tag_lands_too_late_as_its_untagged_twins_does() {
    let tagged: Vec<TickAct> = vec![
        (1_000, |b| {
            OrderBroker::place(b, "", "b", 1, 1.0, OrderPlacement::Limit { price: 99.0 }, false)
        }),
        (1_030, |b| OrderBroker::cancel(b, "b")),
    ];
    let twin: Vec<TickAct> = vec![
        (1_000, |b| OrderBrokerSimulator::submit_limit(b, SYM, 1, 1.0, 99.0, 0.0, true, None)),
        (1_030, |b| b.cancel_all(SYM)),
    ];
    // the order lands at 1_015, the cancel sent at 1_030 at 1_045: after the touch at 1_040
    let t = run_ticks(tagged.clone(), touch_tape(), latency(true));
    let p = run_ticks(twin.clone(), touch_tape(), latency(true));
    assert_eq!(p.strategy.fills.len(), 1, "the twin's cancel lands too late");
    assert_eq!(untagged(&t.strategy.fills), p.strategy.fills, "the same fill as the twin");
    assert_eq!(tags(&t.strategy.fills), vec![Some("b")], "and it names its order");
    // with no latency each cancel lands at once and nothing fills
    assert!(run_ticks(tagged, touch_tape(), latency(false)).strategy.fills.is_empty());
    assert!(run_ticks(twin, touch_tape(), latency(false)).strategy.fills.is_empty());
}

#[test]
fn under_the_latency_gate_an_amend_by_tag_reaches_an_order_still_in_flight_and_waits_its_turn() {
    // placed at 95 and amended to 99 on one tick: both in flight, the amend lands after the order
    let same_tick: Vec<TickAct> = vec![(1_000, |b| {
        OrderBroker::place(b, "", "b", 1, 1.0, OrderPlacement::Limit { price: 95.0 }, false);
        OrderBroker::amend(b, "b", None, Some(99.0), None);
    })];
    let engine = run_ticks(same_tick, touch_tape(), latency(true));
    let got: Vec<_> = engine.strategy.fills.iter().map(|f| (f.price, f.tag.as_deref())).collect();
    assert_eq!(got, vec![(99.0, Some("b"))], "filled at the amended price");

    // amended at 1_030, the new price lands at 1_045: the touch at 1_040 still meets 95
    let late: Vec<TickAct> = vec![
        (1_000, |b| {
            OrderBroker::place(b, "", "b", 1, 1.0, OrderPlacement::Limit { price: 95.0 }, false)
        }),
        (1_030, |b| OrderBroker::amend(b, "b", None, Some(99.0), None)),
    ];
    assert!(run_ticks(late.clone(), touch_tape(), latency(true)).strategy.fills.is_empty());
    let at_once = run_ticks(late, touch_tape(), latency(false));
    assert_eq!(at_once.strategy.fills.len(), 1, "with no latency it lands at once");
}

/// A bid level 10 deep at 99 ahead of the order, then trades of 4 and 5 at 99 that grind that front
/// to 1, so the trade of 3 at ts 4 has an excess of 2: an order that kept its place fills there, one
/// at the back of the level does not.
fn ground_down_front() -> Vec<TickEvent> {
    let snap = TickEvent::Book(vike_marketdata::L2BookUpdate {
        timestamp: 1,
        local_timestamp: 0,
        sequence: 1,
        kind: vike_marketdata::L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(99.0, 10.0)],
        asks: vec![L2BookLevel::new(101.0, 5.0)],
        symbol: SYM.to_string(),
    });
    let tr = |ts, size| vike_simulator::test_support::trade(SYM, ts, 99.0, size);
    vec![snap, tr(2, 4.0), tr(3, 5.0), tr(4, 3.0)]
}

#[test]
fn under_the_queue_model_an_amend_up_by_tag_goes_to_the_back_and_an_amend_down_keeps_its_place() {
    let queued = || StrategyEngineConfig {
        queue_model: Some(QueueModelKind::RiskAverse),
        ..Default::default()
    };
    let rest: TickAct = (1, |b| {
        OrderBroker::place(b, "", "b", 1, 2.0, OrderPlacement::Limit { price: 99.0 }, false)
    });
    let filled = |e: &StrategyEngine<TickScript>| -> Vec<(f64, f64, Option<String>)> {
        e.strategy.fills.iter().map(|f| (f.size, f.price, f.tag.clone())).collect()
    };
    let hold = run_ticks(vec![rest], ground_down_front(), queued());
    assert_eq!(filled(&hold), vec![(2.0, 99.0, Some("b".into()))], "control: the aged order fills");

    let up: TickAct = (3, |b| OrderBroker::amend(b, "b", Some(3.0), None, None));
    let engine = run_ticks(vec![rest, up], ground_down_front(), queued());
    assert!(engine.strategy.fills.is_empty(), "an amend up forfeits the place it had");
    assert_eq!(engine.core.symbol[0].pending[0].size, 3.0, "and rests at its new size");
    let pull: TickAct = (4, |b| OrderBroker::cancel(b, "b"));
    let engine = run_ticks(vec![rest, up, pull], ground_down_front(), queued());
    assert!(engine.core.symbol[0].pending.is_empty(), "its tag followed it to its new identity");

    let down: TickAct = (3, |b| OrderBroker::amend(b, "b", Some(1.0), None, None));
    let engine = run_ticks(vec![rest, down], ground_down_front(), queued());
    assert_eq!(
        filled(&engine),
        vec![(1.0, 99.0, Some("b".into()))],
        "an amend down keeps its place"
    );
}

#[test]
fn a_spot_sell_raised_by_tag_past_the_holding_is_refused_and_keeps_its_terms() {
    let spot = |raise| {
        let script = vec![
            (0, Act::Place("e", 1, 2.0, OrderPlacement::Market, false)),
            (1, Act::Place("x", -1, 1.0, OrderPlacement::Limit { price: 200.0 }, false)),
            (2, Act::Amend("x", Some(raise), None, None)),
        ];
        let p = StrategyEngineConfig {
            cash: 100_000.0,
            cash_symbols: vec![SYM.into()],
            ..Default::default()
        };
        run_with(script, p)
    };
    let past = spot(3.0);
    let why = vike_execution::risk::cash_oversell_refusal(2.0, 0.0, None, 3.0)
        .expect("past the long of 2");
    let reasons: Vec<&str> = past.core.dropped.iter().map(|d| d.reason.as_str()).collect();
    assert_eq!(reasons, vec![why.as_str()], "in the live engine's words, its own size left out");
    assert_eq!(past.core.symbol[0].pending[0].size, 1.0, "it keeps its terms");

    let within = spot(2.0);
    assert!(within.core.dropped.is_empty(), "{:?}", within.core.dropped);
    assert_eq!(within.core.symbol[0].pending[0].size, 2.0, "raised within the long");
}

// ---- a trailing leg of a group (decision 0269's W3c) ----

/// A reduce-only trailing sell of 1 by 2, no activation, its links as stated.
fn trailing_exit(parent: Option<usize>, linked: Vec<usize>) -> OrderListLeg {
    OrderListLeg {
        order_type: OrderType::Trailing,
        trail: Some(2.0),
        parent,
        contingency: (!linked.is_empty()).then_some(ContingencyType::Oco),
        linked,
        ..exit(OrderType::Trailing, None, None, 0)
    }
}

#[test]
fn a_trailing_leg_in_an_oco_fills_as_placed_alone_and_cancels_its_sibling() {
    let trail = OrderPlacement::Trailing { trail: 2.0, activation: None };
    let entry = || (0, Act::Place("e", 1, 1.0, OrderPlacement::Market, false));
    let alone = run(vec![entry(), (1, Act::Place("tr", -1, 1.0, trail, true))]);
    let group = Act::Group(vec![
        ("tr", trailing_exit(None, vec![1])),
        ("tp", exit(OrderType::Limit, Some(110.0), None, 0)),
    ]);
    let grouped = run(vec![entry(), (1, group)]);
    assert_eq!(tags(&alone.strategy.fills), vec![Some("e"), Some("tr")], "it trails out on bar 2");
    assert_eq!(grouped.strategy.fills, alone.strategy.fills, "the leg fills as placed alone");
    assert!(grouped.core.symbol[0].pending.is_empty(), "and its target went with it");
}

#[test]
fn a_trailing_leg_in_an_oto_waits_for_its_entry_and_then_trails() {
    let list = |price| {
        let entry = OrderListLeg {
            side: 1,
            reduce_only: false,
            contingency: None,
            linked: Vec::new(),
            ..exit(OrderType::Limit, Some(price), None, 0)
        };
        Act::Group(vec![("e", entry), ("tr", trailing_exit(Some(0), Vec::new()))])
    };
    // the entry at 100 fills at bar 1's open; the exit it arms trails the long out as bar 2 falls
    let engine = run(vec![(0, list(100.0))]);
    let got: Vec<_> =
        engine.strategy.fills.iter().map(|f| (f.side, f.size, f.tag.as_deref())).collect();
    assert_eq!(got, vec![(1, 1.0, Some("e")), (-1, 1.0, Some("tr"))]);
    assert_eq!(engine.core.position_of(SYM).size, 0.0);

    // an entry that never fills never arms it: nothing fills, and only the entry rests
    let engine = run(vec![(0, list(50.0))]);
    assert!(engine.strategy.fills.is_empty(), "{:?}", engine.strategy.fills);
    assert_eq!(engine.core.symbol[0].pending.len(), 1, "the trailing child is held off the book");
}

// ---- the pre-trade gate judges an amend, as live's `gate_modify` does ----

/// A cap of `cap` on every order's notional, or no ceiling at all.
fn capped(cap: Option<f64>) -> StrategyEngineConfig {
    let risk_limits =
        cap.map(|c| RiskLimits { max_notional_per_order: Some(c), ..RiskLimits::new() });
    StrategyEngineConfig { cash: 100_000.0, risk_limits, ..StrategyEngineConfig::default() }
}

fn reasons<S: Strategy<OrderBrokerSimulator>>(engine: &StrategyEngine<S>) -> Vec<&str> {
    engine.core.dropped.iter().map(|d| d.reason.as_str()).collect()
}

/// A buy limit of 1 at 90 (this tape never trades that low) placed on bar 0 and amended on bar 1.
fn amended(amend: Act, cap: Option<f64>) -> StrategyEngine<Scripted> {
    let place = Act::Place("b", 1, 1.0, OrderPlacement::Limit { price: 90.0 }, false);
    run_with(vec![(0, place), (1, amend)], capped(cap))
}

fn terms(engine: &StrategyEngine<Scripted>) -> (f64, Option<f64>) {
    let o = &engine.core.symbol[0].pending[0];
    (o.size, o.price)
}

#[test]
fn an_amend_by_tag_past_the_ceiling_is_refused_and_keeps_its_terms() {
    // 1 x 90 rests under a cap of 150; raised to 2 (180) or re-priced to 160 it is over it
    for amend in [Act::Amend("b", Some(2.0), None, None), Act::Amend("b", None, Some(160.0), None)]
    {
        let engine = amended(amend, Some(150.0));
        assert_eq!(reasons(&engine), ["over-max-notional"], "the live gate's own words");
        assert_eq!(terms(&engine), (1.0, Some(90.0)), "it keeps its terms");
        assert!(engine.strategy.fills.is_empty(), "{:?}", engine.strategy.fills);
    }
    // within the cap, and down, it passes
    let within = amended(Act::Amend("b", Some(1.5), None, None), Some(150.0));
    assert!(within.core.dropped.is_empty(), "{:?}", within.core.dropped);
    assert_eq!(terms(&within), (1.5, Some(90.0)));
    let down = amended(Act::Amend("b", Some(0.5), None, None), Some(150.0));
    assert!(down.core.dropped.is_empty(), "{:?}", down.core.dropped);
    assert_eq!(terms(&down), (0.5, Some(90.0)));
    // no ceiling: the amend is taken as before
    let free = amended(Act::Amend("b", Some(2.0), None, None), None);
    assert!(free.core.dropped.is_empty(), "{:?}", free.core.dropped);
    assert_eq!(terms(&free), (2.0, Some(90.0)));
}

#[test]
fn an_amended_order_is_judged_at_its_new_terms_and_not_counted_twice() {
    // Equity 100 at an initial margin of 1: the buy of 0.9 at 90 rests (81), and on bar 1 it is
    // worth 94.5 at the mark. Counted as well as its new size, every amend here would be refused.
    let margin = |amend: f64| {
        let limits = RiskLimits { initial_margin_requirement: Some(1.0), ..RiskLimits::new() };
        let p = StrategyEngineConfig {
            cash: 100.0,
            risk_limits: Some(limits),
            ..StrategyEngineConfig::default()
        };
        let place = Act::Place("b", 1, 0.9, OrderPlacement::Limit { price: 90.0 }, false);
        run_with(vec![(0, place), (1, Act::Amend("b", Some(amend), None, None))], p)
    };
    let down = margin(0.8);
    assert!(down.core.dropped.is_empty(), "an amend down passes: {:?}", down.core.dropped);
    assert_eq!(terms(&down), (0.8, Some(90.0)));
    let within = margin(1.1);
    assert!(within.core.dropped.is_empty(), "99 of 100: {:?}", within.core.dropped);
    let up = margin(1.2);
    assert_eq!(reasons(&up), ["insufficient-margin"], "108 of 100");
    assert_eq!(terms(&up), (0.9, Some(90.0)), "it keeps its terms");
}

#[test]
fn a_tagged_quote_amended_past_the_ceiling_is_refused_and_keeps_its_terms() {
    let quoted = |amend: Verb, cap| {
        let place: Verb =
            |b| b.place("", "q", 1, 1.0, OrderPlacement::Limit { price: 90.0 }, false);
        // the maker law's quote (`StrategyEngineConfig::optimistic_quotes`)
        let p = StrategyEngineConfig { optimistic_quotes: true, ..capped(cap) };
        run_with(vec![(0, Act::Twin(place)), (1, Act::Twin(amend))], p)
    };
    let quote = |engine: &StrategyEngine<Scripted>| {
        let tagged = engine.core.tagged_of(SYM);
        let (_, o) = tagged.iter().find(|(t, _)| *t == "q").expect("the quote rests");
        (o.size, o.price)
    };
    let past = quoted(|b| b.amend("q", Some(2.0), None, None), Some(150.0));
    assert_eq!(reasons(&past), ["over-max-notional"]);
    assert_eq!(quote(&past), (1.0, Some(90.0)), "it keeps its terms");
    let repriced = quoted(|b| b.amend("q", None, Some(160.0), None), Some(150.0));
    assert_eq!(reasons(&repriced), ["over-max-notional"]);
    assert_eq!(quote(&repriced), (1.0, Some(90.0)));
    let within = quoted(|b| b.amend("q", Some(1.5), None, None), Some(150.0));
    assert!(within.core.dropped.is_empty(), "{:?}", within.core.dropped);
    assert_eq!(quote(&within), (1.5, Some(90.0)));
    let down = quoted(|b| b.amend("q", Some(0.5), None, None), Some(150.0));
    assert_eq!(quote(&down), (0.5, Some(90.0)));
    let free = quoted(|b| b.amend("q", Some(2.0), None, None), None);
    assert!(free.core.dropped.is_empty(), "{:?}", free.core.dropped);
    assert_eq!(quote(&free), (2.0, Some(90.0)));
}

#[test]
fn under_the_latency_gate_an_amend_is_judged_when_it_is_sent() {
    // Placed at 99 and raised to 2 on one tick, both in flight: the touch at 1_040 fills the order as
    // the account last sent it, 1 when the raise past the cap was refused at its send, 2 within it.
    let acts: Vec<TickAct> = vec![(1_000, |b| {
        OrderBroker::place(b, "", "b", 1, 1.0, OrderPlacement::Limit { price: 99.0 }, false);
        OrderBroker::amend(b, "b", Some(2.0), None, None);
    })];
    let gated =
        |cap| StrategyEngineConfig { latency_model: latency(true).latency_model, ..capped(cap) };
    let sizes = |e: &StrategyEngine<TickScript>| -> Vec<f64> {
        e.strategy.fills.iter().map(|f| f.size).collect()
    };
    let past = run_ticks(acts.clone(), touch_tape(), gated(Some(150.0)));
    assert_eq!(reasons(&past), ["over-max-notional"]);
    assert_eq!(sizes(&past), [1.0], "the order as it was");
    let within = run_ticks(acts.clone(), touch_tape(), gated(Some(250.0)));
    assert!(within.core.dropped.is_empty(), "{:?}", within.core.dropped);
    assert_eq!(sizes(&within), [2.0]);
    assert_eq!(sizes(&run_ticks(acts, touch_tape(), gated(None))), [2.0], "no ceiling");
}

#[test]
fn under_the_queue_model_an_amend_up_refused_by_the_gate_keeps_its_place() {
    let queued =
        |cap| StrategyEngineConfig { queue_model: Some(QueueModelKind::RiskAverse), ..capped(cap) };
    let rest: TickAct = (1, |b| {
        OrderBroker::place(b, "", "b", 1, 2.0, OrderPlacement::Limit { price: 99.0 }, false)
    });
    let up: TickAct = (3, |b| OrderBroker::amend(b, "b", Some(3.0), None, None));
    let sizes = |e: &StrategyEngine<TickScript>| -> Vec<f64> {
        e.strategy.fills.iter().map(|f| f.size).collect()
    };
    // 3 x 99 is over a cap of 250: refused, the order keeps its size and its place, and fills
    let refused = run_ticks(vec![rest, up], ground_down_front(), queued(Some(250.0)));
    assert_eq!(reasons(&refused), ["over-max-notional"]);
    assert_eq!(sizes(&refused), [2.0], "it kept the place it had");
    // admitted (within 300, or with no ceiling) it goes to the back, as before
    for cap in [Some(300.0), None] {
        let engine = run_ticks(vec![rest, up], ground_down_front(), queued(cap));
        assert!(engine.strategy.fills.is_empty(), "{cap:?}: {:?}", engine.strategy.fills);
        assert_eq!(engine.core.symbol[0].pending[0].size, 3.0);
    }
}
