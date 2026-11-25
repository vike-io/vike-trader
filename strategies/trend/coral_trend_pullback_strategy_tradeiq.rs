// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Coral Trend Pullback Strategy (TradeIQ)" by kevinmck100, https://www.tradingview.com/script/kc6Itas8/
// The original's notice: © kevinmck100
// Changed: rewritten by hand in Rust for vike.

//! A pullback into a smoothed trend line (the Coral Trend: six chained EMAs of the close, 25 smoothing, constant 0.4, combined into
//! `bfr`), confirmed by an ADX of 14. The trend is up when `bfr` rose, down when it fell. A long needs: the trend up; since the
//! close last crossed up through `bfr`, a bar that lay wholly over it (a count smaller than the cross's count a bar before); a
//! pullback that began (the close crossing down through `bfr`) after the trend last turned up; the close now crossing up through
//! `bfr`; and +DI over -DI with the ADX over 20 and rising. A short is the mirror. An entry waits until the side's flag, set by its
//! own entry and cleared when a bar's range reaches the stop or target the rule priced from the signal close, is clear.
//!
//! The size risks 1% of the account's worth to a stop at the lowest low (highest high) of the last 5 bars: `worth * 1% /
//! stop_percent / close`, floored to the step. The stop and the target (1.5 times the stop's distance) are in ticks from the entry's
//! fill, so they rest once it has filled. 0.02% commission, no currency conversion; starts with 1,000. The rule's other confirmation
//! (an absolute-strength histogram with a volume reading) is off by default.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Donchian, T3};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, fewer};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SMOOTHING: f64 = 25.0;
const CONSTANT: f64 = 0.4;
const ADX_LENGTH: f64 = 14.0;
const REWARD: f64 = 1.5;
const RISK_PERCENT: f64 = 1.0;
const LOOKBACK: f64 = 5.0;

struct S {
    /// The Coral Trend is a Tillson T3 of the close: 25 smoothing is `(25 - 1) / 2 + 1 = 13` bars, the constant its volume factor.
    coral: T3,
    bfr: f64,
    /// The rule's ADX of 14: Wilder sums (a sum less its length-th part, plus the new value) from 0 on the first bar, the ADX the
    /// 14-bar mean of the DX.
    adx: Adx,
    adx_before: f64,
    price_cross: Cross,
    color_cross: Cross,
    since_over: BarsSince,
    since_under: BarsSince,
    since_wholly_above: BarsSince,
    since_wholly_below: BarsSince,
    since_up_color: BarsSince,
    since_down_color: BarsSince,
    extremes: Donchian,
    in_long: bool,
    in_short: bool,
    profit_points: f64,
    loss_points: f64,
    stop_price: f64,
    target_price: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        coral: T3::new((SMOOTHING - 1.0) / 2.0 + 1.0, CONSTANT).expect("valid parameters"),
        bfr: f64::NAN,
        adx: Adx::new(ADX_LENGTH).expect("a valid length").with_zero_seeded_sums(true),
        adx_before: f64::NAN,
        price_cross: Cross::new(),
        color_cross: Cross::new(),
        since_over: BarsSince::default(),
        since_under: BarsSince::default(),
        since_wholly_above: BarsSince::default(),
        since_wholly_below: BarsSince::default(),
        since_up_color: BarsSince::default(),
        since_down_color: BarsSince::default(),
        extremes: Donchian::new(LOOKBACK).expect("a valid length"),
        in_long: false,
        in_short: false,
        profit_points: 0.0,
        loss_points: 0.0,
        stop_price: 0.0,
        target_price: 0.0,
    })
}

