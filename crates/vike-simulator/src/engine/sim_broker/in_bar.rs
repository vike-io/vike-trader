//! The simulator's IN-BAR fill rules (decision 0269, item 5), each a field of [`InBarRules`]
//! (`StrategyEngineConfig::in_bar`) and every one OFF by default, so a run that sets none is byte-identical.
//! They are SIMULATION rules: when one bar's walk meets the resting book, and in what order. The bar
//! lane alone reads them (`StrategyEngine::run`); the paper book and the live core never do, so a
//! rule here cannot change a live run, and `tests/r7_gate.rs` keeps backtest equal to paper under the
//! defaults. The fill law does not change: each rule decides where on the bar's path, and in which
//! order, an order meets a price, and `vike_model::order_fill_price` still decides whether and where
//! it fills.
//!
//! Two facts the rules read that no `OrderResting` carries come through two verbs of the portable
//! seam whose default is their live meaning, overridden here (`traits.rs`): the legs of ONE exit
//! (`vike_model::OrderBroker::place_exit_leg`: the stop, the target and the trailing leg protecting
//! one position, which [`InBarRules::price_order`] queues together and [`InBarRules::committed_stops`]
//! judges a stop against; live, a reduce-only order), and an order that fills at THIS bar's close
//! alone (`vike_model::OrderBroker::place_at_close`; live, a market order sent at the close). Both
//! name their order by the tag it was placed under.

use serde::Deserialize;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderKind, OrderPlacement, OrderResting};

use super::state::OrderBrokerSimulator;

/// The in-bar fill rules a run switches on, each OFF by default (`Default`), the bar lane's alone:
/// this module's doc has what they share.
///
/// It IS a backtest profile's `[engine.in_bar]` table, field for field (`vike-backtest` reads the TOML
/// straight into it; an absent key is `false`). Each field's first paragraph is the sentence the
/// profile's published schema shows for that key; the rest is the rule's law.
///
/// ```toml
/// [engine.in_bar]
/// path_walk = true      # walk every bar open → nearer extreme → farther extreme → close
/// price_order = true    # fill the levels in the order the price reaches them
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InBarRules {
    /// Walk every bar along its path: the open, the nearer extreme, the farther extreme, the close
    /// (each bar of `data.detail_interval` so, in turn, when the profile states one), every fill
    /// stamped with the bar.
    ///
    /// Each bar is walked along its PATH, `[O]`, `[O→near]`, `[near→far]`, `[far→C]`
    /// (`vike_marketdata::Bar::path_points`: the nearer extreme first, a tie low first), every
    /// stretch stamped with the bar's own time, so the bar's resting orders fill in the order the
    /// path reaches them and a fill carries its bar. With a detail series
    /// (`StrategyEngineConfig::granular_by_symbol`) each detail bar inside the bar is walked so in turn,
    /// stamped with the bar. Each stretch carries the volume of the bar it is cut from.
    #[serde(default)]
    pub path_walk: bool,
    /// A market order placed on a bar fills at that bar's close instead of the next open, and every
    /// order resting then meets that close too.
    ///
    /// A market order placed on a bar fills at THAT bar's close, stamped with that bar, ahead of the
    /// next bar's walk; every order resting then meets that close too, so a stop the close is
    /// already through fills there. Without it a market order fills at the next open, and only an
    /// order placed by `OrderBroker::place_at_close` meets the close.
    #[serde(default)]
    pub fill_at_close: bool,
    /// Resting orders fill in the order the price reaches their levels: each stretch of a bar's walk
    /// is cut at every level, and a gap past several fills market orders, then entries, then exits,
    /// each from the lowest level. Pair it with `path_walk` (without a walk a bar has one stretch).
    ///
    /// The walk meets the resting orders in the order the price reaches them: after the strategy
    /// has run on a bar, each stretch of the next bar's walk is cut at every level resting then, so
    /// one stretch never fills two levels at once, and the book is ordered for the points that fill
    /// several orders at once (an open gapped past them): market orders first as they came, then
    /// entries (orders that are not reduce-only), then exits (reduce-only), each by its level from
    /// the lowest, the legs of one exit together at their lowest level with its stop first, then its
    /// limit, then the rest. An order placed during the walk adds no cut.
    #[serde(default)]
    pub price_order: bool,
    /// The order of the book: market orders first, a new buy ahead of the first resting sell, then
    /// the order they were placed in; without it every new order goes last.
    ///
    /// The order of the book: a new market order goes after the market orders resting and ahead of
    /// everything else, a new buy ahead of the first resting sell, anything else last; an amend keeps
    /// an order where it rests. An if-touched leg of an exit (`OrderBroker::place_exit_leg`) goes
    /// ahead of that exit's limit, and a reduce-only stop or limit placed while a fill is delivered,
    /// which that fill's price is already past, goes ahead of the whole book. Without it every new
    /// order goes last.
    #[serde(default)]
    pub queue_order: bool,
    /// A lone protective stop that the next open gaps through fills even after an entry at that open
    /// reversed the position, opening its size the other way.
    ///
    /// A lone stop of an exit that the next open gaps through is COMMITTED to that open: it stops
    /// being reduce-only, so it fills even after an entry reversed the position, and opens its size
    /// the other way. Not with a market order in the book, and not when another leg of its exit gaps
    /// at the same open. Judged after the strategy has run on a bar, against the next bar's open.
    #[serde(default)]
    pub committed_stops: bool,
    /// A trailing order placed in reaction to a fill starts watching its activation from that point
    /// of the bar, not from the previous bar's close.
    ///
    /// A trailing order placed while a fill is delivered to the strategy waits for its activation
    /// from the next point of the walk, not from the last close the engine marked (which, inside a
    /// bar, is the bar before's).
    #[serde(default)]
    pub trail_from_fill: bool,
    /// A reduce-only order may be placed while nothing is held to reduce; it is checked when it
    /// fills instead.
    ///
    /// A reduce-only order is judged when it FILLS, not when it is placed: it may rest while nothing
    /// is held on its side (a guard placed with the entry it protects), and its fill is capped to the
    /// position then, or dropped. An order list's legs are still judged when the list is placed (its
    /// whole-or-nothing law). BACKTEST-ONLY, for the reference match (the owner's ruling of
    /// 2026-11-10): no live broker rests such an order, so live the order language sends an exit
    /// issued with its entry as that entry's OTO child instead (`vike_tactics`), and this rule is
    /// what `vike_model::OrderBroker::rests_reduce_only_while_flat` answers here.
    #[serde(default)]
    pub reduce_only_at_fill: bool,
}

