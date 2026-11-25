// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "72s Strategy: Adaptive Hull Moving Average+ pt.1" by io72signals, https://www.tradingview.com/script/FrjeeWdl/
// The original's notice: 2020 © io72signals / Antorio Bergasdito
// Changed: rewritten by hand in Rust for vike.

//! An adaptive Hull average of the close: its length starts at 202.5 and each bar shrinks 3.141% (to no less than 172) while
//! ATR(14) is above ATR(46), else grows 3.141% (to no more than 233); a minor one does the same between 89 and 121 from 105.
//! Its slope is an angle in whole degrees, from the change over two bars scaled by 25 over the 34-bar range times its low.
//! A signal is the bar the slope first reaches 17 (or -17) with ATR(14) above ATR(46). Long 0.1 BTC on a rising signal with
//! the close above the average, the low at or under the average plus 2.7 ATR(40) and RSI(14) above 51 and at most 70, or, as
//! a second kind, RSI above 70 with the bar's middle above that band and the minor average above the major; short on the
//! mirror. One entry at a time, each kind closed by its own exits, all judged on the close: the take profit 3 ATR(21) past
//! the signal bar's extreme, the stop at the band (or 4.5 ATR(21) past the extreme when nearer), and the rule's RSI and
//! average exits once the close is 1 ATR(21) in profit. 0.5 per order and 3 ticks of slippage; starts with 1,000.
//!
//! The rule's levels are kept from one signal to the next, starting at 0, and any signal sets both take profits. Its second
//! entries and its order flags are off or unread, so they are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Donchian, Hma, Rsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LOT: f64 = 0.1;
const ADAPT: f64 = 0.03141;
const MAJOR: (f64, f64) = (172.0, 233.0);
const MINOR: (f64, f64) = (89.0, 121.0);
const FLAT: f64 = 17.0;
const MULTIPLIER: f64 = 2.7;

/// A Hull average whose length can change every bar (`Hma::next_value_with_period`): the inner averages over this bar's
/// length, floored, the outer over the root of it.
struct Hull {
    bounds: (f64, f64),
    length: f64,
    hull: Hma,
}

impl Hull {
    fn new(bounds: (f64, f64)) -> Self {
        let hull = Hma::new(bounds.1).expect("a valid length");
        Hull { bounds, length: (bounds.0 + bounds.1) / 2.0, hull }
    }

    fn next(&mut self, close: f64, charged: bool) -> f64 {
        self.length = if charged {
            self.bounds.0.max(self.length * (1.0 - ADAPT))
        } else {
            self.bounds.1.min(self.length * (1.0 + ADAPT))
        };
        self.hull.next_value_with_period(close, self.length.floor())
    }
}

struct S {
    atr_base: Atr,
    atr_fast: Atr,
    atr_slow: Atr,
    atr_zone: Atr,
    rsi: Rsi,
    range: Donchian,
    major: Hull,
    minor: Hull,
    majors: Vec<f64>,
    was_up: bool,
    was_down: bool,
    buy_take: f64,
    sell_take: f64,
    buy_stop: f64,
    sell_stop: f64,
    rsi_70: Cross,
    rsi_30: Cross,
    rsi_45: Cross,
    rsi_55: Cross,
    close_major: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr_base: Atr::new(21.0).expect("a valid length"),
        atr_fast: Atr::new(14.0).expect("a valid length"),
        atr_slow: Atr::new(46.0).expect("a valid length"),
        atr_zone: Atr::new(40.0).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        range: Donchian::new(34.0).expect("a valid length"),
        major: Hull::new(MAJOR),
        minor: Hull::new(MINOR),
        majors: Vec::new(),
        was_up: false,
        was_down: false,
        buy_take: 0.0,
        sell_take: 0.0,
        buy_stop: 0.0,
        sell_stop: 0.0,
        rsi_70: Cross::new(),
        rsi_30: Cross::new(),
        rsi_45: Cross::new(),
        rsi_55: Cross::new(),
        close_major: Cross::new(),
    })
}

