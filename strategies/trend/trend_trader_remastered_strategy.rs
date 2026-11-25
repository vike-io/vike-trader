// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trend Trader-Remastered Strategy" by aybarsm, https://www.tradingview.com/script/En4PabIp/
// The original's notice: © aybarsm
// Ported into this file: the library "tac" (aybarsm/tac/21), MPL-2.0; its notice: © aybarsm
// Ported into this file: the library "ToolsPos" (aybarsm/ToolsPos/10), MPL-2.0; its notice: © aybarsm
// Ported into this file: the library "ToolsFluent" (aybarsm/ToolsFluent/6), MPL-2.0; its notice: © aybarsm
// Changed: rewritten by hand in Rust for vike.

//! A parabolic SAR (start 0.02, step 0.02, cap 0.025) of the libraries' own making, with a lagging line (the SAR before the
//! bar's flip) and a fast one (the lagging line moved one acceleration step on). While the lagging line stood over the bar
//! before's high, a long stop entry of 10% of the account's worth rests at the fast line, restated every bar; while it stood
//! under that low, a short one; each reverses the other.
//!
//! Beside it, a signal book: the lagging line crossing up through the bar's high after standing over the previous one opens a
//! buy side at that line, crossing down through the low a sell side, the other side's crossing ends it. Bill Williams'
//! fractals (five bars, the middle one highest or lowest) are marked higher highs and lower lows against the fractal before,
//! and one is broken out when a bar after it closes past it in its own colour (the two newest unbroken ones checked, or more
//! while they are newer than the side). On a bar that turns against the bar before, a broken-out higher high newer than the
//! side gives a buy side a take-profit sign (the close over the side's price and over the last sign's) and a sell side a
//! re-entry sign (after a take-profit sign, the close over the last re-entry's); a lower low the mirror. Each fractal signs
//! once per kind.
//!
//! On a take-profit sign the rule closes 10% of the position at the bar's close, from the trade that opened it; on a
//! re-entry sign, with the position under half of what that trade opened and under three quarters of it, a stop order at
//! the close for the difference to three quarters, in the direction of the trade that opened it. A filled re-entry is then
//! the trade the rule reads, and as its name is not the long entry's, its own re-entry goes the other way, as the original's
//! does. 0.05% commission; version 6: starts with 10,000 in a USD account, margined at 100% (`main.rs`'s rule 4). **Does not
//! reproduce its reference** (2015 trades, net -253.325546 on BTCUSDT 30m, 2025-01-01..2026-07-30): this file gives 2015 /
//! -253.262258, the net 0.063 over with no cause found (the take profit's and the re-entry's quantities rounded each other
//! way give no nearer net; this file leaves the re-entry's unrounded); the owner accepted those numbers on 2026-11-15.
//!
//! The libraries' lists keep a fractal object by reference: a breakout writes the object and inserts it again at its place
//! (the list holds it twice), and a bar that is both a fractal high and a fractal low makes one object, priced at its high, in
//! both lists; the search for a fractal already signed is a binary search over a list in signing order, which is not sorted.
//! All three are kept as the original has them. What only draws (shapes and lines) is left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SAR_START: f64 = 0.02;
const SAR_STEP: f64 = 0.02;
const SAR_MAX: f64 = 0.025;
const BREAKOUT_LOOKBACK: usize = 2;
const SHARE: f64 = 10.0;
const TAKE_PROFIT_REDUCE: f64 = 10.0;
const RE_WHEN: f64 = 50.0;
const RE_FILL: f64 = 75.0;

/// The libraries' parabolic SAR: its value, the lagging line and the fast one.
struct Sar {
    result: f64,
    max_min: f64,
    acceleration: f64,
    below: bool,
}

