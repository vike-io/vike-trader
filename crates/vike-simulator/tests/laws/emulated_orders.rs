//! Decision 0275's EMULATED orders, simulator side (`crates/vike-simulator/src/engine/sim_broker/emulated.rs`): a
//! pegged order (`OrderBrokerSimulator::submit_pegged_tagged`) and a visible size (`OrderBrokerSimulator::submit_limit_display_tagged`)
//! the run's venue does not hold, worked through ONE tagged child at a time by `vike_execution::emulated`'s law.
//!
//! The scenarios are the deleted real-engine executor suite's (vike-strategy's tests/executors at commit
//! 2e653252b9) that were not the core's alone, re-expressed through the order kind, each tape asserting the SAME
//! children (place, re-price, cancel; sizes and prices). The knobs the order kind dropped (a refresh threshold, a max
//! chase, a timeout) and the typed grid have no twin. The slot probe's "a new child rests after a fill" is a new
//! parent the strategy sends from `on_fill`, which works on that same event. Beside them: the two iceberg bar runs
//! the law's move could not keep, a strategy's OTHER order counting nothing toward a parent, a visible size the
//! venue shows itself keeping the old full-size law byte for byte, and an amend refused in words.
//!
//! The children are read off the book's tagged orders (`OrderBrokerSimulator::tagged_of`) at every hook of the owning strategy and at
//! every fill, as the old suite's tap read them: a new tag is a place, a moved price a re-price, a tag gone without
//! a fill a cancel. A peg runs on binance (no pegged order there), a visible size on bybit (binance shows one).

use std::sync::Arc;

use vike_execution::emulated::{BAR_RUN_REFUSAL, modify_refusal};
use vike_marketdata::{
    Bar, L2Book, L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade,
};
use vike_model::{
    Fill, OrderBroker, OrderDrop, OrderPeg, OrderPegAnchor, OrderPlacement, Strategy,
    SymbolProperties,
};
use vike_simulator::{
    FillModelKind, OrderBrokerSimulator, QueueModelKind, StrategyEngine, StrategyEngineConfig,
    TickEvent,
};

pub(crate) const SYM: &str = "BTCUSDT";
/// The strategy's tag for its one parent.
const TAG: &str = "order";
/// The replay book's tick.
const TICK: f64 = 0.5;
/// A venue that holds no pegged order: the simulator emulates one.
const PEG_VENUE: &str = "binance";
/// A venue that shows no visible size: the simulator emulates one (binance shows it natively).
pub(crate) const SHOWN_VENUE: &str = "bybit";

/// One child order as the venue sees it (`emulated_one_law.rs` reads the live core's venue log in these words).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Child {
    Place { side: i32, qty: f64, price: f64 },
    Reprice { price: f64 },
    Cancel,
}
use Child::Cancel;

pub(crate) fn place(side: i32, qty: f64, price: f64) -> Child {
    Child::Place { side, qty, price }
}

pub(crate) fn reprice(price: f64) -> Child {
    Child::Reprice { price }
}

type Act = Box<dyn Fn(&mut OrderBrokerSimulator)>;

/// The owner of the parent: it sends it at the start (and, with `resend`, again after each fill), acts on the
/// `n`th touch (a quote or a book) as told, and logs every child the tagged lane shows and every fill it hears.
pub(crate) struct Owner {
    send: Act,
    resend: bool,
    on_touch: Vec<(usize, Act)>,
    touches: usize,
    seen: Vec<(String, i32, f64, Option<f64>)>,
    pub(crate) log: Vec<Child>,
    pub(crate) fills: Vec<(i64, f64, f64)>,
    fill_tags: Vec<Option<String>>,
}