/// The slope in whole degrees, negative when the average fell over two bars.
fn slope(two_ago: f64, now: f64, close: f64, scale: f64) -> f64 {
    let pi = libm::atan(1.0) * 4.0;
    let dt = (two_ago - now) / close * scale;
    let c = (1.0 + dt * dt).sqrt();
    let angle = (180.0 * libm::acos(1.0 / c) / pi).round();
    if dt > 0.0 { -angle } else { angle }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_order(0.5), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (open, high, low, close) = (bar.open, bar.high, bar.low, bar.close);
        let base = self.atr_base.next(bar);
        let rsi = self.rsi.next(bar);
        let charged = self.atr_fast.next(bar) > self.atr_slow.next(bar);
        let [highest, _, lowest] = self.range.next(bar);
        let scale = 25.0 / (highest - lowest) * lowest;
        let major = self.major.next(close, charged);
        let minor = self.minor.next(close, charged);
        self.majors.push(major);
        let two_ago = self.majors.len().checked_sub(3).map_or(f64::NAN, |i| self.majors[i]);
        let angle = slope(two_ago, major, close, scale);
        let zone = self.atr_zone.next(bar);
        let (upper, lower) = (major + MULTIPLIER * zone, major - MULTIPLIER * zone);
        let (stop_upper, stop_lower) =
            (major + MULTIPLIER / 2.0 * zone, major - MULTIPLIER / 2.0 * zone);
        let middle = (high + low) / 2.0;

        let up = angle >= FLAT && charged;
        let down = angle <= -FLAT && charged;
        let (rising, falling) = (up && !self.was_up, down && !self.was_down);
        (self.was_up, self.was_down) = (up, down);
        let buy = rising && close > major && low <= upper && rsi > 51.0 && rsi <= 70.0;
        let sell = falling && close < major && high >= lower && (30.0..49.0).contains(&rsi);
        let over_buy = rising && rsi > 70.0 && close > major && middle > upper && minor > major;
        let over_sell = falling && rsi < 30.0 && close < major && middle < lower && minor < major;

        let widest = 4.5 * base;
        if buy || sell || over_buy || over_sell {
            self.buy_take = high + 3.0 * base;
            self.sell_take = low - 3.0 * base;
            if buy || over_buy {
                let far = low - widest;
                self.buy_stop = if far < lower { lower } else { far };
            }
            if sell || over_sell {
                let far = high + widest;
                self.sell_stop = if far > upper { upper } else { far };
            }
        }

        let avg = strategy.avg_cost();
        let in_buy_profit = close >= avg + base;
        let in_sell_profit = close <= avg - base;
        let stop_buy = low <= self.buy_stop;
        let stop_sell = high >= self.sell_stop;
        let take_buy = high >= self.buy_take;
        let take_sell = low <= self.sell_take;
        let under_70 = matches!(self.rsi_70.step(rsi, 70.0), Signal::Short);
        let over_30 = matches!(self.rsi_30.step(rsi, 30.0), Signal::Long);
        let under_45 = matches!(self.rsi_45.step(rsi, 45.0), Signal::Short);
        let over_55 = matches!(self.rsi_55.step(rsi, 55.0), Signal::Long);
        let crossed = self.close_major.step(close, major);
        let (close_over, close_under) =
            (matches!(crossed, Signal::Long), matches!(crossed, Signal::Short));

        let exit_buy =
            (in_buy_profit && (under_70 || rsi < 50.0 || close_under)) || stop_buy || take_buy;
        let exit_sell =
            (in_sell_profit && (over_30 || rsi > 50.0 || close_over)) || stop_sell || take_sell;
        let exit_over_buy = (in_buy_profit
            && (under_45 || (open < stop_upper && middle < stop_upper)))
            || (open < major && rsi < 60.0)
            || stop_buy
            || take_buy;
        let exit_over_sell = (in_sell_profit
            && (over_55 || (open > stop_lower && middle > stop_lower)))
            || (open > major && rsi > 40.0)
            || stop_sell
            || take_sell;

        for (name, side, wanted) in [
            ("buy", Long, buy),
            ("sell", Short, sell),
            ("overBuy", Long, over_buy),
            ("overSell", Short, over_sell),
        ] {
            if wanted {
                strategy.entry(name, side).size(LOT);
            }
        }
        for (name, wanted) in [
            ("buy", exit_buy),
            ("sell", exit_sell),
            ("overBuy", exit_over_buy),
            ("overSell", exit_over_sell),
        ] {
            if wanted {
                strategy.close(name);
            }
        }
    }
}
