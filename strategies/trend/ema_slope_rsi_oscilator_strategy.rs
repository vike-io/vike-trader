// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Slope - RSI Oscilator Strategy" by xvelox_, https://www.tradingview.com/script/RNxThUtE/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-03-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! The slope of EMA(160) of the close, scaled by its own 140-bar deviation into -50 to 50 (an arctangent), against a smoothed RSI
//! (RSI(12), averaged over 3, less 50). One position at a time, long or short, 90% of the account's worth in BTC worked out at
//! the signal close, a position reversing the other side. An entry needs the slope outside the no-trade zone (over 10 either way),
//! the last two bars of its colour and one of three triggers: the slope crossing into the zone's far side, or already past it with
//! a slope change of 2.3 or more; a sudden raise (the slope's change against its own 200-bar peak, over 5 of 50) in the same
//! conditions; or a divergence of the slope and the RSI (the RSI under its values 5 and 10 bars back while the slope rises
//! outside the zone, 5 or more apart: a short; the mirror: a long). Long entries need EMA(20) over EMA(160) and the two within
//! 0.34% of each other, or the 20 over; shorts the mirror. A position is closed at the next open when the RSI crosses the slope
//! the other way with both heading that way on a bar of that colour (the cross counts up to 3 bars), or when the slope re-enters
//! the zone from beyond it. 0.04% commission; starts with 10,000.
//!
//! Most of the rule's inputs are off by default and are not ported: its trailing stops, RSI divergence entries, the RSI-slope
//! oscillator entries, reversing on a cross, the exit confirmation bars (0) and the table and alerts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use std::f64::consts::PI;
use vike_indicators::strategy_kit::max;
use vike_tactics::helpers::{back, push};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_indicators::indicators::{Donchian, Ema, Rsi, Sma, Stddev};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

/// The edge of the no-trade zone.
const ZONE: f64 = 10.0;
const CROSS_BARS: usize = 3;

struct S {
    index: usize,
    short_ma: Ema,
    long_ma: Ema,
    long_mas: VecDeque<f64>,
    deviation: Stddev,
    previous_slope: f64,
    /// The highest of the slope's last 200 changes: a Donchian's upper line of the series as both its high and low.
    peak: Donchian,
    rsi: Rsi,
    rsi_average: Sma,
    c_rsis: VecDeque<f64>,
    cross: Cross,
    previous_open: f64,
    previous_close: f64,
    wait_bear: bool,
    wait_bull: bool,
    cross_bar_bear: Option<usize>,
    cross_bar_bull: Option<usize>,
    exit_count_long: u32,
    exit_count_short: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        index: 0,
        short_ma: Ema::new(20.0).expect("a valid length"),
        long_ma: Ema::new(160.0).expect("a valid length"),
        long_mas: VecDeque::new(),
        deviation: Stddev::new(140.0).expect("a valid length"),
        previous_slope: f64::NAN,
        peak: Donchian::new(200.0).expect("a valid length"),
        rsi: Rsi::new(12.0).expect("a valid length"),
        rsi_average: Sma::new(3.0).expect("a valid length"),
        c_rsis: VecDeque::new(),
        cross: Cross::new(),
        previous_open: f64::NAN,
        previous_close: f64::NAN,
        wait_bear: false,
        wait_bull: false,
        cross_bar_bear: None,
        cross_bar_bull: None,
        exit_count_long: 0,
        exit_count_short: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let index = self.index;
        self.index += 1;
        let short_ma = self.short_ma.next_value(close);
        let long_ma = self.long_ma.next_value(close);
        let percent_diff = (long_ma - short_ma) / short_ma * 100.0;
        push(&mut self.long_mas, long_ma, 4);
        let raw = long_ma - back(&self.long_mas, 3);
        let scale = max(self.deviation.next_value(raw), 1e-10);
        let slope = 100.0 / PI * libm::atan(raw / scale);
        let before = self.previous_slope;
        self.previous_slope = slope;
        let delta = slope - before;
        let change = (slope - before).abs() * 3.0 * 2.0;
        let peak = self.peak.next_hl(change, change)[0];
        let acceleration = if peak > 0.0 { (50.0 * change / peak).min(50.0) } else { 0.0 };

        let rsi = self.rsi_average.next_value(self.rsi.next_value(close));
        let c_rsi = rsi - 50.0;
        let previous_c_rsi = back(&self.c_rsis, 0);
        push(&mut self.c_rsis, c_rsi, 11);
        let signal = self.cross.step(c_rsi, slope);
        let (cross_up, cross_down) =
            (matches!(signal, Signal::Long), matches!(signal, Signal::Short));
        let rsi_down = c_rsi < previous_c_rsi;
        let rsi_up = c_rsi > previous_c_rsi;
        let slope_up = slope > before;
        let slope_down = slope < before;