impl Owner {
    pub(crate) fn new(send: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Self {
        Self {
            send: Box::new(send),
            resend: false,
            on_touch: Vec::new(),
            touches: 0,
            seen: Vec::new(),
            log: Vec::new(),
            fills: Vec::new(),
            fill_tags: Vec::new(),
        }
    }

    pub(crate) fn resend(mut self) -> Self {
        self.resend = true;
        self
    }

    fn on_touch(mut self, n: usize, act: impl Fn(&mut OrderBrokerSimulator) + 'static) -> Self {
        self.on_touch.push((n, Box::new(act)));
        self
    }

    /// Cancel the parent on touch `n`.
    pub(crate) fn pull_on(self, n: usize) -> Self {
        self.on_touch(n, |b| OrderBroker::cancel(b, TAG))
    }

    /// Log what changed on the tagged lane since the last look. At a fill (`filling`) a tag gone is the
    /// fill's and a child may shrink; nothing else re-sizes one.
    fn observe(&mut self, b: &OrderBrokerSimulator, filling: bool) {
        let tagged = b.tagged_of(SYM).into_iter();
        let now: Vec<_> = tagged.map(|(t, o)| (t.to_string(), o.side, o.size, o.price)).collect();
        let gone = self.seen.iter().filter(|(t, ..)| !now.iter().any(|n| &n.0 == t)).count();
        if !filling {
            self.log.extend(std::iter::repeat_n(Cancel, gone));
        }
        for (tag, side, size, price) in &now {
            let price = price.expect("a child is a plain limit");
            match self.seen.iter().find(|(t, ..)| t == tag) {
                None => self.log.push(place(*side, *size, price)),
                Some((_, _, was, was_price)) => {
                    let kept = if filling { size <= was } else { size == was };
                    assert!(kept, "a child is never re-sized: {tag:?} {was} -> {size}");
                    if *was_price != Some(price) {
                        self.log.push(reprice(price));
                    }
                }
            }
        }
        self.seen = now;
    }

    fn touch(&mut self, b: &mut OrderBrokerSimulator) {
        self.observe(b, false);
        self.touches += 1;
        for (n, act) in &self.on_touch {
            if *n == self.touches {
                act(b);
            }
        }
        self.observe(b, false);
    }
}

impl Strategy<OrderBrokerSimulator> for Owner {
    fn on_start(&mut self, b: &mut OrderBrokerSimulator) {
        (self.send)(b);
        self.observe(b, false);
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, _bar: &Bar) {
        self.observe(b, false);
    }
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, _q: &TickQuote) {
        self.touch(b);
    }
    fn on_order_book(&mut self, b: &mut OrderBrokerSimulator, _book: &L2Book) {
        self.touch(b);
    }
    fn on_trade_tick(&mut self, b: &mut OrderBrokerSimulator, _t: &TickTrade) {
        self.observe(b, false);
    }
    fn on_fill(&mut self, b: &mut OrderBrokerSimulator, fill: &Fill) {
        self.observe(b, true);
        self.fills.push((fill.timestamp, fill.price, fill.size));
        self.fill_tags.push(fill.tag.clone());
        if self.resend {
            (self.send)(b);
        }
    }
    fn on_stop(&mut self, b: &mut OrderBrokerSimulator) {
        self.observe(b, false);
    }
}

fn peg(
    side: i32,
    qty: f64,
    anchor: OrderPegAnchor,
    offset: f64,
) -> impl Fn(&mut OrderBrokerSimulator) + 'static {
    move |b: &mut OrderBrokerSimulator| {
        b.submit_pegged_tagged(TAG, side, qty, OrderPeg { anchor, offset })
    }
}

/// A buy pegged to its own best price: the chase the executor suite's `buy(qty)` was.
pub(crate) fn buy_at_best(qty: f64) -> impl Fn(&mut OrderBrokerSimulator) + 'static {
    peg(1, qty, OrderPegAnchor::Best, 0.0)
}

fn shown(
    side: i32,
    total: f64,
    visible: f64,
    price: f64,
) -> impl Fn(&mut OrderBrokerSimulator) + 'static {
    move |b: &mut OrderBrokerSimulator| {
        b.submit_limit_display_tagged(TAG, side, total, price, visible)
    }
}

/// The iceberg suite's `BUY`: 2.5 showing 1.0.
pub(crate) fn iceberg_buy(price: f64) -> impl Fn(&mut OrderBrokerSimulator) + 'static {
    shown(1, 2.5, 1.0, price)
}

/// The iceberg suite's three slices of 2.5 at `price`.
fn slices(price: f64) -> Vec<Child> {
    vec![place(1, 1.0, price), place(1, 1.0, price), place(1, 0.5, price)]
}

pub(crate) fn q(ts: i64, bid: f64, ask: f64) -> TickEvent {
    vike_simulator::test_support::quote(SYM, ts, bid, ask, 1.0, 1.0)
}

fn tr(ts: i64, price: f64, size: f64) -> TickEvent {
    let symbol = SYM.to_string();
    TickEvent::Trade(TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size,
        is_buyer_maker: true,
        symbol,
    })
}

