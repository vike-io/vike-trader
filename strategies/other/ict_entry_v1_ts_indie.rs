// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ICT Entry V1 [TS_Indie]" by Truth_Strategy_Indie, https://www.tradingview.com/script/mVdZBLJ7/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-01-02, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A sweep of liquidity, then a limit entry at the fair value gap, in the direction of the internal structure: all of the
//! account's worth on a limit, with a stop half an average range past the gap and a target 4 times as far the other way. No
//! costs; starts with 100,000, no margin. The script is version 6 and states no size, which its language reads as 100% of
//! equity, priced at the limit.
//!
//! **Gaps.** From the third bar, a low above the high two bars back is a demand gap (from that high up to this low), a
//! high below the low two bars back a supply gap. Each list is newest first. A demand gap is dropped from the front while
//! the low reaches its bottom, a supply gap while the high reaches its top. Every gap is also kept until a close goes
//! through it: a close above a supply gap's top moves it, newest first and while the closes keep going through, to the
//! front of the inverted demand list, a close below a demand gap's bottom to the inverted supply list; an inverted demand
//! gap is dropped while the low reaches its bottom, an inverted supply gap while the high reaches its top.
//!
//! **Liquidity.** Swings alternate on a 5-bar pivot each way. A pivot high after a low records it; another low before any
//! high records it and takes as the high the running high of 6 bars back; both on one bar go by the pivot bar's candle. On
//! each change the running high and low restart from the highest and lowest of the last 5 bars (4 on the side not being
//! looked for). A new swing high not below that highest is a level above, a new swing low not above that lowest a level
//! below. A low reaching the newest level below takes it, and every next one it also reaches; the last taken is the swept
//! level. The levels above mirror it.
//!
//! **Structure.** The internal structure's swing points need a 1-bar pivot and a close beyond the extreme bar's opposite
//! end; a close beyond the confirmed high or low is a break, which turns or continues the trend. Longs only while it is
//! up, shorts only while it is down.
//!
//! **Signals.** When a sweep below closes and opens above the swept level while flat, a buy is armed (and a pending sell
//! cancelled, a sell setup dropped); a close below the swept level disarms it. It fires on a bar whose newest demand gap
//! lies above the swept level, the trend being up, when it was armed at the last bar's end, the newest inverted demand
//! gap's bottom is at or above the demand gap's top, and the space between them is 0.02 to 0.07 average ranges. Then any
//! pending entry is cancelled and a limit buy placed at the gap's top, its stop half an average range under the gap's
//! bottom. The average range is the 50-bar exponential average of the true range. A sell mirrors it. A pending entry is
//! the script's: its flag drops when the position is on its side or any trade closed, after which a later signal no
//! longer cancels it, so it may still fill.
//!
//! Each entry and its exit have their own number, as in the script. The script's session filter is off by default and its
//! date window is all time, so neither is ported; the boxes, lines, labels, alerts and the result table are drawing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::{ConfirmedPivot, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const REWARD: f64 = 4.0;
const RANGE_LENGTH: f64 = 50.0;
const STOP_RANGES: f64 = 0.5;
/// How deep into the gap the entry sits, in percent of it from its near side.
const ENTRY_DEPTH: f64 = 0.0;
const SPACE_MIN: f64 = 0.02;
const SPACE_MAX: f64 = 0.07;
const SWING_STRENGTH: usize = 5;
const STRUCTURE_STRENGTH: f64 = 1.0;
/// Every list the script keeps holds this many.
const KEPT: usize = 9000;

