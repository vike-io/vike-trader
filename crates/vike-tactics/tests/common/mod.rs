//! The bench every suite drives a rule on by hand: a recording `OrderBroker` (every verb the
//! language sends, in order, and the position the test says its fills make), a rule written as a
//! function of the bar's index, and the three doors (`Bench::bar`, `Bench::fill`, `Bench::end`).
//! A verb on another symbol than the run's own is recorded as `Call::On`.

use std::collections::HashMap;

use vike_marketdata::Bar;
use vike_model::{
    Fill, OrderBroker, OrderEvent, OrderEventKind, OrderListLeg, OrderPlacement, Strategy,
};
use vike_tactics::{Rule, RuleDefaults, RuleRun, Tactics, TacticsBookBinding, TacticsBookLot};

pub const BAR_MS: i64 = 60_000;
pub const SYMBOL: &str = "X";

/// One verb the language sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Place {
        tag: String,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    },
    Leg {
        exit: String,
        tag: String,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    },
    AtClose {
        tag: String,
        side: i32,
        qty: f64,
        reduce_only: bool,
    },
    Amend {
        tag: String,
        qty: Option<f64>,
        price: Option<f64>,
        trigger: Option<f64>,
    },
    Cancel {
        tag: String,
    },
    Deny {
        tag: String,
        qty: f64,
        reason: String,
    },
    Group(Vec<(String, OrderListLeg)>),
    /// A verb naming another symbol than the run's own ([`SYMBOL`]), which is recorded bare.
    On(String, Box<Call>),
}

/// A broker that records what it is sent and reads back what the test set.
#[derive(Default)]
pub struct Recorder {
    pub calls: Vec<Call>,
    /// `(tag, side)` of every order placed, never drained.
    pub placed: Vec<(String, i32)>,
    /// The position of [`SYMBOL`] and of any symbol not in `others`.
    pub position: f64,
    /// The position of each other symbol a fill made one on (`Bench::fill_on`).
    pub others: HashMap<String, f64>,
    pub avg: f64,
    pub mark: f64,
    pub now: i64,
    pub bars: Vec<Bar>,
    /// `OrderBroker::rests_reduce_only_while_flat`: the reference's backtest rule (on in the bench,
    /// as the lab runs), or the live one.
    pub reference: bool,
}

impl Recorder {
    /// `call`, sent on `symbol`: bare on the run's own (`RuleRun::new`'s, the one the bench mounts),
    /// [`Call::On`] on any other.
    fn record(&mut self, symbol: &str, call: Call) {
        let call =
            if symbol == SYMBOL { call } else { Call::On(symbol.to_string(), Box::new(call)) };
        self.calls.push(call);
    }
}

impl OrderBroker for Recorder {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        self.placed.push((tag.to_string(), side));
        self.record(symbol, Call::Place { tag: tag.to_string(), side, qty, order, reduce_only });
    }
    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    ) {
        let (exit, tag) = (exit.to_string(), tag.to_string());
        self.placed.push((tag.clone(), side));
        self.record(symbol, Call::Leg { exit, tag, side, qty, order });
    }
    fn place_at_close(&mut self, symbol: &str, tag: &str, side: i32, qty: f64, reduce_only: bool) {
        self.placed.push((tag.to_string(), side));
        self.record(symbol, Call::AtClose { tag: tag.to_string(), side, qty, reduce_only });
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        self.placed.extend(legs.iter().map(|(tag, l)| (tag.clone(), l.side)));
        self.record(symbol, Call::Group(legs));
    }
    fn amend(&mut self, tag: &str, qty: Option<f64>, price: Option<f64>, trigger: Option<f64>) {
        self.calls.push(Call::Amend { tag: tag.to_string(), qty, price, trigger });
    }
    fn cancel(&mut self, tag: &str) {
        self.calls.push(Call::Cancel { tag: tag.to_string() });
    }
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        self.record(symbol, Call::Deny { tag: tag.to_string(), qty, reason: reason.to_string() });
    }
    fn average_price(&self, _symbol: &str) -> f64 {
        self.avg
    }
    fn rests_reduce_only_while_flat(&self) -> bool {
        self.reference
    }
    fn position(&self, symbol: &str) -> f64 {
        self.others.get(symbol).copied().unwrap_or(self.position)
    }
    fn price(&self, _symbol: &str) -> f64 {
        self.mark
    }
    fn equity(&self) -> f64 {
        1_000.0
    }
    fn bars(&self, _symbol: &str) -> &[Bar] {
        &self.bars
    }
    fn index(&self) -> usize {
        self.bars.len().saturating_sub(1)
    }
    fn now(&self) -> i64 {
        self.now
    }
}