/// A one-level book snapshot of `(price, size)` a side.
fn book(ts: i64, bid: (f64, f64), ask: (f64, f64)) -> TickEvent {
    TickEvent::Book(L2BookUpdate {
        timestamp: ts,
        local_timestamp: 0,
        sequence: ts as u64,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: TICK,
        bids: vec![L2BookLevel::new(bid.0, bid.1)],
        asks: vec![L2BookLevel::new(ask.0, ask.1)],
        symbol: SYM.to_string(),
    })
}

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    vike_simulator::test_support::bar(ts, open, high, low, close, 1.0)
}

/// `quote(1, 99.5, 100.5)`, then a quote every ts to 8 that crosses a buy at 100.
fn crossing() -> Vec<TickEvent> {
    std::iter::once(q(1, 99.5, 100.5)).chain((2..=8).map(|ts| q(ts, 99.0, 99.5))).collect()
}

/// A run on `venue`, on the bar lane unless a lane says otherwise.
fn on(venue: &str) -> StrategyEngineConfig {
    StrategyEngineConfig {
        default_venue: Some(venue.to_string()),
        ..StrategyEngineConfig::default()
    }
}

pub(crate) fn lane(fill_model: FillModelKind, venue: &str) -> StrategyEngineConfig {
    StrategyEngineConfig { fill_model, ..on(venue) }
}

/// The tick lane under the queue model.
fn queued(venue: &str) -> StrategyEngineConfig {
    StrategyEngineConfig {
        queue_model: Some(QueueModelKind::RiskAverse),
        ..lane(FillModelKind::Tick, venue)
    }
}

/// `p` with the instrument data `properties` at every instant.
fn on_grid(p: StrategyEngineConfig, properties_value: SymbolProperties) -> StrategyEngineConfig {
    StrategyEngineConfig {
        properties: Some(Arc::new(move |_: &str, _: &str, _: i64| Some(properties_value))),
        ..p
    }
}

fn grid(tick_size: f64, min_qty: f64, min_notional: f64) -> SymbolProperties {
    SymbolProperties {
        tick_size,
        step_size: 0.01,
        min_qty,
        min_notional,
        ..SymbolProperties::default()
    }
}

pub(crate) fn run(
    owner: Owner,
    tape: Vec<TickEvent>,
    p: StrategyEngineConfig,
) -> StrategyEngine<Owner> {
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), Vec::new())], owner, p);
    engine.run_ticks(&[(SYM.to_string(), tape)]);
    engine
}

fn run_bars(owner: Owner, bars: Vec<Bar>, p: StrategyEngineConfig) -> StrategyEngine<Owner> {
    let mut engine = StrategyEngine::new(vec![(SYM.to_string(), bars)], owner, p);
    engine.run();
    engine
}

fn assert_nothing_rests(engine: &StrategyEngine<Owner>) {
    // the children rest in `pending` too, quotes of the maker law
    let st = &engine.core.symbol[0];
    assert!(st.pending.is_empty(), "{:?}", st.pending);
}

/// The one refusal of a run, its words containing `words`, of `qty`.
fn assert_refused(engine: &StrategyEngine<Owner>, words: &str, qty: f64) {
    let [OrderDrop { symbol: symbol_name, reason: why, size, .. }] = &engine.core.dropped[..]
    else {
        panic!("one refusal, not {:?}", engine.core.dropped)
    };
    assert!(symbol_name == SYM && why.contains(words) && *size == qty, "{words}: {why} ({size})");
}

/// The equivalence suite's five touches, and the child they give a buy at its own best pulled on the fifth.
const TOUCHES: [(f64, f64); 5] =
    [(100.0, 101.0), (100.5, 101.0), (101.0, 101.5), (100.5, 101.5), (100.5, 101.0)];

fn followed() -> Vec<Child> {
    vec![place(1, 1.0, 100.0), reprice(100.5), reprice(101.0), reprice(100.5), Cancel]
}

/// The touches as quotes on the tick lane. The amend sent on touch 2 is refused in the live core's words and
/// moves no child.
#[test]
fn the_same_quotes_give_the_same_child_orders_as_the_core() {
    let tape = TOUCHES.iter().zip(1..).map(|(&(bid, ask), ts)| q(ts, bid, ask)).collect();
    let owner = Owner::new(buy_at_best(1.0))
        .on_touch(2, |b| OrderBroker::amend(b, TAG, None, Some(50.0), None))
        .pull_on(5);
    let engine = run(owner, tape, lane(FillModelKind::Tick, PEG_VENUE));
    assert_eq!(engine.strategy.log, followed());
    assert!(engine.strategy.fills.is_empty());
    assert_refused(&engine, &modify_refusal(TAG), 0.0);
    assert_eq!(engine.core.dropped[0].reason, modify_refusal(TAG));
    assert_nothing_rests(&engine);
}