/// The rules' running state on one run: [`InBarRules`] and the facts its verbs file.
#[derive(Default)]
pub(crate) struct InBar {
    pub(crate) rules: InBarRules,
    /// `(qid, exit)`: each resting leg of an exit, by the order it is (`OrderBroker::place_exit_leg`).
    legs: Vec<(u64, String)>,
    /// The orders that meet this bar's close (`OrderBroker::place_at_close`).
    at_close: Vec<u64>,
    /// Per symbol: the walk of the step under way opens with the previous bar's close.
    close_point: Vec<bool>,
    /// The price of the fill being delivered to the strategy, while it is
    /// (`StrategyEngine::fire_on_fill`).
    pub(crate) delivering: Option<f64>,
}

impl InBar {
    pub(crate) fn new(rules: InBarRules, symbols: usize) -> Self {
        InBar { rules, close_point: vec![false; symbols], ..InBar::default() }
    }

    /// The order `old` took the new identity `fresh` (an amend up under the opt-in queue model,
    /// `OrderBrokerSimulator::amend_queue_id`): it is the same order at the venue, so it stays a leg of its exit and
    /// still meets this bar's close.
    pub(crate) fn follow(&mut self, old: u64, fresh: u64) {
        let legs = self.legs.iter_mut().map(|(q, _)| q);
        for q in legs.chain(self.at_close.iter_mut()).filter(|q| **q == old) {
            *q = fresh;
        }
    }
}

/// `bar`'s path as four stretches stamped `ts`, each carrying `bar`'s volume and quotes.
pub(crate) fn path_of(bar: &Bar, ts: i64) -> [Bar; 4] {
    let [o, near, far, c] = bar.path_points();
    [
        stretch(bar, ts, o, o),
        stretch(bar, ts, o, near),
        stretch(bar, ts, near, far),
        stretch(bar, ts, far, c),
    ]
}

