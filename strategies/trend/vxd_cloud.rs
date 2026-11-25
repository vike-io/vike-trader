// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "VXD Cloud" by Vvaz_, https://www.tradingview.com/script/OKKz3vDv/
// The original's notice: © Vvaz_
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-06-08, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A cloud between EMA(30) of the close, EMA(30) of the 30-bar least-squares line of the close, and a trend line two bars back
//! (the bar's low less 1.6 ATR, never falling, while RSI(25) is at or over 50; the high plus 1.6 ATR, never rising, below
//! it; the ATR the 12-bar mean of the true range). A cloud line follows the cloud's low while the Andean oscillator's bull
//! component (30 bars) leads and its high while the bear one does, never against itself; it sets the trend up when it rose two
//! bars in a row, down when it fell two bars in a row (measured: read as over both of the two bars before, 515 trades).
//!
//! Long when the trend is up and the close is above the cloud, short on the mirror, each at the bar's close: the size risks
//! 10 to the lowest of the last 50 pivot lows (5 bars either side) for a long, to the highest pivot high for a short. An entry
//! of a side waits until a turn of the trend to that side has been followed by a stop-out sign (the bar's running extreme
//! crossing its stop level), and the change of the signal is read on both of its edges, so the bar after an entry (whose
//! signal then falls) states the trade's levels from its entry price: a first target at 3 and a second at 4 times the
//! distance to that extreme, half the trade at the first and the rest at the second, both under one stop. The stop starts at
//! that extreme, taken when the trend turned, and moves to the entry once the trade's best price passed one such distance. A
//! turn of the trend the other way closes the trade at the bar's close. No costs; starts with 100 in a USD account.
//!
//! What the original only draws (its pivots, high-volume bars, cloud colours and its imported library's profit table, a
//! display of `Naruephat/Cayoshi_Library`) is left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::{ConfirmedPivot, Ema, Sma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::floor_share;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ATR_LENGTH: f64 = 12.0;
const ATR_MULTIPLIER: f64 = 1.6;
const ANDEAN: f64 = 30.0;
const LINE: usize = 30;
const SIZING_BARS: usize = 50;
const RISK: f64 = 10.0;
const FIRST: f64 = 3.0;
const SECOND: f64 = 4.0;
const LONG: &str = "long2";
const SHORT: &str = "Short2";

/// A missing value read as 0.
fn nz(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x }
}

/// The original's three-way `max`/`min`: a missing value makes it missing.
fn max3(a: f64, b: f64, c: f64) -> f64 {
    if a.is_nan() || b.is_nan() || c.is_nan() { f64::NAN } else { a.max(b).max(c) }
}

fn min3(a: f64, b: f64, c: f64) -> f64 {
    if a.is_nan() || b.is_nan() || c.is_nan() { f64::NAN } else { a.min(b).min(c) }
}

/// Two values that are both there and differ.
fn differ(a: f64, b: f64) -> bool {
    !a.is_nan() && !b.is_nan() && a != b
}

/// Two rises in a row: over the value before it, which was over the one before that.
fn rising(x: f64, back: &VecDeque<f64>) -> bool {
    back.len() == 2 && x > back[0] && back[0] > back[1]
}

fn falling(x: f64, back: &VecDeque<f64>) -> bool {
    back.len() == 2 && x < back[0] && back[0] < back[1]
}

/// Keeps the newest `keep` values, newest first.
fn remember(back: &mut VecDeque<f64>, x: f64, keep: usize) {
    back.push_front(x);
    back.truncate(keep);
}

/// The highest (`way` 1) or lowest (`way` -1) value of a window, its missing values passed over.
fn extreme(window: &VecDeque<f64>, way: f64) -> f64 {
    window
        .iter()
        .filter(|x| !x.is_nan())
        .fold(f64::NAN, |best, &x| if best.is_nan() || way * x > way * best { x } else { best })
}