/// A peg and a visible size placed THROUGH the order seam, amended through it: each amend is refused in
/// `dropped` in the live core's words, as live refuses it, and the run is otherwise the run without it.
#[test]
fn an_amend_on_the_seam_of_an_emulated_parent_is_refused_in_the_cores_words() {
    let peg =
        OrderPlacement::Pegged { peg: OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 } };
    let iceberg = OrderPlacement::Iceberg { price: 100.0, display: 1.0 };
    for (order, venue) in [(peg, PEG_VENUE), (iceberg, SHOWN_VENUE)] {
        let send = move |b: &mut OrderBrokerSimulator| {
            OrderBroker::place(b, "", TAG, 1, 2.5, order, false)
        };
        let tape = || -> Vec<TickEvent> {
            TOUCHES.iter().zip(1..).map(|(&(bid, ask), ts)| q(ts, bid, ask)).collect()
        };
        let plain = run(Owner::new(send), tape(), lane(FillModelKind::Tick, venue));
        let amended = Owner::new(send)
            .on_touch(2, |b| OrderBroker::amend(b, TAG, Some(5.0), Some(50.0), None));
        let engine = run(amended, tape(), lane(FillModelKind::Tick, venue));
        let words = modify_refusal(TAG);
        let (refused, rest): (Vec<OrderDrop>, Vec<OrderDrop>) =
            engine.core.dropped.iter().cloned().partition(|d| d.reason == words);
        assert_eq!(refused.len(), 1, "{order:?}: {:?}", engine.core.dropped);
        assert!(refused[0].symbol == SYM && refused[0].size == 0.0, "{order:?}: {:?}", refused[0]);
        assert_eq!(rest, plain.core.dropped, "{order:?}");
        assert!(!plain.strategy.log.is_empty(), "{order:?}: a child rests on this tape");
        assert_eq!(engine.strategy.log, plain.strategy.log, "{order:?}: no child moved");
        assert_eq!(engine.strategy.fills, plain.strategy.fills, "{order:?}");
    }
}

/// The touches as books of 5.0 a side on the L2 lane.
#[test]
fn the_same_books_give_the_same_child_orders_as_the_core() {
    let tape = TOUCHES.iter().zip(1..).map(|(&(bid, ask), ts)| book(ts, (bid, 5.0), (ask, 5.0)));
    let owner = Owner::new(buy_at_best(1.0)).pull_on(5);
    let engine = run(owner, tape.collect(), lane(FillModelKind::L2Book, PEG_VENUE));
    assert_eq!(engine.strategy.log, followed());
    assert!(engine.strategy.fills.is_empty());
    assert_nothing_rests(&engine);
}

/// The child follows the bid, fills at the ask that crossed it (under the PARENT's tag), and the parent sent
/// again from the fill rests its child on that same quote; it is pulled on touch 6.
#[test]
fn on_the_tick_lane_the_child_follows_the_bid_fills_by_the_tick_law_and_a_new_one_rests_after() {
    let tape = vec![
        q(1, 100.0, 101.0),
        q(2, 100.5, 101.0),
        q(3, 100.5, 101.0),
        q(4, 100.0, 100.5),
        q(5, 99.5, 100.5),
        q(6, 99.5, 100.5),
    ];
    let owner = Owner::new(buy_at_best(1.0)).resend().pull_on(6);
    let engine = run(owner, tape, lane(FillModelKind::Tick, PEG_VENUE));
    let want = [place(1, 1.0, 100.0), reprice(100.5), place(1, 1.0, 100.0), reprice(99.5), Cancel];
    assert_eq!(engine.strategy.log, want);
    assert_eq!(engine.strategy.fills, [(4, 100.5, 1.0)]);
    assert_eq!(engine.strategy.fill_tags, [Some(TAG.to_string())], "the fill names the parent");
    assert_eq!(engine.core.maker_fills, 1);
    assert_eq!(engine.core.symbol[0].pos.size, 1.0);
    assert_nothing_rests(&engine);
}

