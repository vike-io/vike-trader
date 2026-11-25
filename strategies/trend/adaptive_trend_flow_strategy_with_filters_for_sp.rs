// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Adaptive Trend Flow Strategy with Filters for SPX" by PineIndicators, https://www.tradingview.com/script/CNJ1hXQw/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-01-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A trend over bands around the typical price: the basis is the mean of its EMA(2) and EMA(4), the bands that basis plus and
//! minus twice an EMA(2) of its 2-bar standard deviation. The trend turns down when the close goes below the lower band and up
//! when it goes above the upper one. Long `4.5 * worth / close` when it turns up with the close above SMA(4) of the close and
//! the MACD (2, 7, 2) above its signal line; closed when it turns down. 0.01 a fill and 2 ticks of slippage; starts with 1,000.
//!
//! The trend is first set on the first bar that has a basis and bands, up when the close is above the basis.
//!
//! The header states a 20% margin, and 4.5 times the account outruns it on a fall of about 2.8%: the engine's margin call
//! (`[engine.margin_call]`) cuts the long, each cut a trade of its own.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Macd, Sma, Stddev};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Long";
const SENSITIVITY: f64 = 2.0;
const LEVERAGE: f64 = 4.5;

struct S {
    fast: Ema,
    slow: Ema,
    deviation: Stddev,
    smooth: Ema,
    average: Sma,
    macd: Macd,
    /// The band the trend last stood on, NaN until the trend is set.
    level: f64,
    /// `1` up, `-1` down: this bar's and last bar's (`0` before the first).
    trend: i32,
    trend_before: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(2.0).expect("a valid length"),
        slow: Ema::new(4.0).expect("a valid length"),
        deviation: Stddev::new(2.0).expect("a valid length"),
        smooth: Ema::new(2.0).expect("a valid length").with_skip_nan(true),
        average: Sma::new(4.0).expect("a valid length"),
        macd: Macd::new(2.0, 7.0, 2.0).expect("valid lengths"),
        level: f64::NAN,
        trend: 0,
        trend_before: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_order(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        let basis = (self.fast.next_value(typical) + self.slow.next_value(typical)) / 2.0;
        let width = self.smooth.next_value(self.deviation.next_value(typical)) * SENSITIVITY;
        let (upper, lower) = (basis + width, basis - width);
        if self.level.is_nan() {
            self.trend = if bar.close > basis { 1 } else { -1 };
            self.level = if self.trend == 1 { lower } else { upper };
        }
        if self.trend == 1 {
            if bar.close < lower {
                self.trend = -1;
                self.level = upper;
            } else {
                self.level = lower;
            }
        } else if bar.close > upper {
            self.trend = 1;
            self.level = lower;
        } else {
            self.level = upper;
        }

        let above_average = bar.close > self.average.next(bar);
        let [macd, signal, _] = self.macd.next(bar);
        let rising = macd > signal;
        let turned = self.trend != self.trend_before && self.trend_before != 0;
        if turned && self.trend == 1 && above_average && rising {
            let worth = strategy.equity();
            strategy.entry(NAME, Long).size(sizing.floor(worth / bar.close * LEVERAGE));
        }
        if turned && self.trend == -1 {
            strategy.close(NAME);
        }
        self.trend_before = self.trend;
    }
}
