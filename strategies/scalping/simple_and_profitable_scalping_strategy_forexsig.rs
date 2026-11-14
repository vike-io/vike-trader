// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Simple and Profitable Scalping Strategy (ForexSignals TV)" by kevinmck100, https://www.tradingview.com/script/x4za4NFZ/
// The original's notice: © kevinmck100
// Changed: rewritten by hand in Rust for vike.

//! A pullback into the 8/13/21 EMAs of the chart, in the direction of the hourly trend. Hourly EMA(8) over EMA(21) (as of the
//! last closed hour) and the close over the fast one, the chart's EMAs ordered the same way and fanned out (over each of the last
//! three bars the fast one clears the slow one by half an ATR(14)), the bar before wholly clear of the fast EMA and this bar's
//! low (high) touching it. The entry is a stop order: just over the 5-bar high (under the 5-bar low) by half an ATR, with a stop
//! under this bar's low (over its high) by the same buffer. It sizes by risk, `1% of the account's worth / stop percent / close`
//! floored to the step, where the stop percent is the stop's distance over the entry level; the side's flag is set.
//!
//! The target and stop rest in ticks of the symbol from the entry's fill (2 and 1 times the stop's distance, each rounded to a
//! tick count at the signal close; nothing is traded where no tick is known), so they are issued from the fill and again at
//! every close. A flag clears when a bar's range reaches the stop or the target the rule priced for its planned entry (while a
//! position was held at the bar before, or the last closed trade entered on this bar: the run's closed trades,
//! `vike_tactics::Tactics::closed_trades`), and an unfilled order is cancelled, with the flag cleared, when a bar closes the
//! wrong side of the slow EMA with no position held at the bar before. No currency; 0.02% commission; starts with 1,000.
//!
//! The hours are built from the symbol's 30-minute input from the rule's first bar (`vike_tactics::higher::HigherTimeframeSourceIntraday`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::{Atr, Donchian, Ema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const FANOUT_MULTIPLIER: f64 = 0.5;
const FANOUT_LOOKBACK: usize = 3;
const BUFFER_MULTIPLIER: f64 = 0.5;
const HIGH_LOOKBACK: f64 = 5.0;
const REWARD: f64 = 2.0;
const RISK_PERCENT: f64 = 1.0;

/// The original's `math.round`: halves go up.
fn round(x: f64) -> f64 {
    (x + 0.5).floor()
}

struct S {
    hours: HigherTimeframeSourceIntraday,
    trend_fast: Ema,
    trend_slow: Ema,
    /// The last hourly values the call gave.
    hourly: (f64, f64),
    fast: Ema,
    medium: Ema,
    slow: Ema,
    atr: Atr,
    /// The chart's fast and slow EMAs of the last three bars, newest first.
    fasts: VecDeque<f64>,
    slows: VecDeque<f64>,
    extremes: Donchian,
    /// The bar before's low, high and fast EMA.
    before: (f64, f64, f64),
    in_long: bool,
    in_short: bool,
    cancelled: bool,
    held_before: f64,
    profit_points: f64,
    loss_points: f64,
    stop_price: f64,
    target_price: f64,
    slow_percent: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length");
    Box::new(S {
        hours: HigherTimeframeSourceIntraday::new(60),
        trend_fast: ema(8.0),
        trend_slow: ema(21.0),
        hourly: (f64::NAN, f64::NAN),
        fast: ema(8.0),
        medium: ema(13.0),
        slow: ema(21.0),
        atr: Atr::new(14.0).expect("a valid length"),
        fasts: VecDeque::new(),
        slows: VecDeque::new(),
        extremes: Donchian::new(HIGH_LOOKBACK).expect("a valid length"),
        before: (f64::NAN, f64::NAN, f64::NAN),
        in_long: false,
        in_short: false,
        cancelled: false,
        held_before: 0.0,
        profit_points: 0.0,
        loss_points: 0.0,
        stop_price: 0.0,
        target_price: 0.0,
        slow_percent: 0.0,
    })
}