/// On the L2 lane a passive child at its own best fills nothing from a print at its price; an aggressive one
/// (offset 1.0, at the ask) fills from the print and the parent sent again rests at the next touch.
#[test]
fn on_the_l2_lane_a_child_at_its_own_best_fills_only_when_the_book_crosses_it() {
    let passive = vec![
        book(1, (100.0, 5.0), (101.0, 5.0)),
        tr(2, 100.0, 3.0),
        book(3, (100.5, 5.0), (101.0, 5.0)),
        tr(4, 100.5, 3.0),
        book(5, (100.5, 5.0), (101.0, 5.0)),
    ];
    let owner = Owner::new(buy_at_best(1.0)).pull_on(3);
    let engine = run(owner, passive, lane(FillModelKind::L2Book, PEG_VENUE));
    assert_eq!(engine.strategy.log, [place(1, 1.0, 100.0), reprice(100.5), Cancel]);
    assert!(engine.strategy.fills.is_empty());
    assert_eq!(engine.core.maker_fills + engine.core.taker_fills, 0);

    let aggressive = vec![
        book(1, (100.0, 5.0), (101.0, 5.0)),
        tr(2, 101.0, 1.0),
        book(3, (100.5, 5.0), (101.5, 5.0)),
        book(4, (100.5, 5.0), (101.5, 5.0)),
    ];
    let owner = Owner::new(peg(1, 1.0, OrderPegAnchor::Best, 1.0)).resend().pull_on(3);
    let engine = run(owner, aggressive, lane(FillModelKind::L2Book, PEG_VENUE));
    assert_eq!(engine.strategy.log, [place(1, 1.0, 101.0), place(1, 1.0, 101.5), Cancel]);
    assert_eq!(engine.strategy.fills, [(2, 101.0, 1.0)]);
    assert_eq!(engine.core.symbol[0].pos.size, 1.0);
}

/// Under the queue model the passive child fills once the 2.0 ahead of it has traded (the second print); the
/// plain tick lane fills it on the first. The children are the same on both.
#[test]
fn with_the_queue_model_a_passive_child_fills_once_the_size_ahead_of_it_has_traded() {
    let tape = || {
        vec![
            book(1, (100.0, 2.0), (101.0, 5.0)),
            tr(2, 100.0, 1.0),
            tr(3, 100.0, 2.0),
            book(4, (100.5, 2.0), (101.0, 5.0)),
            book(5, (100.5, 2.0), (101.0, 5.0)),
        ]
    };
    let want = [place(1, 1.0, 100.0), place(1, 1.0, 100.5), Cancel];
    for (p, fill_timestamp) in [(queued(PEG_VENUE), 3), (lane(FillModelKind::Tick, PEG_VENUE), 2)] {
        let engine = run(Owner::new(buy_at_best(1.0)).resend().pull_on(3), tape(), p);
        assert_eq!(engine.strategy.log, want);
        assert_eq!(engine.strategy.fills, [(fill_timestamp, 100.0, 1.0)]);
    }
}

/// The instrument's grid snaps the child: a bid of 100.3 rests the buy at 100.0, one of 100.8 moves it to 100.5.
#[test]
fn the_chase_works_to_the_instruments_grid() {
    let tape = vec![q(1, 100.3, 101.0), q(2, 100.8, 101.3)];
    let p = on_grid(lane(FillModelKind::Tick, PEG_VENUE), grid(TICK, 0.0, 0.0));
    let engine = run(Owner::new(buy_at_best(2.0)), tape, p);
    assert_eq!(engine.strategy.log, [place(1, 2.0, 100.0), reprice(100.5)]);
    assert!(engine.core.dropped.is_empty());
}

/// A child under the instrument's minimum notional or size never goes out, and the parent ends in the law's words.
#[test]
fn a_child_the_instrument_refuses_never_goes_out() {
    for (properties, words) in
        [(grid(TICK, 0.0, 500.0), "minimum 500"), (grid(TICK, 3.0, 0.0), "minimum size 3")]
    {
        let tape = vec![q(1, 100.0, 101.0), q(2, 100.5, 101.0)];
        let p = on_grid(lane(FillModelKind::Tick, PEG_VENUE), properties);
        let engine = run(Owner::new(buy_at_best(2.0)), tape, p);
        assert!(engine.strategy.log.is_empty(), "{words}: {:?}", engine.strategy.log);
        assert_refused(&engine, words, 2.0);
        assert_nothing_rests(&engine);
    }
}