#[derive(Clone, Copy)]
struct Candle {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

#[derive(Clone, Copy)]
struct Zone {
    top: f64,
    bottom: f64,
}

/// Adds `x` at the front, dropping the oldest past `KEPT`.
fn record<T>(list: &mut VecDeque<T>, x: T) {
    list.push_front(x);
    list.truncate(KEPT);
}

/// Drops from the front while `reached` holds for the front.
fn drop_reached(list: &mut VecDeque<Zone>, reached: impl Fn(&Zone) -> bool) {
    while list.front().is_some_and(&reached) {
        list.pop_front();
    }
}

/// Moves the front of `from` to the front of `to` while `through` holds for it.
fn invert(from: &mut VecDeque<Zone>, to: &mut VecDeque<Zone>, through: impl Fn(&Zone) -> bool) {
    while let Some(zone) = from.front().copied().filter(&through) {
        from.pop_front();
        record(to, zone);
    }
}

/// Takes levels off the front while `reached` holds: the last one taken.
fn sweep(levels: &mut VecDeque<f64>, reached: impl Fn(f64) -> bool) -> Option<f64> {
    let mut last = None;
    while let Some(level) = levels.front().copied().filter(|&l| reached(l)) {
        levels.pop_front();
        last = Some(level);
    }
    last
}

/// The highest high (`high`) or lowest low of the last `length` candles, NaN before there are that many.
fn extreme(candles: &[Candle], length: usize, high: bool) -> f64 {
    if candles.len() < length {
        return f64::NAN;
    }
    let last = &candles[candles.len() - length..];
    if high {
        last.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max)
    } else {
        last.iter().map(|c| c.low).fold(f64::INFINITY, f64::min)
    }
}

/// The swings that place the liquidity levels.
struct Swings {
    pivots: ConfirmedPivot,
    /// 1 when a high is looked for next, -1 a low, 0 before the first pivot.
    direction: i32,
    high: f64,
    low: f64,
    running_high: f64,
    running_low: f64,
    /// The running high and low at the end of each past bar, newest first.
    history: VecDeque<(f64, f64)>,
}

impl Swings {
    /// One bar (the last of `candles`): the new level above and the new level below, if any.
    fn step(&mut self, candles: &[Candle]) -> (Option<f64>, Option<f64>) {
        let candle = candles[candles.len() - 1];
        let [pivot_high, pivot_low, _, _] = self.pivots.next_hl(candle.high, candle.low);
        let (has_high, has_low) = (!pivot_high.is_nan(), !pivot_low.is_nan());
        // the running high and low of `SWING_STRENGTH + 1` bars back
        let (high_back, low_back) =
            self.history.get(SWING_STRENGTH).copied().unwrap_or((f64::NAN, f64::NAN));
        let (mut skip, mut changed_high, mut changed_low) = (false, false, false);
        if self.high.is_nan() && has_high {
            (self.high, self.direction) = (pivot_high, -1);
        }
        if self.low.is_nan() && has_low {
            (self.low, self.direction) = (pivot_low, 1);
        }
        if candle.high > self.running_high || self.running_high.is_nan() {
            self.running_high = candle.high;
        }
        if candle.low < self.running_low || self.running_low.is_nan() {
            self.running_low = candle.low;
        }
        if has_high && has_low {
            let pivot_bar = candles[candles.len() - 1 - SWING_STRENGTH];
            if self.direction == 1 {
                (self.low, self.high) = (pivot_low, pivot_high);
                self.direction = if pivot_bar.close > pivot_bar.open { -1 } else { 1 };
                skip = true;
            }
            if self.direction == -1 && !skip {
                (self.low, self.high) = (pivot_low, pivot_high);
                self.direction = if pivot_bar.close < pivot_bar.open { 1 } else { -1 };
                skip = true;
            }
            (changed_low, changed_high) = (true, true);
        }
        if self.direction == 1 && !skip {
            if has_high {
                self.high = pivot_high;
                (changed_high, self.direction, skip) = (true, -1, true);
            }
            if has_low {
                (self.low, self.high) = (pivot_low, high_back);
                (changed_low, changed_high, self.direction, skip) = (true, true, 1, true);
            }
        }
        if self.direction == -1 && !skip {
            if has_low {
                self.low = pivot_low;
                (changed_low, self.direction) = (true, 1);
            }
            if has_high {
                (self.high, self.low) = (pivot_high, low_back);
                (changed_low, changed_high, self.direction) = (true, true, -1);
            }
        }
        let highest = extreme(candles, SWING_STRENGTH - usize::from(self.direction != 1), true);
        let lowest = extreme(candles, SWING_STRENGTH - usize::from(self.direction != -1), false);
        if changed_high || changed_low {
            (self.running_low, self.running_high) = (lowest, highest);
        }
        record(&mut self.history, (self.running_high, self.running_low));
        self.history.truncate(SWING_STRENGTH + 1);
        let above = (changed_high && self.high >= highest).then_some(self.high);
        let below = (changed_low && self.low <= lowest).then_some(self.low);
        (above, below)
    }
}