/// The previous bar's values of what the rule carries from bar to bar.
#[derive(Clone, Copy)]
struct Carried {
    alpha_trend: f64,
    cloud: f64,
    trend: f64,
    bull_action: bool,
    bear_action: bool,
    lot: f64,
    buy_price: f64,
    sell_price: f64,
    risk_reward: f64,
    target_long: f64,
    target_long_2: f64,
    target_short: f64,
    target_short_2: f64,
    high: f64,
    low: f64,
    running_high: f64,
    running_low: f64,
    stop_long: f64,
    stop_short: f64,
    one_buy: f64,
    one_sell: f64,
}

struct S {
    previous_close: f64,
    ranges: Sma,
    rsi: IndicatorLine,
    up1: f64,
    up2: f64,
    down1: f64,
    down2: f64,
    fast: Ema,
    closes: VecDeque<f64>,
    line: Ema,
    /// AlphaTrend's last two values, newest first.
    alpha_back: VecDeque<f64>,
    /// The cloud line's last two values, newest first.
    cloud_back: VecDeque<f64>,
    pivots: ConfirmedPivot,
    tops: VecDeque<f64>,
    bottoms: VecDeque<f64>,
    before: Option<Carried>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        previous_close: f64::NAN,
        ranges: Sma::new(ATR_LENGTH).expect("a valid length"),
        rsi: IndicatorLine::new("rsi", &[25.0]),
        up1: f64::NAN,
        up2: f64::NAN,
        down1: f64::NAN,
        down2: f64::NAN,
        fast: Ema::new(30.0).expect("a valid length"),
        closes: VecDeque::with_capacity(LINE + 1),
        line: Ema::new(30.0).expect("a valid length").with_skip_nan(true),
        alpha_back: VecDeque::with_capacity(3),
        cloud_back: VecDeque::with_capacity(3),
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        tops: VecDeque::with_capacity(SIZING_BARS + 1),
        bottoms: VecDeque::with_capacity(SIZING_BARS + 1),
        before: None,
    })
}