/// A bar has no book: a pegged order on a bar run places nothing and is dropped in the law's words.
#[test]
fn on_bars_a_pegged_order_places_nothing_and_says_why() {
    let bars = [60_000, 120_000, 180_000].map(|ts| bar(ts, 100.0, 100.0, 100.0, 100.0)).to_vec();
    let engine = run_bars(Owner::new(buy_at_best(1.0)), bars, on(PEG_VENUE));
    assert_eq!(
        engine.core.dropped,
        [OrderDrop {
            symbol: SYM.to_string(),
            reason: BAR_RUN_REFUSAL.to_string(),
            size: 1.0,
            weight: 0.0
        }]
    );
    assert!(engine.strategy.log.is_empty() && engine.strategy.fills.is_empty());
    assert_nothing_rests(&engine);
}

/// A sell on the mid less 0.25: 100.25 rounds UP the grid to 100.5, then 100.75 to 101.0, which the bid of 101.0
/// fills by the tick law; the parent is done.
#[test]
fn a_sell_on_the_mid_snaps_up_the_grid_and_fills_by_the_tick_law() {
    let tape = vec![q(1, 100.0, 101.0), q(2, 100.0, 102.0), q(3, 101.0, 101.5)];
    let p = on_grid(lane(FillModelKind::Tick, PEG_VENUE), grid(TICK, 0.0, 0.0));
    let engine = run(Owner::new(peg(-1, 1.0, OrderPegAnchor::Mid, 0.25)), tape, p);
    assert_eq!(engine.strategy.log, [place(-1, 1.0, 100.5), reprice(101.0)]);
    assert_eq!(engine.core.symbol[0].pos.size, -1.0);
    assert!(engine.core.dropped.is_empty(), "done is no refusal: {:?}", engine.core.dropped);
    assert_nothing_rests(&engine);
}

/// Under the queue model a partial fill counts toward the parent, and the rest is re-priced, never re-sized.
#[test]
fn with_the_queue_model_a_partial_fill_counts_and_the_rest_is_repriced_never_resized() {
    let tape = vec![
        book(1, (100.0, 1.0), (101.0, 5.0)),
        tr(2, 100.0, 2.0),
        book(3, (100.5, 1.0), (101.0, 5.0)),
        tr(4, 100.5, 5.0),
        book(5, (100.0, 1.0), (101.0, 5.0)),
    ];
    let engine = run(Owner::new(buy_at_best(2.0)), tape, queued(PEG_VENUE));
    assert_eq!(engine.strategy.log, [place(1, 2.0, 100.0), reprice(100.5)]);
    assert_eq!(engine.core.maker_fills, 2, "one partial, then the rest");
    assert_eq!(engine.core.symbol[0].pos.size, 2.0);
    assert!(engine.core.dropped.is_empty());
    assert_nothing_rests(&engine);
}

/// The instrument's grid snaps the price (100.3 down to 100.0) and the 0.5 left after a slice, under the
/// minimum of 0.75, rides on that slice.
#[test]
fn the_instruments_grid_snaps_the_price_and_a_short_last_slice_rides_on_the_last() {
    let p = on_grid(lane(FillModelKind::Tick, SHOWN_VENUE), grid(TICK, 0.75, 0.0));
    let engine = run(Owner::new(iceberg_buy(100.3)), crossing(), p);
    assert_eq!(engine.strategy.log, [place(1, 1.0, 100.0), place(1, 1.5, 100.0)]);
    assert_eq!(engine.core.symbol[0].pos.size, 2.5);
    assert_nothing_rests(&engine);
}

/// A visible slice under the instrument's minimum notional never goes out.
#[test]
fn a_slice_the_instrument_refuses_never_goes_out() {
    let p = on_grid(lane(FillModelKind::Tick, SHOWN_VENUE), grid(TICK, 0.75, 500.0));
    let engine = run(Owner::new(iceberg_buy(100.0)), crossing(), p);
    assert!(engine.strategy.log.is_empty(), "{:?}", engine.strategy.log);
    assert_refused(&engine, "minimum 500", 2.5);
}