/// The internal structure: its trend is 1 up, -1 down, 0 before it is known.
struct Structure {
    pivots: ConfirmedPivot,
    trend: i32,
    /// 1 when the next swing low is being confirmed, -1 the next swing high.
    leg: i32,
    high: f64,
    low: f64,
    high_at: usize,
    low_at: usize,
    /// The extremes reached since the last confirmed swing point, and their bars.
    reach_high: f64,
    reach_low: f64,
    reach_high_at: usize,
    reach_low_at: usize,
    high_confirmed: bool,
    low_confirmed: bool,
    pulled_back_from_high: bool,
    pulled_back_from_low: bool,
    pivot_since_high: bool,
    pivot_since_low: bool,
    firsts: i32,
    /// `reach_high`, `reach_low` and their bars at the last bar's end.
    previous: (f64, f64, usize, usize),
}

impl Structure {
    fn step(&mut self, candles: &[Candle]) {
        let n = candles.len() - 1;
        let candle = candles[n];
        let [pivot_high, pivot_low, _, _] = self.pivots.next_hl(candle.high, candle.low);
        let (pivot_high, pivot_low) = (!pivot_high.is_nan(), !pivot_low.is_nan());
        self.pivot_since_high |= pivot_high;
        self.pivot_since_low |= pivot_low;
        if candle.high > self.reach_high || self.reach_high.is_nan() {
            (self.reach_high, self.reach_high_at) = (candle.high, n);
            (self.pulled_back_from_high, self.pivot_since_high) = (false, false);
        }
        if candle.low < self.reach_low || self.reach_low.is_nan() {
            (self.reach_low, self.reach_low_at) = (candle.low, n);
            (self.pulled_back_from_low, self.pivot_since_low) = (false, false);
        }
        if candle.close < candles[self.reach_high_at].low {
            self.pulled_back_from_high = true;
        }
        if candle.close > candles[self.reach_low_at].high {
            self.pulled_back_from_low = true;
        }
        if self.high_confirmed && self.low_confirmed {
            (self.pulled_back_from_high, self.pulled_back_from_low) = (false, false);
            (self.pivot_since_high, self.pivot_since_low) = (false, false);
        }
        let breaks_up = candle.close > self.high;
        let breaks_down = candle.close < self.low;
        let (mut change_high, mut change_low) = (false, false);
        let (mut turned_down, mut turned_up, mut went_on_down, mut went_on_up) =
            (false, false, false, false);
        if self.trend == 1 {
            if breaks_down && self.low_confirmed {
                (self.high_confirmed, self.low_confirmed) = (true, false);
                (change_high, turned_down) = (true, true);
            }
            if !self.high.is_nan()
                && self.leg == 1
                && self.pulled_back_from_high
                && self.pivot_since_high
            {
                (self.high_confirmed, change_high) = (true, true);
            }
            if self.leg == -1 && breaks_up && self.high_confirmed {
                (self.high_confirmed, self.low_confirmed) = (false, true);
                (change_low, went_on_up) = (true, true);
            }
        }
        if self.trend == -1 {
            if breaks_up && self.high_confirmed {
                (self.high_confirmed, self.low_confirmed) = (false, true);
                (change_low, turned_up) = (true, true);
            }
            if !self.low.is_nan()
                && self.leg == -1
                && self.pulled_back_from_low
                && self.pivot_since_low
            {
                (self.low_confirmed, change_low) = (true, true);
            }
            if self.leg == 1 && breaks_down && self.low_confirmed {
                (self.high_confirmed, self.low_confirmed) = (true, false);
                (change_high, went_on_down) = (true, true);
            }
        }
        if change_low {
            (self.low, self.low_at, self.leg) = (self.reach_low, self.reach_low_at, 1);
        }
        if change_high {
            (self.high, self.high_at, self.leg) = (self.reach_high, self.reach_high_at, -1);
        }
        if self.high.is_nan() && !self.reach_high.is_nan() && pivot_high {
            (self.reach_low, self.reach_low_at, self.high_confirmed) = (candle.low, n, true);
            self.firsts += 1;
            (self.high, self.high_at, self.leg) = (self.reach_high, self.reach_high_at, -1);
            if self.firsts == 2 {
                self.leg = 1;
            }
            if !self.low_confirmed {
                self.trend = 1;
            }
        }
        if self.low.is_nan() && !self.reach_low.is_nan() && pivot_low {
            (self.reach_high, self.reach_high_at, self.low_confirmed) = (candle.high, n, true);
            self.firsts += 1;
            (self.low, self.low_at, self.leg) = (self.reach_low, self.reach_low_at, 1);
            if self.firsts == 2 {
                self.leg = -1;
            }
            if !self.high_confirmed {
                self.trend = -1;
            }
        }
        if self.high_confirmed && self.low_confirmed {
            self.firsts = 0;
        }
        let (previous_high, previous_low, previous_high_at, previous_low_at) = self.previous;
        if (!self.high_confirmed && candle.high >= previous_high) || turned_up || went_on_up {
            self.reach_low = f64::NAN;
        }
        if (!self.low_confirmed && candle.low <= previous_low) || turned_down || went_on_down {
            self.reach_high = f64::NAN;
        }
        if (turned_up || went_on_up) && self.low_at > previous_high_at {
            (self.reach_high, self.reach_high_at) = (candle.high, n);
        }
        if (turned_down || went_on_down) && self.high_at > previous_low_at {
            (self.reach_low, self.reach_low_at) = (candle.low, n);
        }
        let mut turned = false;
        if breaks_down && self.high_confirmed && self.trend == 1 {
            (self.trend, turned) = (-1, true);
        }
        if !turned && breaks_up && self.low_confirmed && self.trend == -1 {
            self.trend = 1;
        }
        self.previous = (self.reach_high, self.reach_low, self.reach_high_at, self.reach_low_at);
    }
}

