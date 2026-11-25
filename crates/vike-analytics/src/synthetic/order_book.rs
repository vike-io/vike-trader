//! A synthetic limit order book: the Cont-Stoikov-Talreja queue model (Cont, Stoikov and Talreja, "A
//! stochastic model for order book dynamics", Operations Research 58(3), 2010; "CST" below), simulated
//! event by event and emitted as the three tick streams a tick replay reads: `L2BookUpdate`s (one
//! `Snapshot` anchor, then one `Delta` per change, `sequence` contiguous from 1), the `TickTrade` every
//! market order prints, and a top-of-book `TickQuote` whenever the top changes.
//!
//! # The model (CST section 2.2)
//!
//! Prices are whole ticks and every order is one unit (`order_size` in the output; CST's unit is the
//! average limit-order size). With `pA` the best ask, `pB` the best bid and `i >= 1` a distance in ticks
//! from the OPPOSITE best quote, these independent exponential clocks drive the book:
//! - a limit buy arrives at `pA - i` and a limit sell at `pB + i`, each at rate `limit_rates[i - 1]`, for
//!   `i <= depth` (`depth` is `limit_rates.len()`);
//! - a market buy takes one unit at `pA` and a market sell one at `pB`, each at rate `market_rate`;
//! - each resting order `i` ticks from the opposite best is cancelled at rate `cancel_rates[i - 1]`, so a
//!   queue of `x` orders loses one at `x` times that rate; beyond `depth` the last rate applies (CST:
//!   `theta(i) = theta(5)` for `i > 5`).
//!
//! The simulation is exact (Gillespie): the next event comes after an exponential time at the sum of
//! every rate, and is each one with probability proportional to its rate. Distances move with the
//! book, so a limit order can land inside the spread and improve the quote, and the book never crosses.
//!
//! # Defaults: CST's Table 2 ([`SyntheticOrderBookModel::cont_stoikov_talreja`])
//!
//! Sky Perfect Communications on the Tokyo Stock Exchange, rates per MINUTE: `lambda(1..5)` = 1.85,
//! 1.51, 1.09, 0.88, 0.77, extended to `depth` 10 by the paper's power-law fit `1.92 / i^0.52` (its
//! Figure 1(a)); `theta(1..5)` = 0.71, 0.81, 0.68, 0.56, 0.47, then `theta(5)`; `mu` = 0.94. The tick is
//! the sample's 100 (yen, its Table 1) and the order size 1. The initial queues are Poisson with mean
//! `lambda(i) / theta(i)`, the M/M/inf mean of a queue at a fixed distance (CST's eq. 19 at `t -> inf`),
//! which the paper does not state as a starting law; the model's own dynamics take over within minutes.
//!
//! # Where it departs from the paper
//!
//! - CST's book lives on a finite grid `{1..n}` and forces `pA = n + 1` (`pB = 0`) when a side is empty.
//!   Here the grid is unbounded, and an empty side's reference sits `depth + 1` ticks from the opposite
//!   best, the edge of where orders arrive: a market order against an empty side trades nothing, and no
//!   quote is emitted while a side is empty.
//! - A timestamp is the event time floored to the millisecond, so events can share a timestamp, never
//!   a sequence number. A price walking to zero ticks or below is refused, not clamped.

use std::collections::BTreeMap;

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};

use super::{Draws, at_least_zero};

/// An initial queue whose Poisson mean is above this is refused: the draw walks one arrival per order.
const MAX_INITIAL_MEAN: f64 = 10_000.0;

/// How each side's queues start, by distance `i` (entry `i - 1`) from the opposite best quote: the
/// best bid and best ask start one tick apart.
#[derive(Debug, Clone, PartialEq)]
pub enum SyntheticInitialQueues {
    /// Each side's queue at distance `i` holds a Poisson number of orders with mean `means[i - 1]`.
    Poisson(Vec<f64>),
    /// Exactly these counts: `bids[i - 1]` orders `i` ticks below the best ask, `asks[i - 1]` orders `i`
    /// ticks above the best bid.
    Fixed {
        /// Bid queues by distance from the ask.
        bids: Vec<u64>,
        /// Ask queues by distance from the bid.
        asks: Vec<u64>,
    },
}