        // The cross that closes a position stays valid for a few bars.
        if cross_down {
            (self.wait_bear, self.wait_bull) = (true, false);
            (self.cross_bar_bear, self.cross_bar_bull) = (Some(index), None);
        }
        if cross_up {
            (self.wait_bull, self.wait_bear) = (true, false);
            (self.cross_bar_bull, self.cross_bar_bear) = (Some(index), None);
        }
        let since_bear = self.cross_bar_bear.map_or(usize::MAX, |b| index - b);
        let since_bull = self.cross_bar_bull.map_or(usize::MAX, |b| index - b);
        if self.wait_bear && self.cross_bar_bear.is_some() && since_bear > CROSS_BARS {
            (self.wait_bear, self.cross_bar_bear) = (false, None);
        }
        if self.wait_bull && self.cross_bar_bull.is_some() && since_bull > CROSS_BARS {
            (self.wait_bull, self.cross_bar_bull) = (false, None);
        }
        let valid_bear = self.cross_bar_bear.is_some() && since_bear <= CROSS_BARS;
        let valid_bull = self.cross_bar_bull.is_some() && since_bull <= CROSS_BARS;
        let cross_bearish = ((self.wait_bear && valid_bear) || cross_down)
            && rsi_down
            && slope_down
            && close < bar.open;
        let cross_bullish =
            ((self.wait_bull && valid_bull) || cross_up) && rsi_up && slope_up && close > bar.open;

        // The slope and the RSI parting ways.
        let acceleration_active = acceleration >= 5.0;
        let gap = (slope - c_rsi).abs();
        let (r5, r10) = (back(&self.c_rsis, 5), back(&self.c_rsis, 10));
        let bearish_div = acceleration_active
            && delta > 0.0
            && slope > ZONE
            && c_rsi < r5
            && c_rsi < r10
            && gap >= 5.0;
        let bullish_div = acceleration_active
            && delta < 0.0
            && slope < -ZONE
            && c_rsi > r5
            && c_rsi > r10
            && gap >= 5.0;

        // The entries.
        let delta_enough = delta.abs() >= 2.3;
        let stretch_long = percent_diff >= -0.34;
        let stretch_short = percent_diff <= 0.34;
        let outside = slope.abs() > ZONE;
        let (trend_long, trend_short) = (short_ma > long_ma, short_ma < long_ma);
        let last_long = close > bar.open && self.previous_close > self.previous_open;
        let last_short = close < bar.open && self.previous_close < self.previous_open;
        let cross_above = slope > ZONE && before <= ZONE;
        let cross_below = slope < -ZONE && before >= -ZONE;
        let accel_long = acceleration_active
            && delta_enough
            && delta > 0.0
            && slope > ZONE
            && stretch_long
            && trend_long;
        let accel_short = acceleration_active
            && delta_enough
            && delta < 0.0
            && slope < -ZONE
            && stretch_short
            && trend_short;
        let ntz_long = !cross_up
            && !cross_down
            && (cross_above || (slope > ZONE && delta_enough && delta > 0.0))
            && stretch_long
            && trend_long;
        let ntz_short = !cross_up
            && !cross_down
            && (cross_below || (slope < -ZONE && delta_enough && delta < 0.0))
            && stretch_short
            && trend_short;
        let held = strategy.position();
        let long_condition =
            held <= 0.0 && outside && last_long && (ntz_long || accel_long || bullish_div);
        let short_condition =
            held >= 0.0 && outside && last_short && (ntz_short || accel_short || bearish_div);
        let (mut suppress_long, mut suppress_short) = (false, false);
        if long_condition {
            suppress_short = held < 0.0;
            (self.exit_count_long, self.exit_count_short) = (0, 0);
        }
        if short_condition {
            suppress_long = held > 0.0;
            (self.exit_count_long, self.exit_count_short) = (0, 0);
        }
        let size = sizing.floor(strategy.equity() * 90.0 / 100.0 / close);
        if long_condition {
            strategy.entry("Long", Long).size(size);
        }
        if short_condition {
            strategy.entry("Short", Short).size(size);
        }

        // The exits.
        if held > 0.0 && !suppress_long && cross_bearish {
            strategy.close("Long");
            (self.wait_bear, self.wait_bull) = (false, false);
            self.exit_count_long = 0;
        }
        if held < 0.0 && !suppress_short && cross_bullish {
            strategy.close("Short");
            (self.wait_bear, self.wait_bull) = (false, false);
            self.exit_count_short = 0;
        }
        if held > 0.0 && !suppress_long {
            let reentry = slope <= ZONE && before > ZONE;
            let inside = slope <= ZONE;
            self.exit_count_long = if reentry {
                1
            } else if inside && self.exit_count_long > 0 {
                self.exit_count_long + 1
            } else {
                0
            };
            if inside && self.exit_count_long > 0 {
                strategy.close("Long");
                self.exit_count_long = 0;
            }
        }
        if held < 0.0 && !suppress_short {
            let reentry = slope >= -ZONE && before < -ZONE;
            let inside = slope >= -ZONE;
            self.exit_count_short = if reentry {
                1
            } else if inside && self.exit_count_short > 0 {
                self.exit_count_short + 1
            } else {
                0
            };
            if inside && self.exit_count_short > 0 {
                strategy.close("Short");
                self.exit_count_short = 0;
            }
        }
        self.previous_open = bar.open;
        self.previous_close = close;
    }
}
