// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "YinYang RSI Volume Trend Strategy" by YinYangAlgorithms, https://www.tradingview.com/script/cO34DTxx/
// The original's notice: © YinYangAlgorithms
// Changed: rewritten by hand in Rust for vike.

//! A purchase zone round a smoothed middle line. Over 80 bars: the middle is the mean of the volume-weighted averages
//! (VWMA) of the highest high and of the lowest low, smoothed by EMA(80); the zone's half-width is the 80-bar RSI, as a
//! fraction, of the 3-bar VWMA of the smoothed middle times the volume over its 80-bar VWMA, times the 80-bar SMA of the
//! distance between the two averages. The stop lines lie 0.1% beyond the zone; the basis is its middle.
//!
//! Long 1,000,000 worth when the close crosses up through the zone's bottom after it was armed, which a cross down through
//! the bottom or through the top does and a long entry spends. A long is closed when the close crosses up through the top,
//! crosses down through the lower stop line, or crosses down through the basis after crossing up through it since the last
//! cross through the top. A short is the mirror. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema, Rsi, Sma, Vwma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 80.0;
const CASH: f64 = 1_000_000.0;
const STOP: f64 = 0.1;

/// One side's state: whether an entry is armed, and whether a take-profit has been reached.
#[derive(Default)]
struct Side {
    armed: bool,
    profit_armed: bool,
}

struct S {
    range: Donchian,
    high_average: Vwma,
    low_average: Vwma,
    smooth: Ema,
    volume_average: Vwma,
    weighted_mid: Vwma,
    width: Sma,
    rsi: Rsi,
    bottom: Cross,
    top: Cross,
    low_stop: Cross,
    high_stop: Cross,
    basis: Cross,
    long: Side,
    short: Side,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Donchian::new(LENGTH).expect("a valid length"),
        high_average: Vwma::new(LENGTH).expect("a valid length"),
        low_average: Vwma::new(LENGTH).expect("a valid length"),
        smooth: Ema::new(LENGTH).expect("a valid length").with_skip_nan(true),
        volume_average: Vwma::new(LENGTH).expect("a valid length"),
        weighted_mid: Vwma::new(3.0).expect("a valid length"),
        width: Sma::new(LENGTH).expect("a valid length"),
        rsi: Rsi::new(LENGTH).expect("a valid length"),
        bottom: Cross::new(),
        top: Cross::new(),
        low_stop: Cross::new(),
        high_stop: Cross::new(),
        basis: Cross::new(),
        long: Side::default(),
        short: Side::default(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [highest, _, lowest] = self.range.next(bar);
        let mid_high = self.high_average.next_cv(highest, bar.volume);
        let mid_low = self.low_average.next_cv(lowest, bar.volume);
        let mid = (mid_high + mid_low) / 2.0;
        let smoothed = self.smooth.next_value(mid);
        let volume_ratio = bar.volume / self.volume_average.next_cv(bar.volume, bar.volume);
        let weighted = self.weighted_mid.next_cv(smoothed * volume_ratio, bar.volume);
        let width = self.width.next_value(mid_high - mid_low);
        // The RSI starts with the first number it is given.
        let rsi = if weighted.is_nan() { f64::NAN } else { self.rsi.next_value(weighted) };
        let add = rsi * 0.01 * width;
        let high = smoothed + add;
        let low = smoothed - add;
        let basis = (high + low) / 2.0;
        let high_stop = high * (1.0 + STOP * 0.01);
        let low_stop = low * (1.0 - STOP * 0.01);

        let close = bar.close;
        let bottom = self.bottom.step(close, low);
        let top = self.top.step(close, high);
        let low_stop = self.low_stop.step(close, low_stop);
        let high_stop = self.high_stop.step(close, high_stop);
        let basis = self.basis.step(close, basis);
        let (up, down) =
            (|s: Signal| matches!(s, Signal::Long), |s: Signal| matches!(s, Signal::Short));

        let long_start = up(bottom) && self.long.armed;
        self.long.armed = if down(top) || down(bottom) {
            true
        } else if long_start {
            false
        } else {
            self.long.armed
        };
        let long_end = up(top);
        self.long.profit_armed = if up(basis) {
            true
        } else if long_end {
            false
        } else {
            self.long.profit_armed
        };
        let long_exit = long_end || down(low_stop) || (down(basis) && self.long.profit_armed);

        let short_start = down(top) && self.short.armed;
        self.short.armed = if up(bottom) || up(top) {
            true
        } else if short_start {
            false
        } else {
            self.short.armed
        };
        let short_end = down(bottom);
        self.short.profit_armed = if down(basis) {
            true
        } else if short_end {
            false
        } else {
            self.short.profit_armed
        };
        let short_exit = short_end || up(high_stop) || (up(basis) && self.short.profit_armed);

        if long_start {
            strategy.entry("buy", Long).size(cash_worth(&sizing, CASH, close));
        } else if long_exit {
            strategy.close("buy");
        }
        if short_start {
            strategy.entry("sell", Short).size(cash_worth(&sizing, CASH, close));
        } else if short_exit {
            strategy.close("sell");
        }
    }
}