/// One straight stretch of `bar`'s walk, from `from` to `to`, stamped `ts`.
fn stretch(bar: &Bar, ts: i64, from: f64, to: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: from,
        high: from.max(to),
        low: from.min(to),
        close: to,
        volume: bar.volume,
        funding: None,
        bid: bar.bid,
        ask: bar.ask,
        symbol: None,
    }
}

/// `s` cut at every one of `levels` (sorted, distinct) that lies strictly inside it, in the order the
/// price meets them.
fn cut_at(s: &Bar, levels: &[f64]) -> Vec<Bar> {
    let (from, to) = (s.open, s.close);
    let mut inside: Vec<f64> =
        levels.iter().copied().filter(|&l| l > from.min(to) && l < from.max(to)).collect();
    if inside.is_empty() {
        return vec![s.clone()];
    }
    if to < from {
        inside.reverse();
    }
    let points: Vec<f64> = std::iter::once(from).chain(inside).chain([to]).collect();
    points.windows(2).map(|p| stretch(s, s.timestamp, p[0], p[1])).collect()
}

/// The level the price order ranks `o` by.
fn level(o: &OrderResting) -> f64 {
    o.price.or(o.activation).or(o.extreme).unwrap_or(0.0)
}

impl OrderBrokerSimulator {
    /// Where a new order joins `symbols[si]`'s book under [`InBarRules::queue_order`] (the end
    /// without it). The in-bar rules do not read the maker law's quotes (`OrderBrokerSimulatorSymbolState::quotes`): a
    /// quote joins at the end, and no order queues by one.
    pub(super) fn queue_at(&self, si: usize, o: &OrderResting) -> usize {
        let pending = &self.symbol[si].pending;
        if !self.in_bar.rules.queue_order || self.is_quote(si, o.qid) {
            return pending.len();
        }
        if let Some(price) = self.in_bar.delivering
            && o.reduce_only
            && let (OrderKind::Stop | OrderKind::Limit, Some(level)) = (o.kind, o.price)
        {
            let side = f64::from(o.side);
            let past = if o.kind == OrderKind::Stop {
                side * (price - level) >= 0.0
            } else {
                side * (level - price) >= 0.0
            };
            if past {
                return 0;
            }
        }
        let market = o.kind == OrderKind::Market;
        let buy = o.side > 0;
        pending
            .iter()
            .position(|r| {
                r.kind != OrderKind::Market
                    && (market || (buy && r.side < 0))
                    && !self.is_quote(si, r.qid)
            })
            .unwrap_or(pending.len())
    }

