// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Buy&Sell Strategy depends on AO+Stoch+RSI+ATR by SerdarYILMAZ" by SerdarYILMAZ, https://www.tradingview.com/script/RpYoQCVJ/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-09-20, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when the 3-bar SMA of the 14-bar stochastic is under 20, RSI(10) under 30 and the awesome oscillator (SMA(5) less
//! SMA(34) of the bar's midpoint) rising; short 1 on the mirror (over 80, over 70, falling). Each signal reverses the other side
//! and issues, from the same bar, a stop and a target one ATR(14) away: for a long a stop under the bar's low and a target over
//! its close, for a short a stop over its high and a target under its close. A signal on the side already held moves them.
//! No costs.
//!
//! The ATR is Wilder's average (`Smma::with_alpha_steps`) of a true range that is undefined on the first bar (no close before
//! it: `TrueRange::with_first_bar_missing`), so it starts on the 15th.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, Smma, Stochastic, TrueRange};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "Long Position";
const DOWN: &str = "Short Position";
const ATR_LENGTH: f64 = 14.0;

struct S {
    fast: Sma,
    slow: Sma,
    stoch: Stochastic,
    rsi: IndicatorLine,
    range: TrueRange,
    atr: Smma,
    previous_awesome: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let atr = Smma::new(ATR_LENGTH).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        fast: Sma::new(5.0).expect("a valid length"),
        slow: Sma::new(34.0).expect("a valid length"),
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
        rsi: IndicatorLine::new("rsi", &[10.0]),
        range: TrueRange::default().with_first_bar_missing(true),
        atr: atr.with_alpha_steps(true),
        previous_awesome: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let mid = (bar.high + bar.low) / 2.0;
        let awesome = (self.fast.next_value(mid) - self.slow.next_value(mid)) * 1000.0;
        let k = self.stoch.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next_value(self.range.next(bar));
        let long = k < 20.0 && rsi < 30.0 && awesome > self.previous_awesome;
        let short = k > 80.0 && rsi > 70.0 && awesome < self.previous_awesome;
        self.previous_awesome = awesome;
        if long {
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("TP/SL long").from(UP).take(bar.close + atr).stop(bar.low - atr);
        }
        if short {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("TP/SL short").from(DOWN).take(bar.close - atr).stop(bar.high + atr);
        }
    }
}