/// Every parameter of the queue model (the module doc's "The model"); rates are per MINUTE.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticOrderBookModel {
    /// `lambda(i)`: limit orders per minute per side at distance `i` from the opposite best; its length
    /// is the model's depth, at least 1.
    pub limit_rates: Vec<f64>,
    /// `theta(i)`: cancellations per minute per resting order at distance `i`, as long as
    /// `limit_rates`; the last entry applies beyond the depth.
    pub cancel_rates: Vec<f64>,
    /// `mu`: market orders per minute per side.
    pub market_rate: f64,
    /// The price of one tick, `> 0`.
    pub tick_size: f64,
    /// The quantity of one order, `> 0`.
    pub order_size: f64,
    /// The book at the first instant.
    pub initial_queues: SyntheticInitialQueues,
}

impl SyntheticOrderBookModel {
    /// CST's Table 2 estimates and the defaults the module doc documents.
    #[must_use]
    pub fn cont_stoikov_talreja() -> Self {
        let mut limit_rates = vec![1.85, 1.51, 1.09, 0.88, 0.77];
        let mut cancel_rates = vec![0.71, 0.81, 0.68, 0.56, 0.47];
        for i in 6..=10 {
            limit_rates.push(1.92 / libm::pow(f64::from(i), 0.52));
            cancel_rates.push(0.47);
        }
        let means = limit_rates.iter().zip(&cancel_rates).map(|(l, c)| l / c).collect();
        SyntheticOrderBookModel {
            limit_rates,
            cancel_rates,
            market_rate: 0.94,
            tick_size: 100.0,
            order_size: 1.0,
            initial_queues: SyntheticInitialQueues::Poisson(means),
        }
    }
}

/// The three tick streams of one simulated book, every row's `symbol` empty (the store that serves
/// them stamps it) and its `local_timestamp` 0 (no feed).
#[derive(Debug, Clone)]
pub struct SyntheticOrderBook {
    /// The `Snapshot` at the start, then one `Delta` per change; `sequence` runs 1, 2, 3, ...
    pub updates: Vec<L2BookUpdate>,
    /// One trade per market order that met a resting order; `is_buyer_maker` is `true` for a market sell.
    pub trades: Vec<TickTrade>,
    /// The top of the book at the start and after every change of it, while both sides hold orders.
    pub quotes: Vec<TickQuote>,
}