impl Sar {
    fn next(&mut self, index: usize, bar: &Bar, back: &Back) -> (f64, f64, f64) {
        let mut first = false;
        if index == 1 {
            if bar.close > back.close(1) {
                self.below = true;
                self.max_min = bar.high;
                self.result = back.low(1);
            } else {
                self.below = false;
                self.max_min = bar.low;
                self.result = back.high(1);
            }
            first = true;
            self.acceleration = SAR_START;
        }
        self.result += self.acceleration * (self.max_min - self.result);
        let stage0 = self.result;
        if self.below {
            if self.result > bar.low {
                first = true;
                self.below = false;
                self.result = bar.high.max(self.max_min);
                self.max_min = bar.low;
                self.acceleration = SAR_START;
            }
        } else if self.result < bar.high {
            first = true;
            self.below = true;
            self.result = bar.low.min(self.max_min);
            self.max_min = bar.high;
            self.acceleration = SAR_START;
        }
        if !first {
            if self.below {
                if bar.high > self.max_min {
                    self.max_min = bar.high;
                    self.acceleration = (self.acceleration + SAR_STEP).min(SAR_MAX);
                }
            } else if bar.low < self.max_min {
                self.max_min = bar.low;
                self.acceleration = (self.acceleration + SAR_STEP).min(SAR_MAX);
            }
        }
        if self.below {
            self.result = nan_min(self.result, back.low(1));
            if index > 1 {
                self.result = nan_min(self.result, back.low(2));
            }
        } else {
            self.result = nan_max(self.result, back.high(1));
            if index > 1 {
                self.result = nan_max(self.result, back.high(2));
            }
        }
        let lagging = if index > 1 && first { stage0 } else { self.result };
        let fast = if lagging.is_nan() {
            f64::NAN
        } else {
            lagging + self.acceleration * (self.max_min - lagging)
        };
        (self.result, lagging, fast)
    }
}

/// The original's `math.max`/`math.min`: a missing value makes it missing.
fn nan_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

fn nan_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// The last five bars, newest first.
#[derive(Default)]
struct Back(Vec<Bar>);

impl Back {
    fn push(&mut self, bar: &Bar) {
        self.0.insert(0, bar.clone());
        self.0.truncate(5);
    }
    fn get(&self, i: usize, f: impl Fn(&Bar) -> f64) -> f64 {
        self.0.get(i).map_or(f64::NAN, f)
    }
    fn high(&self, i: usize) -> f64 {
        self.get(i, |b| b.high)
    }
    fn low(&self, i: usize) -> f64 {
        self.get(i, |b| b.low)
    }
    fn close(&self, i: usize) -> f64 {
        self.get(i, |b| b.close)
    }
    fn open(&self, i: usize) -> f64 {
        self.get(i, |b| b.open)
    }
    fn time(&self, i: usize) -> Option<i64> {
        self.0.get(i).map(|b| b.timestamp)
    }
}

/// One fractal object, which the lists hold by reference (an index into `Fractals::objects`).
struct Fractal {
    price: f64,
    when: i64,
    breakout_time: Option<i64>,
}

#[derive(Default)]
struct Fractals {
    objects: Vec<Fractal>,
    higher_highs: Vec<usize>,
    lower_lows: Vec<usize>,
    /// The high and the low of the last fractal high and low, the value `valuewhen(.., 1)` reads.
    last_high: Option<f64>,
    last_low: Option<f64>,
}

impl Fractals {
    /// One bar of the library's `eval`: marks a fractal two bars back, then the breakouts.
    fn eval(&mut self, back: &Back, start: i64) {
        let h = |i| back.high(i);
        let l = |i| back.low(i);
        let is_high = h(4) < h(2) && h(3) <= h(2) && h(2) >= h(1) && h(2) > h(0);
        let is_low = l(4) > l(2) && l(3) >= l(2) && l(2) <= l(1) && l(2) < l(0);
        let mut mark_high = 0;
        if is_high {
            if let Some(last) = self.last_high {
                mark_high = if h(2) > last {
                    2
                } else if h(2) < last {
                    1
                } else {
                    0
                };
            }
            self.last_high = Some(h(2));
        }
        let mut mark_low = 0;
        if is_low {
            if let Some(last) = self.last_low {
                mark_low = if l(2) < last {
                    -2
                } else if l(2) > last {
                    -1
                } else {
                    0
                };
            }
            self.last_low = Some(l(2));
        }
        let (high, low) = (mark_high > 0, mark_low < 0);
        if high || low {
            let price = if high { h(2) } else { l(2) };
            let when = back.time(2).expect("a fractal two bars back");
            self.objects.push(Fractal { price, when, breakout_time: None });
            let object = self.objects.len() - 1;
            if mark_high == 2 {
                self.higher_highs.push(object);
            }
            if mark_low == -2 {
                self.lower_lows.push(object);
            }
        }
        let (close, open) = (back.close(1), back.open(1));
        let time = back.time(1);
        self.breakouts(true, mark_high == 2, start, close, open, time);
        self.breakouts(false, mark_low == -2, start, close, open, time);
    }