/// Each slice rests after the last one fills, and the last is what remains.
#[test]
fn on_the_tick_lane_each_slice_rests_after_the_last_fills_and_the_last_is_what_remains() {
    let tape = vec![
        q(1, 99.5, 100.5),
        q(2, 99.5, 100.0),
        q(3, 99.0, 99.5),
        q(4, 99.0, 99.5),
        q(5, 99.0, 99.5),
    ];
    let engine = run(Owner::new(iceberg_buy(100.0)), tape, lane(FillModelKind::Tick, SHOWN_VENUE));
    assert_eq!(engine.strategy.log, slices(100.0));
    assert_eq!(engine.core.maker_fills, 3);
    assert_eq!(engine.core.symbol[0].pos.size, 2.5);
    assert!(engine.strategy.fill_tags.iter().all(|t| t.as_deref() == Some(TAG)));
    assert!(engine.core.dropped.is_empty());
    assert_nothing_rests(&engine);
}

/// The price is fixed, so an iceberg needs no book: it runs a slice a bar.
#[test]
fn on_bars_the_iceberg_runs_a_slice_a_bar() {
    let bars = (0..5).map(|i| bar(i * 60_000, 99.5, 99.5, 99.5, 99.5)).collect();
    let engine = run_bars(Owner::new(iceberg_buy(100.0)), bars, on(SHOWN_VENUE));
    assert_eq!(engine.strategy.log, slices(100.0));
    assert_eq!(engine.core.symbol[0].pos.size, 2.5);
    assert!(engine.core.dropped.is_empty());
    assert_nothing_rests(&engine);
}

/// On the L2 lane a slice fills only once the book shows its size within its price: the first print cannot fill
/// a 1.0 slice off a 0.5 ask.
#[test]
fn on_the_l2_lane_a_slice_fills_only_once_the_book_shows_its_size_within_its_price() {
    let tape = vec![
        book(1, (100.0, 5.0), (101.0, 0.5)),
        tr(2, 101.0, 1.0),
        book(3, (100.0, 5.0), (101.0, 2.0)),
        tr(4, 101.0, 1.0),
        tr(5, 101.0, 1.0),
    ];
    let engine =
        run(Owner::new(shown(1, 2.0, 1.0, 101.0)), tape, lane(FillModelKind::L2Book, SHOWN_VENUE));
    assert_eq!(engine.strategy.log, [place(1, 1.0, 101.0), place(1, 1.0, 101.0)]);
    assert_eq!(engine.core.symbol[0].pos.size, 2.0);
    assert_nothing_rests(&engine);
}

/// The equivalence suite's iceberg tape: the slices a filling core sent.
#[test]
fn the_same_quotes_give_the_same_slices_as_a_filling_core() {
    let engine =
        run(Owner::new(iceberg_buy(100.0)), crossing(), lane(FillModelKind::Tick, SHOWN_VENUE));
    assert_eq!(engine.strategy.log, slices(100.0));
    assert_eq!(engine.core.symbol[0].pos.size, 2.5);
}

/// A venue no emulation places a child on (`vike_venues::EMULATION_REFUSED_VENUES`) gets no child of either kind,
/// and the refusal names it.
#[test]
fn a_venue_refused_by_name_gets_no_child() {
    for &(venue, _) in vike_venues::EMULATION_REFUSED_VENUES {
        for (kind, qty, owner) in [
            ("shown", 2.5, Owner::new(iceberg_buy(100.0))),
            ("pegged", 1.0, Owner::new(buy_at_best(1.0))),
        ] {
            let tape = vec![q(1, 99.5, 100.5), q(2, 99.5, 100.0)];
            let engine = run(owner, tape, lane(FillModelKind::Tick, venue));
            assert!(engine.strategy.log.is_empty(), "{venue} {kind}: {:?}", engine.strategy.log);
            assert_refused(&engine, venue, qty);
        }
    }
}

/// Eight bars reaching 98: the iceberg below the open fills every slice.
fn eight_bars() -> Vec<Bar> {
    (0..8).map(|i| bar(i * 60_000, 100.0, 100.0, 98.0, 100.0)).collect()
}

#[test]
fn a_bar_run_on_the_simulator_fills_every_slice() {
    let engine = run_bars(Owner::new(iceberg_buy(99.0)), eight_bars(), on(SHOWN_VENUE));
    assert_eq!(engine.core.symbol[0].pos.size, 2.5);
    assert!(engine.core.dropped.is_empty(), "{:?}", engine.core.dropped);
    assert_nothing_rests(&engine);
}