struct S {
    candles: Vec<Candle>,
    average_range: Ema,
    swings: Swings,
    structure: Structure,
    demand: VecDeque<Zone>,
    supply: VecDeque<Zone>,
    demand_kept: VecDeque<Zone>,
    supply_kept: VecDeque<Zone>,
    inverted_demand: VecDeque<Zone>,
    inverted_supply: VecDeque<Zone>,
    levels_above: VecDeque<f64>,
    levels_below: VecDeque<f64>,
    swept_above: f64,
    swept_below: f64,
    up_trend: bool,
    down_trend: bool,
    buy_armed: bool,
    sell_armed: bool,
    buy_armed_before: bool,
    sell_armed_before: bool,
    pending_buy: bool,
    pending_sell: bool,
    buys: usize,
    sells: usize,
    closed: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let swings = Swings {
        pivots: ConfirmedPivot::new(SWING_STRENGTH as f64, SWING_STRENGTH as f64)
            .expect("valid spans"),
        direction: 0,
        high: f64::NAN,
        low: f64::NAN,
        running_high: f64::NAN,
        running_low: f64::NAN,
        history: VecDeque::with_capacity(SWING_STRENGTH + 2),
    };
    let structure = Structure {
        pivots: ConfirmedPivot::new(STRUCTURE_STRENGTH, STRUCTURE_STRENGTH).expect("valid spans"),
        trend: 0,
        leg: 0,
        high: f64::NAN,
        low: f64::NAN,
        high_at: 0,
        low_at: 0,
        reach_high: f64::NAN,
        reach_low: f64::NAN,
        reach_high_at: 0,
        reach_low_at: 0,
        high_confirmed: false,
        low_confirmed: false,
        pulled_back_from_high: false,
        pulled_back_from_low: false,
        pivot_since_high: false,
        pivot_since_low: false,
        firsts: 0,
        previous: (f64::NAN, f64::NAN, 0, 0),
    };
    Box::new(S {
        candles: Vec::new(),
        average_range: Ema::new(RANGE_LENGTH).expect("a valid length"),
        swings,
        structure,
        demand: VecDeque::new(),
        supply: VecDeque::new(),
        demand_kept: VecDeque::new(),
        supply_kept: VecDeque::new(),
        inverted_demand: VecDeque::new(),
        inverted_supply: VecDeque::new(),
        levels_above: VecDeque::new(),
        levels_below: VecDeque::new(),
        swept_above: f64::NAN,
        swept_below: f64::NAN,
        up_trend: false,
        down_trend: false,
        buy_armed: false,
        sell_armed: false,
        buy_armed_before: false,
        sell_armed_before: false,
        pending_buy: false,
        pending_sell: false,
        buys: 0,
        sells: 0,
        closed: 0,
    })
}