impl S {
    /// The 30-bar least-squares line of the close at its newest point, the oldest close at x = 1.
    fn least_squares(&self) -> f64 {
        if self.closes.len() < LINE {
            return f64::NAN;
        }
        let n = LINE as f64;
        let (mut sum_x, mut sum_x2, mut sum_y, mut sum_xy, mut x) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &y in self.closes.iter().rev() {
            x += 1.0;
            sum_x += x;
            sum_x2 += x * x;
            sum_y += y;
            sum_xy += y * x;
        }
        let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_x2 - sum_x * sum_x);
        let intercept = sum_y / n - slope * sum_x / n + slope;
        intercept + slope * (n - 1.0)
    }

    /// The Andean oscillator's bull and bear components.
    fn andean(&mut self, close: f64, open: f64) -> (f64, f64) {
        let alpha = 2.0 / (ANDEAN + 1.0);
        let or = |x: f64, y: f64| if x.is_nan() { y } else { x };
        let (squared, open_squared) = (close * close, open * open);
        self.up1 = or(max3(close, open, self.up1 - (self.up1 - close) * alpha), close);
        self.up2 =
            or(max3(squared, open_squared, self.up2 - (self.up2 - squared) * alpha), squared);
        self.down1 = or(min3(close, open, self.down1 + (close - self.down1) * alpha), close);
        self.down2 =
            or(min3(squared, open_squared, self.down2 + (squared - self.down2) * alpha), squared);
        let bull = (self.down2 - self.down1 * self.down1).sqrt();
        let bear = (self.up2 - self.up1 * self.up1).sqrt();
        (bull, bear)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (close, open, high, low) = (bar.close, bar.open, bar.high, bar.low);
        let none = f64::NAN;
        let was = self.before.unwrap_or(Carried {
            alpha_trend: none,
            cloud: none,
            trend: none,
            bull_action: false,
            bear_action: false,
            lot: none,
            buy_price: none,
            sell_price: none,
            risk_reward: none,
            target_long: none,
            target_long_2: none,
            target_short: none,
            target_short_2: none,
            high: none,
            low: none,
            running_high: none,
            running_low: none,
            stop_long: none,
            stop_short: none,
            one_buy: none,
            one_sell: none,
        });

        // The true range (none on the first bar) and its 12-bar mean.
        let range = if self.previous_close.is_nan() {
            none
        } else {
            (high - low)
                .max((high - self.previous_close).abs())
                .max((low - self.previous_close).abs())
        };
        self.previous_close = close;
        // The mean starts with the second bar, the first true range there is.
        let atr = if range.is_nan() { none } else { self.ranges.next_value(range) };
        let down_line = high + atr * ATR_MULTIPLIER;
        let up_line = low - atr * ATR_MULTIPLIER;
        let rsi = self.rsi.next(bar)[0];
        let (bull, bear) = self.andean(close, open);
        let fast = self.fast.next_value(close);
        remember(&mut self.closes, close, LINE);
        let line = self.line.next_value(self.least_squares());

        let held = nz(was.alpha_trend);
        let alpha_trend = if rsi >= 50.0 {
            if up_line < held { held } else { up_line }
        } else if down_line > held {
            held
        } else {
            down_line
        };
        let alpha_two_back = if self.alpha_back.len() == 2 { self.alpha_back[1] } else { none };
        remember(&mut self.alpha_back, alpha_trend, 2);
        let cloud_high = max3(fast, line, alpha_two_back);
        let cloud_low = min3(fast, line, alpha_two_back);
        let green = close > cloud_high;
        let red = close < cloud_low;
        let cloud_held = nz(was.cloud);
        let cloud = if bull > bear {
            if cloud_low < cloud_held { cloud_held } else { cloud_low }
        } else if bear > bull {
            if cloud_high > cloud_held { cloud_held } else { cloud_high }
        } else {
            cloud_held
        };
        let up = rising(cloud, &self.cloud_back);
        let down = falling(cloud, &self.cloud_back);
        remember(&mut self.cloud_back, cloud, 2);
        let trend = if up {
            1.0
        } else if down {
            -1.0
        } else {
            was.trend
        };

        let [_, _, top, bottom] = self.pivots.next(bar);
        remember(&mut self.tops, top, SIZING_BARS);
        remember(&mut self.bottoms, bottom, SIZING_BARS);
        let (highest, lowest) = (extreme(&self.tops, 1.0), extreme(&self.bottoms, -1.0));

        let position = strategy.position();
        let open_price = strategy.lots().first().map_or(none, |lot| lot.price);
        let flat_or_short = strategy.lots().is_empty() || position <= 0.0;
        let flat_or_long = strategy.lots().is_empty() || position >= 0.0;
        let turned = differ(trend, was.trend);
        let exit_short = turned && trend == 1.0;
        let exit_long = turned && trend == -1.0;
        let bull_action = trend > 0.0 && flat_or_short && green;
        let bear_action = trend < 0.0 && flat_or_long && red;
        let bull_change = self.before.is_some() && bull_action != was.bull_action;
        let bear_change = self.before.is_some() && bear_action != was.bear_action;

        let lot = if bull_change {
            (RISK / (close - lowest)).abs()
        } else if bear_change {
            (RISK / (highest - close)).abs()
        } else {
            was.lot
        };
        let buy_price = if position >= 0.0 {
            if bull_change { open_price } else { was.buy_price }
        } else {
            none
        };
        let sell_price = if position <= 0.0 {
            if bear_change { open_price } else { was.sell_price }
        } else {
            none
        };
        let long_reach =
            |times: f64| buy_price * (1.0 + (open_price - lowest) / open_price * times);
        let short_reach =
            |times: f64| sell_price * (1.0 - (highest - open_price) / open_price * times);
        let risk_reward = if bull_change {
            long_reach(1.0)
        } else if bear_change {
            short_reach(1.0)
        } else {
            was.risk_reward
        };
        let keep = |now: bool, then: f64| if now { then } else { 0.0 };
        let target_long =
            if bull_change { long_reach(FIRST) } else { keep(position > 0.0, was.target_long) };
        let target_long_2 =
            if bull_change { long_reach(SECOND) } else { keep(position > 0.0, was.target_long_2) };
        let target_short =
            if bear_change { short_reach(FIRST) } else { keep(position < 0.0, was.target_short) };
        let target_short_2 = if bear_change {
            short_reach(SECOND)
        } else {
            keep(position < 0.0, was.target_short_2)
        };
        let running_high = if position != 0.0 {
            if high > was.high { high } else { was.running_high }
        } else {
            high
        };
        let running_low =
            if position != 0.0 { if low < was.low { low } else { was.running_low } } else { low };
        let stop_long = if exit_short {
            lowest
        } else if position > 0.0 && running_high > risk_reward {
            buy_price
        } else {
            was.stop_long
        };
        let stop_short = if exit_long {
            highest
        } else if position < 0.0 && running_low < risk_reward {
            sell_price
        } else {
            was.stop_short
        };
        let stopped_long = running_low < stop_long && was.running_low >= was.stop_long;
        let stopped_short = running_high > stop_short && was.running_high <= was.stop_short;
        let one_buy =
            if differ(was.trend, 1.0) && trend == 1.0 || was.trend.is_nan() && trend == 1.0 {
                1.0
            } else if stopped_long {
                0.0
            } else if position > 0.0 {
                1.0
            } else if trend == -1.0 {
                0.0
            } else {
                was.one_buy
            };
        let one_sell =
            if differ(was.trend, -1.0) && trend == -1.0 || was.trend.is_nan() && trend == -1.0 {
                1.0
            } else if stopped_short {
                0.0
            } else if position < 0.0 {
                1.0
            } else if trend == 1.0 {
                0.0
            } else {
                was.one_sell
            };

        if exit_long {
            strategy.close(LONG);
        }
        if exit_short {
            strategy.close(SHORT);
        }
        let size = strategy.sizing().floor(lot);
        if bull_change && was.one_buy == 0.0 {
            strategy.entry(LONG, Long).size(size);
        }
        if bear_change && was.one_sell == 0.0 {
            strategy.entry(SHORT, Short).size(size);
        }
        let step = strategy.sizing().qty_step();
        let half = |strategy: &Tactics<'_>, name: &str| {
            strategy
                .bindings()
                .iter()
                .find(|b| b.name == name)
                .map_or(0.0, |b| floor_share(0.5 * b.left, b.left, step))
        };
        if position > 0.0 {
            let first = half(strategy, LONG);
            levels(strategy.exit("TP1").from(LONG).size(first), target_long, stop_long);
            levels(strategy.exit("TP2").from(LONG), target_long_2, stop_long);
            levels(strategy.exit("SL").from(LONG), none, stop_long);
        }
        if position < 0.0 {
            let first = half(strategy, SHORT);
            levels(strategy.exit("STP1").from(SHORT).size(first), target_short, stop_short);
            levels(strategy.exit("STP2").from(SHORT), target_short_2, stop_short);
            levels(strategy.exit("SSL").from(SHORT), none, stop_short);
        }

        self.before = Some(Carried {
            alpha_trend,
            cloud,
            trend,
            bull_action,
            bear_action,
            lot,
            buy_price,
            sell_price,
            risk_reward,
            target_long,
            target_long_2,
            target_short,
            target_short_2,
            high,
            low,
            running_high,
            running_low,
            stop_long,
            stop_short,
            one_buy,
            one_sell,
        });
    }
}

/// An exit's target and stop, each only when it is there.
fn levels(exit: &mut vike_tactics::TacticsExit, take: f64, stop: f64) {
    if !take.is_nan() {
        exit.take(take);
    }
    if !stop.is_nan() {
        exit.stop(stop);
    }
}