/// The bar run takes its grid and minimum from the instrument data: 99.3 rests at 99, and of 10 in slices of 3
/// the last 1 is under the minimum of 2, so it rides on the third slice (3, 3, 4).
#[test]
fn a_bar_run_takes_the_grid_and_the_minimum_from_its_instrument_data() {
    let p = on_grid(on(SHOWN_VENUE), grid(TICK, 2.0, 0.0));
    let engine = run_bars(Owner::new(shown(1, 10.0, 3.0, 99.3)), eight_bars(), p);
    assert_eq!(
        engine.strategy.log,
        [place(1, 3.0, 99.0), place(1, 3.0, 99.0), place(1, 4.0, 99.0)]
    );
    let pos = engine.core.symbol[0].pos;
    assert_eq!((pos.size, pos.average_price), (10.0, 99.0));
    assert!(engine.core.dropped.is_empty(), "{:?}", engine.core.dropped);
}

/// The law's progress is the PARENT's own filled size: a market buy the strategy filled first, as large as the
/// parent, leaves every child to go out (counted from the account's position, the parent would be done at once).
#[test]
fn a_strategys_other_order_on_the_symbol_is_no_progress_of_the_parent() {
    let after_a_market_buy = |size: f64, send: Box<dyn Fn(&mut OrderBrokerSimulator)>| {
        move |b: &mut OrderBrokerSimulator| {
            OrderBroker::place(b, SYM, "", 1, size, OrderPlacement::Market, false);
            send(b);
        }
    };
    let owner = Owner::new(after_a_market_buy(1.0, Box::new(buy_at_best(1.0))));
    let engine = run(owner, crossing(), lane(FillModelKind::Tick, PEG_VENUE));
    assert_eq!(engine.strategy.log, [place(1, 1.0, 99.5)]);
    assert_eq!(engine.core.symbol[0].pos.size, 2.0);
    let owner = Owner::new(after_a_market_buy(2.5, Box::new(iceberg_buy(100.0))));
    let engine = run(owner, crossing(), lane(FillModelKind::Tick, SHOWN_VENUE));
    assert_eq!(engine.strategy.log, slices(100.0));
    assert_eq!(engine.core.symbol[0].pos.size, 5.0);
}

/// A visible size the venue shows itself (binance's row; a run with no venue names an id off the roster, whose
/// book shows its own) is `OrderBrokerSimulator::submit_limit_iceberg`'s full-size law, byte for byte: the same result, the
/// same fills, and under the queue model the same refusal.
#[test]
fn a_visible_size_the_venue_shows_keeps_the_full_size_law_byte_for_byte() {
    let by_kind = || {
        Owner::new(|b: &mut OrderBrokerSimulator| {
            b.submit_limit_display_tagged(TAG, 1, 3.0, 100.0, 1.0)
        })
    };
    let by_law = || {
        Owner::new(|b: &mut OrderBrokerSimulator| {
            b.submit_limit_iceberg(SYM, 1, 3.0, 100.0, 1.0, 0.0, true, None)
        })
    };
    let lanes = [
        (FillModelKind::Bar, None),
        (FillModelKind::Tick, None),
        (FillModelKind::Tick, Some(QueueModelKind::RiskAverse)),
    ];
    for venue in [Some("binance"), None] {
        for (k, &(fill_model, queue_model)) in lanes.iter().enumerate() {
            let digest = |owner: Owner| {
                let default_venue = venue.map(str::to_string);
                let p = StrategyEngineConfig {
                    fill_model,
                    queue_model,
                    default_venue,
                    ..StrategyEngineConfig::default()
                };
                let engine = if fill_model == FillModelKind::Bar {
                    let bars = [(0, 101.0), (60_000, 99.0), (120_000, 98.0)];
                    run_bars(owner, bars.map(|(ts, px)| bar(ts, px, px, px, px)).to_vec(), p)
                } else {
                    run(owner, vec![q(1, 101.0, 102.0), q(2, 99.0, 100.0), q(3, 98.0, 99.0)], p)
                };
                let s = &engine.strategy;
                let c = &engine.core;
                let fees = (c.maker_fills, c.taker_fills, c.fees_paid);
                let book =
                    (&c.dropped, &c.trades, c.cash, c.symbol[0].pos, c.pending_of(SYM), fees);
                (s.fills.len(), format!("{:?} {:?} {book:?}", s.fills, s.fill_tags))
            };
            let (fills, kind) = digest(by_kind());
            assert_eq!(kind, digest(by_law()).1, "{venue:?} lane {k}");
            assert_eq!(fills > 0, k < 2, "{venue:?} lane {k}: it fills, but under the queue model");
        }
    }
}