/// Simulate `model` from `start_ms` until before `end_ms`, the best bid starting at the tick under
/// `start_price` and the best ask one tick above, drawn from `seed` (the module doc's model).
///
/// # Errors
/// A sentence naming the parameter refused, or the minute the price walked to zero ticks.
pub fn order_book_stream(
    model: &SyntheticOrderBookModel,
    start_price: f64,
    start_ms: i64,
    end_ms: i64,
    seed: u64,
) -> Result<SyntheticOrderBook, String> {
    check(model, start_price, start_ms, end_ms)?;
    let mut draws = Draws(ChaCha8Rng::seed_from_u64(seed));
    let mut book = Book::open(model, start_price, &mut draws)?;
    let mut out =
        SyntheticOrderBook { updates: Vec::new(), trades: Vec::new(), quotes: Vec::new() };
    out.updates.push(L2BookUpdate {
        timestamp: start_ms,
        local_timestamp: 0,
        sequence: 1,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: model.tick_size,
        bids: book.bids.iter().rev().map(|(&t, &n)| book.level(t, n)).collect(),
        asks: book.asks.iter().map(|(&t, &n)| book.level(t, n)).collect(),
        symbol: String::new(),
    });
    let mut top = book.top();
    if let Some(quote) = book.quote(top, start_ms) {
        out.quotes.push(quote);
    }
    let mut minutes = 0.0;
    loop {
        let total = book.total_rate();
        if total <= 0.0 {
            break; // nothing can happen any more: an empty book with no arrivals
        }
        minutes += draws.exponential(total);
        // Floored: the instant falls inside its millisecond. Past `end_ms` (or any overflow) ends it.
        // The model's time unit is the minute (`vike_model::MS_PER_MINUTE`, exact in an `f64`).
        let timestamp =
            start_ms.saturating_add((minutes * vike_model::MS_PER_MINUTE as f64) as i64);
        if timestamp >= end_ms {
            break;
        }
        let event = book.choose(draws.unit() * total);
        let Some(Change { bid_side, tick, count, traded }) = book.apply(event, minutes)? else {
            continue; // a market order against an empty side: nothing changed
        };
        let level = vec![book.level(tick, count)];
        let (bids, asks) = if bid_side { (level, Vec::new()) } else { (Vec::new(), level) };
        out.updates.push(L2BookUpdate {
            timestamp,
            local_timestamp: 0,
            sequence: out.updates.len() as u64 + 1,
            kind: L2BookUpdateKind::Delta,
            tick_size: model.tick_size,
            bids,
            asks,
            symbol: String::new(),
        });
        if traded {
            out.trades.push(TickTrade {
                timestamp,
                local_timestamp: 0,
                price: book.price(tick),
                size: model.order_size,
                // A market SELL hit the bid: the buyer was the resting order.
                is_buyer_maker: bid_side,
                symbol: Default::default(),
            });
        }
        let now = book.top();
        if now != top {
            top = now;
            if let Some(quote) = book.quote(top, timestamp) {
                out.quotes.push(quote);
            }
        }
    }
    Ok(out)
}

/// The model's parameters, the start price and the window, checked before the first draw.
fn check(
    model: &SyntheticOrderBookModel,
    start_price: f64,
    start_ms: i64,
    end_ms: i64,
) -> Result<(), String> {
    let depth = model.limit_rates.len();
    if depth == 0 || model.cancel_rates.len() != depth {
        return Err(format!(
            "the model needs at least one limit rate and one cancel rate per limit rate, not {} \
             and {}",
            depth,
            model.cancel_rates.len()
        ));
    }
    for (i, (l, c)) in model.limit_rates.iter().zip(&model.cancel_rates).enumerate() {
        at_least_zero(&format!("limit_rates[{i}]"), *l)?;
        at_least_zero(&format!("cancel_rates[{i}]"), *c)?;
    }
    at_least_zero("market_rate", model.market_rate)?;
    for (name, x) in [("tick_size", model.tick_size), ("order_size", model.order_size)] {
        if !(x.is_finite() && x > 0.0) {
            return Err(format!("{name} must be finite and above zero, not {x}"));
        }
    }
    if let SyntheticInitialQueues::Poisson(means) = &model.initial_queues {
        for (i, mean) in means.iter().enumerate() {
            at_least_zero(&format!("initial_queues[{i}]"), *mean)?;
            if *mean > MAX_INITIAL_MEAN {
                return Err(format!(
                    "initial_queues[{i}] is {mean} orders on average; at most {MAX_INITIAL_MEAN}"
                ));
            }
        }
    }
    let ticks = start_price / model.tick_size;
    // NaN and infinity fall outside the range too.
    if !(1.0..1e15).contains(&ticks) {
        return Err(format!(
            "the start price {start_price} must be at least one tick ({}) and finite",
            model.tick_size
        ));
    }
    if end_ms <= start_ms {
        return Err(format!("the window must end after it starts, not {start_ms}..{end_ms}"));
    }
    Ok(())
}

/// One event of the model, as [`Book::choose`] picks it; `true` is the bid side.
#[derive(Debug, Clone, Copy)]
enum Event {
    /// A limit order joins the queue at this tick.
    Limit(bool, i64),
    /// A market order takes one unit from this side's best.
    Market(bool),
    /// One resting order at this tick is cancelled.
    Cancel(bool, i64),
}

/// What one applied event changed: the level `(side, tick)` and its order count after, and whether a
/// trade printed.
struct Change {
    bid_side: bool,
    tick: i64,
    count: u64,
    traded: bool,
}