fn long_name(number: usize) -> String {
    format!("Long {number}")
}

fn short_name(number: usize) -> String {
    format!("Short {number}")
}

impl S {
    /// The gaps and their inversions, from the third bar.
    fn gaps(&mut self) {
        let n = self.candles.len() - 1;
        let (candle, two_back) = (self.candles[n], self.candles[n - 2]);
        drop_reached(&mut self.demand, |z| candle.low <= z.bottom);
        if candle.low > two_back.high {
            let zone = Zone { top: candle.low, bottom: two_back.high };
            record(&mut self.demand, zone);
            record(&mut self.demand_kept, zone);
        }
        drop_reached(&mut self.supply, |z| candle.high >= z.top);
        if candle.high < two_back.low {
            let zone = Zone { top: two_back.low, bottom: candle.high };
            record(&mut self.supply, zone);
            record(&mut self.supply_kept, zone);
        }
        drop_reached(&mut self.inverted_demand, |z| candle.low <= z.bottom);
        invert(&mut self.supply_kept, &mut self.inverted_demand, |z| candle.close > z.top);
        drop_reached(&mut self.inverted_supply, |z| candle.high >= z.top);
        invert(&mut self.demand_kept, &mut self.inverted_supply, |z| candle.close < z.bottom);
    }

    /// The gap a buy enters at, when the gap check passes this bar.
    fn buy_zone(&self, average_range: f64) -> Option<Zone> {
        let gap = *self.demand.front()?;
        if !(self.swept_below < gap.bottom && self.up_trend && self.buy_armed_before) {
            return None;
        }
        let inverted = self.inverted_demand.front().map_or(f64::NAN, |z| z.bottom);
        let space = (inverted - gap.top).abs();
        let fits = space >= average_range * SPACE_MIN && space <= average_range * SPACE_MAX;
        (fits && inverted >= gap.top).then_some(gap)
    }