    /// The breakout walk of one list, newest first, as the library runs it (its limit counts the low lookback for both).
    fn breakouts(
        &mut self,
        highs: bool,
        marked: bool,
        start: i64,
        close: f64,
        open: f64,
        time: Option<i64>,
    ) {
        let list = if highs { &mut self.higher_highs } else { &mut self.lower_lows };
        if list.is_empty() {
            return;
        }
        let limit = BREAKOUT_LOOKBACK + usize::from(marked);
        let mut counter = 0;
        let mut i = list.len() - 1;
        loop {
            counter += 1;
            let object = list[i];
            let fractal = &mut self.objects[object];
            if fractal.when < start && counter > limit {
                break;
            }
            let broken = if highs {
                close > open && close > fractal.price
            } else {
                close < open && close < fractal.price
            };
            if fractal.breakout_time.is_none() && broken {
                fractal.breakout_time = time;
                list.insert(i, object);
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Way {
    Buy,
    Sell,
}

/// The signal book's side, the library's `PosInfo`.
struct Side {
    way: Option<Way>,
    price: f64,
    when: Option<i64>,
    take_profits: Vec<f64>,
    re_entries: Vec<f64>,
    marks_take_profit: Vec<i64>,
    marks_re_entry: Vec<i64>,
}

/// The original's `array.binary_search`: the index of `value`, or -1, searched as if the list were sorted.
fn binary_search(list: &[i64], value: i64) -> i64 {
    let (mut low, mut high) = (0_i64, list.len() as i64 - 1);
    while low <= high {
        let middle = (low + high) / 2;
        let at = list[middle as usize];
        if at == value {
            return middle;
        }
        if at < value {
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }
    -1
}

struct S {
    index: usize,
    back: Back,
    sar: Sar,
    lagging_before: f64,
    fractals: Fractals,
    side: Side,
    init_size: f64,
    init_name: Option<String>,
    re_trigger: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        index: 0,
        back: Back::default(),
        sar: Sar { result: f64::NAN, max_min: f64::NAN, acceleration: f64::NAN, below: false },
        lagging_before: f64::NAN,
        fractals: Fractals::default(),
        side: Side {
            way: None,
            price: f64::NAN,
            when: None,
            take_profits: Vec::new(),
            re_entries: Vec::new(),
            marks_take_profit: Vec::new(),
            marks_re_entry: Vec::new(),
        },
        init_size: f64::NAN,
        init_name: None,
        re_trigger: f64::NAN,
    })
}

/// The price a stop entry at `level` is sized from: its level on the grid, or this close when the close is already through it.
fn sized_at(strategy: &Tactics<'_>, side: OrderSide, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(side, level);
    if (side == Long && level <= close) || (side == Short && level >= close) {
        close
    } else {
        level
    }
}

impl S {
    /// The signal book's take-profit and re-entry signs for this bar, with the fractal each came from.
    fn signs(&mut self, bar: &Bar, proximity: f64, way: Way) -> (bool, bool, Option<i64>) {
        let back = &self.back;
        let up_eligible = back.close(1) > back.open(1) && bar.close <= bar.open;
        let down_eligible = back.close(1) < back.open(1) && bar.close >= bar.open;
        let last_take_profit = self.side.take_profits.last().copied();
        let last_re_entry = self.side.re_entries.last().copied();
        let side_when = self.side.when;
        let after_side = |time: Option<i64>| match (time, side_when) {
            (Some(t), Some(w)) => t >= w,
            (Some(_), None) => true,
            (None, _) => false,
        };
        let (mut take_profit, mut re_entry, mut from) = (false, false, None);
        let highs = &self.fractals.higher_highs;
        if !highs.is_empty()
            && up_eligible
            && (way == Way::Buy || (way == Way::Sell && last_take_profit.is_some()))
        {
            for &object in &highs[highs.len().saturating_sub(BREAKOUT_LOOKBACK)..] {
                let fractal = &self.fractals.objects[object];
                if !after_side(fractal.breakout_time) {
                    continue;
                }
                take_profit = way == Way::Buy
                    && bar.close > self.side.price
                    && binary_search(&self.side.marks_take_profit, fractal.when) == -1
                    && last_take_profit.is_none_or(|p| bar.close > p);
                re_entry = way == Way::Sell
                    && proximity > 0.0
                    && binary_search(&self.side.marks_re_entry, fractal.when) == -1
                    && last_take_profit.is_some()
                    && last_re_entry.is_none_or(|p| bar.close > p);
                if take_profit || re_entry {
                    from = Some(fractal.when);
                    break;
                }
            }
        }
        let lows = &self.fractals.lower_lows;
        if !lows.is_empty()
            && down_eligible
            && (way == Way::Sell || (way == Way::Buy && last_take_profit.is_some()))
        {
            for &object in &lows[lows.len().saturating_sub(BREAKOUT_LOOKBACK)..] {
                let fractal = &self.fractals.objects[object];
                if !after_side(fractal.breakout_time) {
                    continue;
                }
                take_profit = way == Way::Sell
                    && bar.close < self.side.price
                    && binary_search(&self.side.marks_take_profit, fractal.when) == -1
                    && last_take_profit.is_none_or(|p| bar.close < p);
                re_entry = way == Way::Buy
                    && proximity > 0.0
                    && binary_search(&self.side.marks_re_entry, fractal.when) == -1
                    && last_take_profit.is_some()
                    && last_re_entry.is_none_or(|p| bar.close < p);
                if take_profit || re_entry {
                    from = Some(fractal.when);
                    break;
                }
            }
        }
        (take_profit, re_entry, from)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let index = self.index;
        self.index += 1;
        // `back` reads as the original's history operator from here: 0 is this bar, 1 the bar before.
        self.back.push(bar);
        let (_, lagging, fast) = self.sar.next(index, bar, &self.back);
        let before = std::mem::replace(&mut self.lagging_before, lagging);
        let (high_before, low_before) = (self.back.high(1), self.back.low(1));

        let proximity = ((bar.close - lagging).abs() / lagging * 100.0 * 100.0).round() / 100.0;
        let cross_high = bar.high > lagging && high_before <= before;
        let cross_low = bar.low < lagging && low_before >= before;
        let lag_cross = if before > high_before && cross_high {
            1
        } else if before < low_before && cross_low {
            -1
        } else {
            0
        };
        let enter = match lag_cross {
            1 => Some(Way::Buy),
            -1 => Some(Way::Sell),
            _ => None,
        };
        let exit = match (self.side.way, lag_cross) {
            (Some(Way::Buy), -1) | (Some(Way::Sell), 1) => self.side.way,
            _ => None,
        };
        let held = if enter.is_none() && exit.is_none() { self.side.way } else { None };
        if exit.is_some() {
            self.side.way = None;
            self.side.price = f64::NAN;
            self.side.when = None;
            self.side.take_profits.clear();
            self.side.re_entries.clear();
        }
        if let Some(way) = enter {
            self.side.way = Some(way);
            self.side.price = lagging;
            self.side.when = Some(bar.timestamp);
            self.side.take_profits.clear();
            self.side.re_entries.clear();
            self.side.marks_take_profit.clear();
            self.side.marks_re_entry.clear();
        }
        // The side's start, else this bar's time, bounds the breakout walk.
        let start = self.side.when.unwrap_or(bar.timestamp);
        self.fractals.eval(&self.back, start);
        let (mut take_profit, mut re_entry) = (false, false);
        if let Some(way) = held {
            let (take, again, from) = self.signs(bar, proximity, way);
            (take_profit, re_entry) = (take, again);
            if let Some(when) = from {
                if take_profit {
                    self.side.take_profits.push(bar.close);
                    self.side.marks_take_profit.push(when);
                } else {
                    self.side.re_entries.push(bar.close);
                    self.side.marks_re_entry.push(when);
                }
            }
        }

        // The strategy: a stop entry while the lagging line stood past the bar before.
        let size = strategy.position().abs();
        let enter_long = before > high_before;
        let enter_short = before < low_before;
        if enter_long || enter_short {
            let (name, way) = if enter_long { ("Long", Long) } else { ("Short", Short) };
            let worth =
                share_of_worth(&strategy.sizing(), SHARE, sized_at(strategy, way, fast, bar.close));
            strategy.entry(name, way).size(worth).stop(fast);
        }
        if let Some(lot) = strategy.lots().last().filter(|lot| lot.entry_timestamp == bar.timestamp)
        {
            let name = lot.name.clone();
            self.init_size = strategy.position().abs();
            self.init_name = Some(name);
            self.re_trigger = self.init_size / 100.0 * RE_WHEN;
        }
        let Some(name) = self.init_name.clone() else { return };
        if strategy.lots().is_empty() {
            return;
        }
        let step = strategy.sizing().qty_step();
        if take_profit {
            let qty = floor_share(size / 100.0 * TAKE_PROFIT_REDUCE, size, step);
            strategy.close(&name).size(qty).here();
        } else if re_entry && size < self.re_trigger && size < self.init_size / 100.0 * RE_FILL {
            let qty = self.init_size / 100.0 * RE_FILL - size;
            let way = if name == "Long" { Long } else { Short };
            strategy.order(&format!("RE-{name}"), way).size(qty).stop(bar.close);
        }
    }
}