/// The book's state: order counts by tick, per side, and the references distances are measured from.
struct Book<'a> {
    model: &'a SyntheticOrderBookModel,
    bids: BTreeMap<i64, u64>,
    asks: BTreeMap<i64, u64>,
    /// The best bid, or while the bid side is empty, `depth + 1` ticks under the best ask.
    bid_reference: i64,
    /// The best ask, or while the ask side is empty, `depth + 1` ticks over the best bid.
    ask_reference: i64,
}

impl<'a> Book<'a> {
    /// The book at the start: the best bid on the tick under `start_price`, the ask one tick above, and
    /// the initial queues behind them.
    fn open(
        model: &'a SyntheticOrderBookModel,
        start_price: f64,
        draws: &mut Draws,
    ) -> Result<Self, String> {
        let bid = libm::floor(start_price / model.tick_size) as i64;
        let ask = bid + 1;
        let mut book = Book {
            model,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            bid_reference: bid,
            ask_reference: ask,
        };
        let counts: Vec<(u64, u64)> = match &model.initial_queues {
            SyntheticInitialQueues::Poisson(means) => {
                means.iter().map(|mean| (draws.queue(*mean), draws.queue(*mean))).collect()
            }
            SyntheticInitialQueues::Fixed { bids, asks } => (0..bids.len().max(asks.len()))
                .map(|i| (bids.get(i).copied().unwrap_or(0), asks.get(i).copied().unwrap_or(0)))
                .collect(),
        };
        for (distance, (bid_count, ask_count)) in (1_i64..).zip(counts) {
            let bid_tick = ask - distance;
            if bid_tick <= 0 && bid_count > 0 {
                return Err(format!(
                    "the start price {start_price} is too low for a queue {distance} ticks under \
                     the ask: start it higher"
                ));
            }
            if bid_count > 0 {
                book.bids.insert(bid_tick, bid_count);
            }
            if ask_count > 0 {
                book.asks.insert(bid + distance, ask_count);
            }
        }
        book.refresh();
        Ok(book)
    }

    fn depth(&self) -> i64 {
        self.model.limit_rates.len() as i64
    }

    /// The cancel rate per order `distance` ticks from the opposite best (the last beyond the depth).
    fn cancel_rate(&self, distance: i64) -> f64 {
        let i = usize::try_from(distance.clamp(1, self.depth())).unwrap_or(1) - 1;
        self.model.cancel_rates[i]
    }

    /// Re-read the references from the book (the module doc's empty-side rule).
    fn refresh(&mut self) {
        let edge = self.depth() + 1;
        match (self.bids.last_key_value(), self.asks.first_key_value()) {
            (Some((&bid, _)), Some((&ask, _))) => {
                (self.bid_reference, self.ask_reference) = (bid, ask)
            }
            (Some((&bid, _)), None) => (self.bid_reference, self.ask_reference) = (bid, bid + edge),
            (None, Some((&ask, _))) => (self.bid_reference, self.ask_reference) = (ask - edge, ask),
            (None, None) => {}
        }
    }

    /// Every event of the model with its rate, in one fixed order: limit buys and sells by distance,
    /// the two market orders, then the cancels of every bid and every ask level by tick.
    fn for_each_event(&self, mut visit: impl FnMut(Event, f64) -> bool) {
        for (distance, rate) in (1_i64..).zip(&self.model.limit_rates) {
            if !visit(Event::Limit(true, self.ask_reference - distance), *rate)
                || !visit(Event::Limit(false, self.bid_reference + distance), *rate)
            {
                return;
            }
        }
        if !visit(Event::Market(false), self.model.market_rate)
            || !visit(Event::Market(true), self.model.market_rate)
        {
            return;
        }
        for (&tick, &count) in &self.bids {
            let rate = self.cancel_rate(self.ask_reference - tick) * count as f64;
            if !visit(Event::Cancel(true, tick), rate) {
                return;
            }
        }
        for (&tick, &count) in &self.asks {
            let rate = self.cancel_rate(tick - self.bid_reference) * count as f64;
            if !visit(Event::Cancel(false, tick), rate) {
                return;
            }
        }
    }