    /// The gap a sell enters at, when the gap check passes this bar.
    fn sell_zone(&self, average_range: f64) -> Option<Zone> {
        let gap = *self.supply.front()?;
        if !(self.swept_above > gap.top && self.down_trend && self.sell_armed_before) {
            return None;
        }
        let inverted = self.inverted_supply.front().map_or(f64::NAN, |z| z.top);
        let space = (inverted - gap.bottom).abs();
        let fits = space >= average_range * SPACE_MIN && space <= average_range * SPACE_MAX;
        (fits && inverted <= gap.bottom).then_some(gap)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let candle = Candle { open: bar.open, high: bar.high, low: bar.low, close: bar.close };
        let true_range = match self.candles.last() {
            None => candle.high - candle.low,
            Some(before) => (candle.high - candle.low)
                .max((candle.high - before.close).abs())
                .max((candle.low - before.close).abs()),
        };
        self.candles.push(candle);
        let average_range = self.average_range.next_value(true_range);
        if self.candles.len() > 2 {
            self.gaps();
        }
        let (above, below) = self.swings.step(&self.candles);
        if let Some(level) = above {
            record(&mut self.levels_above, level);
        }
        if let Some(level) = below {
            record(&mut self.levels_below, level);
        }
        let broke_below = sweep(&mut self.levels_below, |l| candle.low <= l);
        if let Some(level) = broke_below {
            self.swept_below = level;
        }
        let broke_above = sweep(&mut self.levels_above, |l| candle.high >= l);
        if let Some(level) = broke_above {
            self.swept_above = level;
        }
        self.structure.step(&self.candles);

        if !self.up_trend && self.structure.trend == 1 {
            (self.up_trend, self.down_trend) = (true, false);
        }
        if !self.down_trend && self.structure.trend == -1 {
            (self.down_trend, self.up_trend) = (true, false);
        }

        let held = strategy.position();
        let done = strategy.closed_trades().unwrap_or(&[]).len();
        let closed_now = done > std::mem::replace(&mut self.closed, done);

        if broke_below.is_some()
            && candle.close > self.swept_below
            && candle.open > self.swept_below
            && held == 0.0
        {
            self.buy_armed = true;
            if self.pending_sell {
                strategy.cancel(&short_name(self.sells));
                self.pending_sell = false;
            }
            self.sell_armed = false;
        }
        if candle.close < self.swept_below {
            self.buy_armed = false;
        }
        let buy = self.buy_zone(average_range).filter(|_| self.buy_armed);
        if buy.is_some() {
            self.buy_armed = false;
        }

        if broke_above.is_some()
            && candle.close < self.swept_above
            && candle.open < self.swept_above
            && held == 0.0
        {
            self.sell_armed = true;
            if self.pending_buy {
                strategy.cancel(&long_name(self.buys));
                self.pending_buy = false;
            }
            self.buy_armed = false;
        }
        if candle.close > self.swept_above {
            self.sell_armed = false;
        }
        let sell = self.sell_zone(average_range).filter(|_| self.sell_armed);
        if sell.is_some() {
            self.sell_armed = false;
        }

        if held > 0.0 || (closed_now && self.pending_buy) {
            self.pending_buy = false;
        }
        if held < 0.0 || (closed_now && self.pending_sell) {
            self.pending_sell = false;
        }
        if buy.is_some() || sell.is_some() {
            if self.pending_buy {
                strategy.cancel(&long_name(self.buys));
                self.pending_buy = false;
            }
            if self.pending_sell {
                strategy.cancel(&short_name(self.sells));
                self.pending_sell = false;
            }
        }

        let risk = average_range * STOP_RANGES;
        if let Some(gap) = buy {
            let price = gap.top - (ENTRY_DEPTH / 100.0 * (gap.top - gap.bottom)).abs();
            let stop = gap.bottom - risk;
            let take = price + (price - stop) * REWARD;
            self.buys += 1;
            let name = long_name(self.buys);
            strategy.entry(&name, Long).size(share_of_worth(&sizing, 100.0, price)).limit(price);
            strategy.exit(&format!("Long Exit {}", self.buys)).from(&name).stop(stop).take(take);
            self.pending_buy = true;
        }
        if let Some(gap) = sell {
            let price = gap.bottom + (ENTRY_DEPTH / 100.0 * (gap.top - gap.bottom)).abs();
            let stop = gap.top + risk;
            let take = price - (stop - price) * REWARD;
            self.sells += 1;
            let name = short_name(self.sells);
            strategy.entry(&name, Short).size(share_of_worth(&sizing, 100.0, price)).limit(price);
            strategy.exit(&format!("Short Exit {}", self.sells)).from(&name).stop(stop).take(take);
            self.pending_sell = true;
        }
        (self.buy_armed_before, self.sell_armed_before) = (self.buy_armed, self.sell_armed);
    }
}
