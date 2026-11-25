// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "AI SuperTrend x Pivot Percentile - Strategy [PresentTrading]" by PresentTrading, https://www.tradingview.com/script/eC3qAYNQ/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! Two volume-weighted supertrends of higher timeframes, each read by a weighted k-nearest-neighbours vote, a percentile
//! trend count, and a daily directional filter.
//!
//! - **The supertrends.** The first: the close of the last closed 360-minute bar times each chart bar's volume, weighted
//!   (WMA) over 10 chart bars and divided by the WMA(10) of the volume, with bands 3.5 times the ATR(10) of the 360-minute
//!   bars either side; the second the same on 720-minute bars with 5 bars and 3.0 times an ATR(5). Each band ratchets as a
//!   supertrend's does, from 0 where it has no value yet, and the direction turns down when the close crosses under the
//!   lower band and up when it crosses over the upper one (it holds up while the ATR had no value on the bar before).
//! - **The vote.** Each supertrend's last 48 values (the current one first) are labelled 1 where the WMA(10) of the close
//!   stood above the WMA(80) of that supertrend on that bar, else 0; the 6 nearest the current value (bubble-sorted by
//!   distance, ties in their order) vote, each weighted by one over its distance plus 0.000001. A vote of exactly 1 is up,
//!   exactly 0 down, anything else neither.
//! - **The percentile count.** The 75th percentile of the highs and the 25th of the lows (nearest rank) over 14, 28, 42,
//!   84 and 98 bars, each against the same percentiles over 144 bars: a percentile above the long high counts bullish, under
//!   the long low bearish, a low percentile between the two weakly bullish and a high one between them weakly bearish. The
//!   trend value is twice the bullish less the bearish counts, plus the weak bullish less the weak bearish.
//! - **The daily filter.** From the last closed day's high, low and close as each chart bar reads them (the day before is
//!   the bar before's, so a move shows on the one chart bar a new day arrives): the directional moves and the true range of
//!   the days, each a WMA(14) over chart bars, the two directional indexes, and the ADX as the WMA(14) of their DX. A DX of
//!   no movement either way is undefined (both indexes 0, on every chart bar but the 14 after a new day's move).
//!
//! Every WMA here skips an undefined value, as the original's platform's does: it is the average of the last defined values,
//! so the ADX stands on the last 14 defined DX values. Measured on the reference: with an undefined value kept in the window
//! (so an ADX only on the 14th chart bar after a new day), 44 trades and a net of -291.11; read as 0, -243.93; skipped, the
//! reference's 44 and -236.03.
//!
//! Long 10% of the account's worth when both supertrends point up, both votes are up, the ADX is over 20 with the plus index
//! over the minus, and the trend value is above 0; short the mirror. Every bar that is not the turn of a supertrend up with
//! an up vote (or the trend value is under 0), a long's stop is set to the first supertrend less 3.5 times its ATR; the
//! short's to the first supertrend plus that, on the mirror bars. An entry reverses the other side. Commission 0.1% of each
//! fill and one tick of slippage; starts with 10,000 in a USD account, no margin stated.
//!
//! The day is the symbol's daily input, its history before the window included
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`); the 360- and 720-minute bars are built from its 30-minute input
//! from the rule's first bar (`vike_tactics::higher::HigherTimeframeSourceIntraday`). The plots, shapes and alerts are left
//! out. The original's list of percentile lengths holds two more slots it never sets: they count nothing. Its exit set while
//! its side is neither held nor coming waits there for that side's next entry; here an exit is set with its entry, which on
//! every entry bar of the reference's window is the same level.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::Atr;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{back, nan_to_zero, push};
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar, HigherTimeframeSourceIntraday};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics, TacticsInput};

const SHARE: f64 = 10.0;
/// The first supertrend's band width in ATRs, which both stops are set by.
const FACTOR: f64 = 3.5;
const NEIGHBOURS: usize = 6;
const POINTS: usize = 48;
const PRICE_LENGTH: usize = 10;
const TREND_LENGTH: usize = 80;
const ADX_LENGTH: usize = 14;
const ADX_FLOOR: f64 = 20.0;
const LENGTHS: [usize; 5] = [14, 28, 42, 84, 98];
const LONG_WINDOW: usize = 144;

/// The weighted moving average as the original's platform computes it, newest weighted most: an undefined value is skipped,
/// so the average is of the last `length` defined values, undefined until that many have come.
struct Weighted {
    length: usize,
    /// The window, newest first.
    values: VecDeque<f64>,
}

impl Weighted {
    fn new(length: usize) -> Self {
        Weighted { length, values: VecDeque::with_capacity(length + 1) }
    }

    fn next(&mut self, x: f64) -> f64 {
        if !x.is_nan() {
            push(&mut self.values, x, self.length);
        }
        if self.values.len() < self.length {
            return f64::NAN;
        }
        let (mut sum, mut norm) = (0.0, 0.0);
        for (i, value) in self.values.iter().enumerate() {
            let weight = ((self.length - i) * self.length) as f64;
            norm += weight;
            sum += value * weight;
        }
        sum / norm
    }
}

/// The nearest-rank `percent` percentile of the newest `length` values of `ring` (newest first), undefined until it holds
/// that many.
fn nearest_rank(ring: &VecDeque<f64>, length: usize, percent: f64) -> f64 {
    if ring.len() < length {
        return f64::NAN;
    }
    let mut values: Vec<f64> = ring.iter().take(length).copied().collect();
    values.sort_by(f64::total_cmp);
    let rank = (percent / 100.0 * length as f64).ceil() as usize;
    values[rank.clamp(1, length) - 1]
}

/// The original's weighted vote of the `NEIGHBOURS` points of `data` nearest `x`: a stable bubble sort by distance, each
/// neighbour's label weighted by one over its distance plus 0.000001.
fn vote(data: &[f64; POINTS], labels: &[f64; POINTS], x: f64) -> f64 {
    let mut distances = data.map(|d| (x - d).abs());
    let mut indices: [usize; POINTS] = std::array::from_fn(|i| i);
    for i in 0..POINTS - 1 {
        for j in 0..POINTS - i - 1 {
            if distances[j] > distances[j + 1] {
                distances.swap(j, j + 1);
                indices.swap(j, j + 1);
            }
        }
    }
    let (mut sum, mut total) = (0.0, 0.0);
    for (distance, &index) in distances.iter().zip(&indices).take(NEIGHBOURS) {
        let weight = 1.0 / (distance + 1e-6);
        sum += weight * labels[index];
        total += weight;
    }
    sum / total
}

/// One volume-weighted supertrend of a higher timeframe and its vote.
struct Supertrend {
    frame: HigherTimeframeSourceIntraday,
    atr: Atr,
    factor: f64,
    /// The last closed frame bar's close and ATR, and the ATR as the bar before read it.
    frame_close: f64,
    frame_atr: f64,
    frame_atr_before: f64,
    weighted_price: Weighted,
    weighted_volume: Weighted,
    /// The bands, the line and the direction as the bar before left them.
    lower_before: f64,
    upper_before: f64,
    line_before: f64,
    direction_before: f64,
    price: Weighted,
    trend: Weighted,
    /// The line's last values and their labels, newest first.
    lines: VecDeque<f64>,
    labels: VecDeque<f64>,
}

/// One bar of a supertrend: its direction (-1 up, 1 down) and the one before, its line, its ATR, and its vote.
struct Reading {
    direction: f64,
    direction_before: f64,
    line: f64,
    atr: f64,
    vote: f64,
}

impl Supertrend {
    fn new(minutes: i64, length: usize, factor: f64) -> Self {
        Supertrend {
            frame: HigherTimeframeSourceIntraday::new(minutes),
            atr: Atr::new(length as f64).expect("a valid length"),
            factor,
            frame_close: f64::NAN,
            frame_atr: f64::NAN,
            frame_atr_before: f64::NAN,
            weighted_price: Weighted::new(length),
            weighted_volume: Weighted::new(length),
            lower_before: f64::NAN,
            upper_before: f64::NAN,
            line_before: f64::NAN,
            direction_before: f64::NAN,
            price: Weighted::new(PRICE_LENGTH),
            trend: Weighted::new(TREND_LENGTH),
            lines: VecDeque::with_capacity(POINTS + 1),
            labels: VecDeque::with_capacity(POINTS + 1),
        }
    }

    fn next(&mut self, bar: &Bar, close_before: f64, half_hour: TacticsInput<'_>) -> Reading {
        for higher in self.frame.next(bar, half_hour) {
            self.frame_close = higher.close;
            self.frame_atr = self.atr.next(&higher);
        }
        let average = self.weighted_price.next(self.frame_close * bar.volume)
            / self.weighted_volume.next(bar.volume);
        let (lower_before, upper_before) =
            (nan_to_zero(self.lower_before), nan_to_zero(self.upper_before));
        let mut lower = average - self.factor * self.frame_atr;
        let mut upper = average + self.factor * self.frame_atr;
        if !(lower > lower_before || close_before < lower_before) {
            lower = lower_before;
        }
        if !(upper < upper_before || close_before > upper_before) {
            upper = upper_before;
        }
        let direction = if self.frame_atr_before.is_nan() {
            1.0
        } else if self.line_before == upper_before {
            if bar.close > upper { -1.0 } else { 1.0 }
        } else if bar.close < lower {
            1.0
        } else {
            -1.0
        };
        let line = if direction == -1.0 { lower } else { upper };

        let price = self.price.next(bar.close);
        let trend = self.trend.next(line);
        push(&mut self.lines, line, POINTS);
        push(&mut self.labels, if price > trend { 1.0 } else { 0.0 }, POINTS);
        let data = std::array::from_fn(|i| back(&self.lines, i));
        let labels = std::array::from_fn(|i| self.labels.get(i).copied().unwrap_or(0.0));
        let reading = Reading {
            direction,
            direction_before: self.direction_before,
            line,
            atr: self.frame_atr,
            vote: vote(&data, &labels, line),
        };
        (self.lower_before, self.upper_before, self.line_before) = (lower, upper, line);
        (self.direction_before, self.frame_atr_before) = (direction, self.frame_atr);
        reading
    }
}

/// The daily directional filter: the ADX and the two directional indexes.
struct Directional {
    days: HigherTimeframeSourceCalendar,
    /// The last closed day's high, low and close, and as the bar before read them.
    day: [f64; 3],
    day_before: [f64; 3],
    range: Weighted,
    plus: Weighted,
    minus: Weighted,
    index: Weighted,
}

impl Directional {
    /// The ADX, the plus index and the minus index on this bar.
    fn next(&mut self, bar: &Bar, days: TacticsInput<'_>) -> [f64; 3] {
        for day in self.days.next(bar, days) {
            self.day = [day.high, day.low, day.close];
        }
        let [high, low, _] = self.day;
        let [high_before, low_before, close_before] = self.day_before;
        let up = high - high_before;
        let down = low_before - low;
        let plus_move = if up > down && up > 0.0 { up } else { 0.0 };
        let minus_move = if down > up && down > 0.0 { down } else { 0.0 };
        let range = self
            .range
            .next((high - low).abs() + (high - close_before).abs() + (low - close_before).abs());
        let plus = 100.0 * self.plus.next(plus_move) / range;
        let minus = 100.0 * self.minus.next(minus_move) / range;
        let index = 100.0 * (plus - minus).abs() / (plus + minus);
        self.day_before = self.day;
        [self.index.next(index), plus, minus]
    }
}

struct S {
    first: Supertrend,
    second: Supertrend,
    directional: Directional,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    close_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        first: Supertrend::new(360, 10, FACTOR),
        second: Supertrend::new(720, 5, 3.0),
        directional: Directional {
            days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
            day: [f64::NAN; 3],
            day_before: [f64::NAN; 3],
            range: Weighted::new(ADX_LENGTH),
            plus: Weighted::new(ADX_LENGTH),
            minus: Weighted::new(ADX_LENGTH),
            index: Weighted::new(ADX_LENGTH),
        },
        highs: VecDeque::with_capacity(LONG_WINDOW + 1),
        lows: VecDeque::with_capacity(LONG_WINDOW + 1),
        close_before: f64::NAN,
    })
}

impl S {
    /// The percentile trend value: positive bullish, negative bearish.
    fn trend_value(&self) -> f64 {
        let long_high = nearest_rank(&self.highs, LONG_WINDOW, 75.0);
        let long_low = nearest_rank(&self.lows, LONG_WINDOW, 25.0);
        let (mut bull, mut bear, mut weak_bull, mut weak_bear) = (0.0, 0.0, 0.0, 0.0);
        for length in LENGTHS {
            let high = nearest_rank(&self.highs, length, 75.0);
            let low = nearest_rank(&self.lows, length, 25.0);
            bull += f64::from(u8::from(high > long_high)) + f64::from(u8::from(low > long_high));
            bear += f64::from(u8::from(high < long_low)) + f64::from(u8::from(low < long_low));
            weak_bull += f64::from(u8::from(low < long_high && low > long_low));
            weak_bear += f64::from(u8::from(high > long_low && high < long_high));
        }
        let bull_strength = bull + 0.5 * weak_bull - 0.5 * weak_bear - bear;
        let bear_strength = bear + 0.5 * weak_bear - 0.5 * weak_bull - bull;
        bull_strength - bear_strength
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string(), higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [adx, plus, minus] = self.directional.next(bar, strategy.input(higher::DAY));
        let first = self.first.next(bar, self.close_before, strategy.input(higher::HALF_HOUR));
        let second = self.second.next(bar, self.close_before, strategy.input(higher::HALF_HOUR));
        self.close_before = bar.close;
        push(&mut self.highs, bar.high, LONG_WINDOW);
        push(&mut self.lows, bar.low, LONG_WINDOW);
        let trend = self.trend_value();

        let turned_up =
            |r: &Reading| r.direction == -1.0 && r.direction_before == 1.0 && r.vote == 1.0;
        let turned_down =
            |r: &Reading| r.direction == 1.0 && r.direction_before == -1.0 && r.vote == 0.0;
        let long = first.direction < 0.0
            && first.vote == 1.0
            && second.direction < 0.0
            && second.vote == 1.0
            && adx > ADX_FLOOR
            && plus > minus
            && trend > 0.0;
        let short = first.direction > 0.0
            && first.vote == 0.0
            && second.direction > 0.0
            && second.vote == 0.0
            && adx > ADX_FLOOR
            && minus > plus
            && trend < 0.0;
        let long_exit = !(turned_up(&first) || turned_up(&second)) || trend < 0.0;
        let short_exit = !(turned_down(&first) || turned_down(&second)) || trend > 0.0;
        let long_stop = first.line - first.atr * FACTOR;
        let short_stop = first.line + first.atr * FACTOR;

        let size = share_of_worth(&strategy.sizing(), SHARE, bar.close);
        if long {
            strategy.entry("Long", Long).size(size);
        }
        if long_exit && !long_stop.is_nan() {
            strategy.exit("Exit Long").from("Long").stop(long_stop);
        }
        if short {
            strategy.entry("Short", Short).size(size);
        }
        if short_exit && !short_stop.is_nan() {
            strategy.exit("Exit Short").from("Short").stop(short_stop);
        }
    }
}