impl S {
    /// The exits rest in ticks from the fill, for whatever is held.
    fn exits(&self, strategy: &mut Tactics<'_>, tick: f64) {
        let (held, cost) = (strategy.position(), strategy.avg_cost());
        let (profit, loss) = (self.profit_points * tick, self.loss_points * tick);
        if held > 0.0 {
            strategy.exit("TP/SL Long").from("Long").take(cost + profit).stop(cost - loss);
        } else if held < 0.0 {
            strategy.exit("TP/SL Short").from("Short").take(cost - profit).stop(cost + loss);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let close = bar.close;
        for hour in self.hours.next(bar, strategy.input(higher::HALF_HOUR)) {
            let (fast, slow) =
                (self.trend_fast.next_value(hour.close), self.trend_slow.next_value(hour.close));
            if !fast.is_nan() {
                self.hourly.0 = fast;
            }
            if !slow.is_nan() {
                self.hourly.1 = slow;
            }
        }
        let (trend_fast, trend_slow) = self.hourly;
        let (fast, medium, slow) =
            (self.fast.next(bar), self.medium.next(bar), self.slow.next(bar));
        let atr = self.atr.next(bar);
        let gap = atr * FANOUT_MULTIPLIER;
        let buffer = atr * BUFFER_MULTIPLIER;

        self.fasts.push_front(fast);
        self.slows.push_front(slow);
        self.fasts.truncate(FANOUT_LOOKBACK);
        self.slows.truncate(FANOUT_LOOKBACK);
        let fanned = |sign: f64| {
            self.fasts.len() == FANOUT_LOOKBACK
                && self
                    .fasts
                    .iter()
                    .zip(&self.slows)
                    .all(|(f, s)| (f - sign * gap) * sign > s * sign)
        };
        // `(fast - gap) > slow` going long, `(fast + gap) < slow` short.
        let bull_fan = fanned(1.0);
        let bear_fan = fanned(-1.0);

        let (low_before, high_before, fast_before) = self.before;
        let bull_breakout = low_before > fast_before;
        let bear_breakout = high_before < fast_before;
        let bull_pullback = bar.low < fast;
        let bear_pullback = bar.high > fast;
        self.before = (bar.low, bar.high, fast);

        let go_long = trend_fast > trend_slow
            && close > trend_fast
            && fast > medium
            && medium > slow
            && bull_fan
            && bull_breakout
            && bull_pullback;
        let go_short = trend_fast < trend_slow
            && close < trend_fast
            && fast < medium
            && medium < slow
            && bear_fan
            && bear_breakout
            && bear_pullback;
        let open_long = go_long && !self.in_long;
        let open_short = go_short && !self.in_short;
        let entering = open_long || open_short;
        let in_trade = self.in_long || self.in_short;

        let [local_high, _, local_low] = self.extremes.next(bar);

        let mut entry_level = f64::NAN;
        if entering {
            entry_level = if go_long { local_high + buffer } else { local_low - buffer };
            self.stop_price = if open_long { bar.low - buffer } else { bar.high + buffer };
            let amount = (entry_level - self.stop_price).abs();
            self.slow_percent = ((1.0 - (entry_level - amount) / entry_level) * 100.0).abs();
        }
        let risk = strategy.equity() * RISK_PERCENT / 100.0;
        let quantity = sizing.floor((risk / self.slow_percent * 100.0).abs() / close);
        if open_long {
            strategy.entry("Long", Long).size(quantity).stop(entry_level);
            (self.in_long, self.in_short) = (true, false);
        }
        if open_short {
            strategy.entry("Short", Short).size(quantity).stop(entry_level);
            (self.in_long, self.in_short) = (false, true);
        }

        let (long_active, short_active) = (self.held_before > 0.0, self.held_before < 0.0);
        if !(long_active || short_active)
            && ((self.in_long && close < slow) || (self.in_short && close > slow))
        {
            (self.in_long, self.in_short) = (false, false);
            strategy.cancel_all();
            self.cancelled = true;
        }
        if entering {
            let tp_percent = self.slow_percent * REWARD;
            self.profit_points = round(tp_percent / 100.0 * close / tick);
            self.loss_points = round(self.slow_percent / 100.0 * close / tick);
            let target = self.profit_points * tick;
            self.target_price = if open_long { entry_level + target } else { entry_level - target };
        }
        self.exits(strategy, tick);

        let exited = strategy
            .closed_trades()
            .unwrap_or(&[])
            .last()
            .is_some_and(|row| row.entry_timestamp == bar.timestamp);
        let stop_hit = ((short_active || exited) && bar.high >= self.stop_price)
            || ((long_active || exited) && bar.low <= self.stop_price);
        let target_hit = ((long_active || exited) && bar.high >= self.target_price)
            || ((short_active || exited) && bar.low <= self.target_price);
        if (in_trade && (stop_hit || target_hit) && !self.cancelled) || exited {
            (self.in_long, self.in_short) = (false, false);
        } else if self.cancelled {
            self.cancelled = false;
        }
        self.held_before = strategy.position();
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        if let Some(tick) = strategy.tick() {
            self.exits(strategy, tick);
        }
    }
}