    /// `vike_model::OrderBroker::place_exit_leg` here: a reduce-only `order` under `tag` as a leg of
    /// the exit `exit`, placed as `vike_model::OrderBroker::place` places any order. The exit is read
    /// by [`InBarRules::price_order`] (its legs queue together) and [`InBarRules::committed_stops`]
    /// (a stop is judged against its other legs), and under [`InBarRules::queue_order`] an
    /// if-touched leg goes ahead of the exit's resting limit. It links no fills: one leg's fill
    /// pulls none of the others (an order list's OCO does).
    pub(super) fn file_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
    ) {
        self.place(symbol, tag, side, qty, order, true);
        let Some(&(si, qid)) = self.order_tags.get(tag) else { return };
        let Some(at) = self.symbol[si].pending.iter().position(|o| o.qid == qid) else { return };
        self.in_bar.legs.retain(|(q, _)| self.symbol[si].pending.iter().any(|o| o.qid == *q));
        self.in_bar.legs.push((qid, exit.to_string()));
        if !self.in_bar.rules.queue_order
            || self.symbol[si].pending[at].kind != OrderKind::TakeProfit
        {
            return;
        }
        let limit = self.symbol[si].pending[..at]
            .iter()
            .position(|o| o.kind == OrderKind::Limit && self.exit_of(o.qid) == Some(exit));
        if let Some(ahead) = limit {
            let leg = self.symbol[si].pending.remove(at);
            self.symbol[si].pending.insert(ahead, leg);
        }
    }

    /// `vike_model::OrderBroker::place_at_close` here: a market order under `tag` that fills at THIS
    /// bar's close, stamped with this bar, ahead of the next bar's walk, while every other order
    /// waits for that walk ([`InBarRules::fill_at_close`] has every market order do so).
    pub(super) fn file_at_close(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        reduce_only: bool,
    ) {
        self.place(symbol, tag, side, qty, OrderPlacement::Market, reduce_only);
        if let Some(&(si, qid)) = self.order_tags.get(tag)
            && self.symbol[si].pending.iter().any(|o| o.qid == qid)
        {
            self.in_bar.at_close.push(qid);
        }
    }

    /// The exit the order `qid` is a leg of.
    fn exit_of(&self, qid: u64) -> Option<&str> {
        self.in_bar.legs.iter().find(|(q, _)| *q == qid).map(|(_, e)| e.as_str())
    }

    /// Whether `o` meets the walk's opening point on `symbols[si]` when that point is the previous
    /// bar's close (it always does otherwise).
    pub(crate) fn meets_close_point(&self, si: usize, k: usize, o: &OrderResting) -> bool {
        k > 0
            || !self.in_bar.close_point[si]
            || self.in_bar.rules.fill_at_close
            || self.in_bar.at_close.contains(&o.qid)
    }

    /// The step's fills are done: its opening close point, where it had one, has passed.
    pub(crate) fn close_points_passed(&mut self) {
        if self.in_bar.close_point.iter().any(|c| *c) {
            self.in_bar.close_point.fill(false);
            self.in_bar.at_close.clear();
        }
    }

    /// After the strategy has run on step `i`: the rules that act on the book it leaves and on the
    /// next step's walk, in this order: the price order's book, the committed stops, this bar's
    /// close ahead of the next walk, the price order's cuts. No-op with every rule off.
    pub(crate) fn after_decide(&mut self, i: usize) {
        let r = self.in_bar.rules;
        if !(r.price_order
            || r.committed_stops
            || r.fill_at_close
            || !self.in_bar.at_close.is_empty())
        {
            return;
        }
        let next = i + 1;
        for si in 0..self.symbols.len() {
            if r.price_order {
                self.queue_by_price(si);
            }
            if next >= self.n {
                continue;
            }
            if r.committed_stops {
                self.commit_gapped(si, next);
            }
            self.stage_close(si, i);
            if r.price_order {
                self.cut_walk(si, next);
            }
        }
        if !self.in_bar.close_point.iter().any(|c| *c) {
            self.in_bar.at_close.clear();
        }
    }

    /// [`InBarRules::price_order`]'s book: market orders first as they came, then entries, then
    /// exits, each by its level from the lowest, an exit's legs together at their lowest level with
    /// its stop first; the maker law's quotes, which no in-bar rule reads, last as they came. A
    /// stable sort, so equal keys keep their order.
    fn queue_by_price(&mut self, si: usize) {
        let pending = &self.symbol[si].pending;
        let keys: Vec<(u8, f64, usize, u8)> = pending
            .iter()
            .enumerate()
            .map(|(i, o)| {
                if self.is_quote(si, o.qid) {
                    return (3, 0.0, i, 0);
                }
                if o.kind == OrderKind::Market {
                    return (0, 0.0, i, 0);
                }
                if !o.reduce_only {
                    return (1, level(o), i, 0);
                }
                let Some(exit) = self.exit_of(o.qid) else { return (2, level(o), i, 3) };
                let legs =
                    pending.iter().enumerate().filter(|(_, p)| self.exit_of(p.qid) == Some(exit));
                let (first, lowest) =
                    legs.fold((i, f64::INFINITY), |(f, l), (j, p)| (f.min(j), l.min(level(p))));
                let rank = match o.kind {
                    OrderKind::Stop => 0,
                    OrderKind::Limit => 1,
                    _ => 2,
                };
                (2, lowest, first, rank)
            })
            .collect();
        let mut order: Vec<usize> = (0..pending.len()).collect();
        // `total_cmp`, so a level a strategy left NaN sorts last instead of stopping the run.
        order.sort_by(|&a, &b| {
            let (x, y) = (keys[a], keys[b]);
            x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)).then(x.2.cmp(&y.2)).then(x.3.cmp(&y.3))
        });
        let mut taken: Vec<Option<OrderResting>> =
            std::mem::take(&mut self.symbol[si].pending).into_iter().map(Some).collect();
        self.symbol[si].pending =
            order.iter().map(|&k| taken[k].take().expect("each once")).collect();
    }

    /// [`InBarRules::committed_stops`]: each exit's lone stop that the open of step `next` gaps
    /// through stops being reduce-only. Nothing is committed with a market order in the book.
    fn commit_gapped(&mut self, si: usize, next: usize) {
        let open = self.symbol[si].sub[next].first().map_or(self.bars[si][next].open, |s| s.open);
        let pending = &self.symbol[si].pending;
        if pending.iter().any(|o| o.kind == OrderKind::Market) {
            return;
        }
        let gapped = |o: &OrderResting| match (o.kind, o.price) {
            (OrderKind::Stop, Some(level)) => (open - level) * f64::from(o.side) >= 0.0,
            (OrderKind::Limit | OrderKind::TakeProfit, Some(level)) => {
                (open - level) * f64::from(o.side) <= 0.0
            }
            _ => false,
        };
        let lone: Vec<u64> = pending
            .iter()
            .filter(|o| o.kind == OrderKind::Stop && gapped(o))
            .filter_map(|o| {
                let exit = self.exit_of(o.qid)?;
                let mates =
                    pending.iter().filter(|m| self.exit_of(m.qid) == Some(exit) && gapped(m));
                (mates.count() == 1).then_some(o.qid)
            })
            .collect();
        for o in self.symbol[si].pending.iter_mut().filter(|o| lone.contains(&o.qid)) {
            o.reduce_only = false;
        }
    }

    /// This bar's close (step `i`) ahead of the next step's walk, stamped with this bar, when an
    /// order is to meet it: under [`InBarRules::fill_at_close`] any resting order, else one placed
    /// by `OrderBroker::place_at_close`. A next step with no walk of its own walks its bar after it.
    fn stage_close(&mut self, si: usize, i: usize) {
        let pending = &self.symbol[si].pending;
        let wanted = if self.in_bar.rules.fill_at_close {
            self.rests_beside_quotes(si)
        } else {
            pending.iter().any(|o| self.in_bar.at_close.contains(&o.qid))
        };
        if !wanted {
            return;
        }
        let bar = self.bars[si][i].clone();
        let point = stretch(&bar, bar.timestamp, bar.close, bar.close);
        let next = &mut self.symbol[si].sub[i + 1];
        if next.is_empty() {
            next.push(self.bars[si][i + 1].clone());
        }
        next.insert(0, point);
        self.in_bar.close_point[si] = true;
    }

    /// [`InBarRules::price_order`]'s cuts: every stretch of step `next`'s walk cut at every level
    /// resting (a trigger, an unactivated trailing order's activation, a stop-limit's limit).
    fn cut_walk(&mut self, si: usize, next: usize) {
        let mut levels: Vec<f64> = self.symbol[si]
            .pending
            .iter()
            .filter(|o| !self.is_quote(si, o.qid))
            .flat_map(|o| {
                let trigger = match o.kind {
                    OrderKind::Stop
                    | OrderKind::Limit
                    | OrderKind::StopLimit
                    | OrderKind::TakeProfit => o.price,
                    OrderKind::Trailing | OrderKind::TrailingLimit if o.extreme.is_none() => {
                        o.activation
                    }
                    _ => None,
                };
                trigger.into_iter().chain(o.limit.filter(|_| o.kind == OrderKind::StopLimit))
            })
            .collect();
        if levels.is_empty() {
            return;
        }
        levels.sort_by(f64::total_cmp);
        levels.dedup();
        let walk = std::mem::take(&mut self.symbol[si].sub[next]);
        self.symbol[si].sub[next] = walk.iter().flat_map(|s| cut_at(s, &levels)).collect();
    }
}