    /// The sum of every event's rate, in [`Self::for_each_event`]'s order.
    fn total_rate(&self) -> f64 {
        let mut total = 0.0;
        self.for_each_event(|_, rate| {
            total += rate;
            true
        });
        total
    }

    /// The event whose slice of `[0, total]` holds `target`; the last event with a rate if rounding
    /// leaves `target` past the end.
    fn choose(&self, target: f64) -> Event {
        let (mut left, mut chosen) = (target, None);
        self.for_each_event(|event, rate| {
            if rate > 0.0 {
                chosen = Some(event);
                if left <= rate {
                    return false;
                }
                left -= rate;
            }
            true
        });
        chosen.expect("a positive total rate has an event with a rate")
    }

    /// Apply `event` at `minutes`: what it changed, or `None` for a market order against an empty side.
    fn apply(&mut self, event: Event, minutes: f64) -> Result<Option<Change>, String> {
        let (bid_side, tick, traded) = match event {
            Event::Limit(bid_side, tick) => {
                if tick <= 0 {
                    return Err(format!(
                        "at minute {minutes:.3} the book's price walked to {tick} ticks; start it higher"
                    ));
                }
                *self.side(bid_side).entry(tick).or_insert(0) += 1;
                (bid_side, tick, false)
            }
            Event::Market(bid_side) => {
                let best =
                    if bid_side { self.bids.last_key_value() } else { self.asks.first_key_value() };
                let Some((&tick, _)) = best else { return Ok(None) };
                self.take_one(bid_side, tick);
                (bid_side, tick, true)
            }
            Event::Cancel(bid_side, tick) => {
                self.take_one(bid_side, tick);
                (bid_side, tick, false)
            }
        };
        let count = self.side(bid_side).get(&tick).copied().unwrap_or(0);
        self.refresh();
        Ok(Some(Change { bid_side, tick, count, traded }))
    }

    fn side(&mut self, bid_side: bool) -> &mut BTreeMap<i64, u64> {
        if bid_side { &mut self.bids } else { &mut self.asks }
    }

    /// One order fewer at `tick`; the level goes when it empties.
    fn take_one(&mut self, bid_side: bool, tick: i64) {
        let side = self.side(bid_side);
        if let Some(count) = side.get_mut(&tick) {
            *count -= 1;
            if *count == 0 {
                side.remove(&tick);
            }
        }
    }

    fn price(&self, tick: i64) -> f64 {
        tick as f64 * self.model.tick_size
    }

    fn level(&self, tick: i64, count: u64) -> L2BookLevel {
        L2BookLevel::new(self.price(tick), count as f64 * self.model.order_size)
    }

    /// `(best bid tick, its count, best ask tick, its count)` when both sides hold orders.
    fn top(&self) -> Option<(i64, u64, i64, u64)> {
        let (&bid, &bid_count) = self.bids.last_key_value()?;
        let (&ask, &ask_count) = self.asks.first_key_value()?;
        Some((bid, bid_count, ask, ask_count))
    }

    fn quote(&self, top: Option<(i64, u64, i64, u64)>, timestamp: i64) -> Option<TickQuote> {
        let (bid, bid_count, ask, ask_count) = top?;
        Some(TickQuote {
            timestamp,
            local_timestamp: 0,
            bid: self.price(bid),
            ask: self.price(ask),
            bid_size: bid_count as f64 * self.model.order_size,
            ask_size: ask_count as f64 * self.model.order_size,
            symbol: Default::default(),
        })
    }
}

impl Draws {
    /// A Poisson queue size with mean `mean >= 0` (zero draws for a zero mean).
    fn queue(&mut self, mean: f64) -> u64 {
        if mean > 0.0 { self.poisson(mean) } else { 0 }
    }
}

#[path = "order_book_tests.rs"]
#[cfg(test)]
mod order_book_tests;