/// The reference's `math.round`: halves go up.
fn round(x: f64) -> f64 {
    (x + 0.5).floor()
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let close = bar.close;
        let before = if self.bfr.is_nan() { 0.0 } else { self.bfr };
        let bfr = self.coral.next_value(close);
        self.bfr = bfr;
        let bullish = bfr > before;
        let bearish = bfr < before;

        let signal = self.price_cross.step(close, bfr);
        let (over, under) = (matches!(signal, Signal::Long), matches!(signal, Signal::Short));
        let color = self.color_cross.step(bfr, before);
        let (turned_up, turned_down) =
            (matches!(color, Signal::Long), matches!(color, Signal::Short));

        // The counts the rule compares: those of the cross a bar ago, and now.
        let (over_before, under_before) = (self.since_over.0, self.since_under.0);
        let since_over = self.since_over.step(over);
        let since_under = self.since_under.step(under);
        let wholly_above = self.since_wholly_above.step(bar.low > bfr && bar.high > bfr);
        let wholly_below = self.since_wholly_below.step(bar.low < bfr && bar.high < bfr);
        let pre_bull = fewer(wholly_above, over_before);
        let pre_bear = fewer(wholly_below, under_before);
        let since_up = self.since_up_color.step(turned_up);
        let since_down = self.since_down_color.step(turned_down);
        let (pullback_start, flip) = if bullish {
            (since_under, since_up)
        } else if bearish {
            (since_over, since_down)
        } else {
            (None, None)
        };
        let pullback_valid = fewer(pullback_start, flip);
        let entry_breakout = (bullish && over) || (bearish && under);

        let [adx, di_plus, di_minus] = self.adx.next(bar);
        let adx_valid = adx > 20.0 && adx > self.adx_before;
        self.adx_before = adx;
        let long_confirmed = di_plus > di_minus && adx_valid;
        let short_confirmed = di_minus > di_plus && adx_valid;

        let [highest, _, lowest] = self.extremes.next(bar);

        let go_long = bullish && pre_bull && pullback_valid && entry_breakout && long_confirmed;
        let go_short = bearish && pre_bear && pullback_valid && entry_breakout && short_confirmed;
        let open_long = go_long && !self.in_long;
        let open_short = go_short && !self.in_short;
        let in_trade = self.in_long || self.in_short;

        let stop_amount = if bullish { close - lowest } else { highest - close };
        let stop_percent = ((1.0 - (close - stop_amount) / close) * 100.0).abs();
        let quantity = sizing
            .floor((strategy.equity() * RISK_PERCENT / 100.0 / stop_percent * 100.0).abs() / close);
        if open_long {
            strategy.entry("Long", Long).size(quantity);
            (self.in_long, self.in_short) = (true, false);
        }
        if open_short {
            strategy.entry("Short", Short).size(quantity);
            (self.in_long, self.in_short) = (false, true);
        }
        if open_long || open_short {
            self.profit_points = round(stop_percent * REWARD / 100.0 * close / tick);
            self.loss_points = round(stop_percent / 100.0 * close / tick);
            let (stop, target) = (self.loss_points * tick, self.profit_points * tick);
            (self.stop_price, self.target_price) = if open_long {
                (close - stop, close + target)
            } else {
                (close + stop, close - target)
            };
        }

        let stop_hit = (self.in_short && bar.high >= self.stop_price)
            || (self.in_long && bar.low <= self.stop_price);
        let target_hit = (self.in_long && bar.high >= self.target_price)
            || (self.in_short && bar.low <= self.target_price);
        if in_trade && (stop_hit || target_hit) {
            (self.in_long, self.in_short) = (false, false);
        }
    }

    /// The stop and target are in ticks from the entry's fill, so they rest once it has filled.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (held, cost) = (strategy.position(), strategy.avg_cost());
        let (profit, loss) = (self.profit_points * tick, self.loss_points * tick);
        if held > 0.0 {
            strategy.exit("TP/SL Long").from("Long").take(cost + profit).stop(cost - loss);
        } else if held < 0.0 {
            strategy.exit("TP/SL Short").from("Short").take(cost - profit).stop(cost + loss);
        }
    }
}