/// What a script does on bar `k` (and after a fill inside bar `k`).
pub type Step = fn(&mut Tactics<'_>, i64);

/// A rule written as what it does on bar `k`, and after a fill inside bar `k`.
pub struct Script {
    pub header: RuleDefaults,
    pub on_bar: Step,
    pub after_fill: Step,
    /// What the rule saw: `(k, position, avg_cost)` on every bar.
    pub seen: Vec<(i64, f64, f64)>,
    /// The lots and bindings it saw on every bar.
    pub lots: Vec<(Vec<TacticsBookLot>, Vec<TacticsBookBinding>)>,
    /// The `k` of every bar `after_fill` ran with.
    pub reruns: Vec<i64>,
}

pub fn quiet(_: &mut Tactics<'_>, _: i64) {}

impl Rule for Script {
    fn defaults(&self) -> RuleDefaults {
        self.header
    }
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let k = bar.timestamp / BAR_MS;
        self.seen.push((k, strategy.position(), strategy.avg_cost()));
        self.lots.push((strategy.lots().to_vec(), strategy.bindings().to_vec()));
        (self.on_bar)(strategy, k);
    }
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.reruns.push(bar.timestamp / BAR_MS);
        (self.after_fill)(strategy, bar.timestamp / BAR_MS);
    }
}

pub fn script(on_bar: Step) -> Script {
    let (seen, lots, reruns) = (Vec::new(), Vec::new(), Vec::new());
    Script { header: RuleDefaults::PLAIN, on_bar, after_fill: quiet, seen, lots, reruns }
}

/// A flat bar `k` at `price`.
pub fn bar(k: i64, price: f64) -> Bar {
    Bar {
        timestamp: k * BAR_MS,
        open: price,
        high: price,
        low: price,
        close: price,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A rule on the recorder, driven by hand.
pub struct Bench {
    pub run: RuleRun<Script>,
    pub broker: Recorder,
}

impl Bench {
    /// `run`, with a tick of 0.01.
    pub fn new(run: RuleRun<Script>) -> Bench {
        let broker = Recorder { mark: 100.0, reference: true, ..Recorder::default() };
        Bench { run: run.tick(0.01), broker }
    }

    pub fn of(on_bar: Step) -> Bench {
        Bench::new(RuleRun::new(script(on_bar), SYMBOL))
    }

    /// Bar `k` at `price` closes: the rule runs. What it sent.
    pub fn bar(&mut self, k: i64, price: f64) -> Vec<Call> {
        let b = bar(k, price);
        self.broker.now = b.timestamp;
        self.broker.mark = price;
        self.run.on_bar(&mut self.broker, &b);
        self.broker.bars.push(b);
        std::mem::take(&mut self.broker.calls)
    }

    /// The order `tag` fills `size` at `price`, stamped `ts`: the position moves by its side
    /// (read off what was placed under it), and the average is `price` when it opens. What the
    /// language sent in answer.
    pub fn fill_at(&mut self, tag: &str, size: f64, price: f64, ts: i64) -> Vec<Call> {
        let side = self.side_of(tag);
        self.fill_side(Some(tag), side, size, price, ts)
    }

    /// A fill of `tag` stamped inside the bar after the last one closed.
    pub fn fill(&mut self, tag: &str, size: f64, price: f64) -> Vec<Call> {
        let ts = self.broker.bars.last().map_or(0, |b| b.timestamp + BAR_MS);
        self.fill_at(tag, size, price, ts)
    }

    /// A fill naming `tag` (`None`: the engine's own), of `side`.
    pub fn fill_side(
        &mut self,
        tag: Option<&str>,
        side: i32,
        size: f64,
        price: f64,
        ts: i64,
    ) -> Vec<Call> {
        let before = self.broker.position;
        self.broker.position += f64::from(side) * size;
        if self.broker.position == 0.0 {
            self.broker.avg = 0.0;
        } else if before == 0.0 || (before > 0.0) != (self.broker.position > 0.0) {
            self.broker.avg = price;
        }
        self.broker.now = ts;
        let fill = Fill {
            side,
            size,
            price,
            fee: 0.0,
            timestamp: ts,
            is_maker: false,
            symbol: SYMBOL.to_string(),
            tag: tag.map(str::to_string),
        };
        self.run.on_fill(&mut self.broker, &fill);
        std::mem::take(&mut self.broker.calls)
    }

    /// [`Bench::fill`] of an order on `symbol`, another than the run's own: that symbol's position
    /// moves by its side.
    pub fn fill_on(&mut self, symbol: &str, tag: &str, size: f64, price: f64) -> Vec<Call> {
        let side = self.side_of(tag);
        *self.broker.others.entry(symbol.to_string()).or_default() += f64::from(side) * size;
        let ts = self.broker.bars.last().map_or(0, |b| b.timestamp + BAR_MS);
        self.broker.now = ts;
        let (fee, is_maker, symbol, tag) = (0.0, false, symbol.to_string(), Some(tag.to_string()));
        let fill = Fill { side, size, price, fee, timestamp: ts, is_maker, symbol, tag };
        self.run.on_fill(&mut self.broker, &fill);
        std::mem::take(&mut self.broker.calls)
    }

    /// The order `tag` ended without a fill (`kind`).
    pub fn end(&mut self, tag: &str, kind: OrderEventKind) -> Vec<Call> {
        let event = OrderEvent { order_client_id: String::new(), tag: Some(tag.to_string()), kind };
        self.run.on_order_event(&mut self.broker, &event);
        std::mem::take(&mut self.broker.calls)
    }

    /// The side the order `tag` was placed with, from every call so far.
    fn side_of(&self, tag: &str) -> i32 {
        self.broker
            .placed
            .iter()
            .find(|(t, _)| t == tag)
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("no order was placed under {tag}"))
    }
}
