// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fibonacci + RSI - Strategy" by MohamedYAbdelaziz, https://www.tradingview.com/script/Ouv7XVfc/
// The original's notice: © MohamedYAbdelaziz
// Changed: rewritten by hand in Rust for vike.

//! Bands around the 200-bar volume-weighted average of the typical price `(high + low + close) / 3`, at 1 and 0.764 times
//! three of its 200-bar standard deviations. A long limit at this close, all of the account's worth, when the low is under
//! the lower 1 band, RSI(14) of the bar before crossed up through 30, and the high is under the lower 0.764 band; a short
//! limit on the mirror (upper bands, crossing down through 70). Each rests until it fills or the next signal moves it.
//! Once the high passes the lower 0.764 band as it stood on the bar the long grew, a target at that level and a stop 0.2%
//! under the average cost; the short the mirror. No costs; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Stddev, Vwma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "Long";
const SHORT_NAME: &str = "Short";
const LENGTH: f64 = 200.0;
const MULTIPLIER: f64 = 3.0;
const LEVEL: f64 = 0.764;
const LOSS: f64 = 0.002;

struct S {
    basis: Vwma,
    dev: Stddev,
    rsi: IndicatorLine,
    /// RSI one and two bars back.
    rsi1: f64,
    rsi2: f64,
    held: f64,
    exit_long: f64,
    exit_short: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        basis: Vwma::new(LENGTH).expect("a valid length"),
        dev: Stddev::new(LENGTH).expect("a valid length"),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        rsi1: f64::NAN,
        rsi2: f64::NAN,
        held: 0.0,
        exit_long: f64::NAN,
        exit_short: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let source = (bar.high + bar.low + bar.close) / 3.0;
        let basis = self.basis.next_cv(source, bar.volume);
        let dev = MULTIPLIER * self.dev.next_value(source);
        let upper = basis + LEVEL * dev;
        let top = basis + dev;
        let lower = basis - LEVEL * dev;
        let bottom = basis - dev;
        let rsi = self.rsi.next(bar)[0];
        let crossed_up = self.rsi1 > 30.0 && self.rsi2 <= 30.0;
        let crossed_down = self.rsi1 < 70.0 && self.rsi2 >= 70.0;
        (self.rsi2, self.rsi1) = (self.rsi1, rsi);

        let held = strategy.position();
        if held > self.held {
            self.exit_long = lower;
        }
        if held < self.held {
            self.exit_short = upper;
        }
        self.held = held;

        if bar.low < bottom && crossed_up && bar.high < lower {
            strategy
                .entry(LONG_NAME, Long)
                .size(share_of_worth(&sizing, 100.0, bar.close))
                .limit(bar.close);
        }
        if bar.high > top && crossed_down && bar.low > upper {
            strategy
                .entry(SHORT_NAME, Short)
                .size(share_of_worth(&sizing, 100.0, bar.close))
                .limit(bar.close);
        }
        let cost = strategy.avg_cost();
        if bar.high > self.exit_long && held > 0.0 {
            strategy
                .exit("exit long")
                .from(LONG_NAME)
                .take(self.exit_long)
                .stop(cost * (1.0 - LOSS));
        }
        if bar.low < self.exit_short && held < 0.0 {
            strategy
                .exit("exit short")
                .from(SHORT_NAME)
                .take(self.exit_short)
                .stop(cost * (1.0 + LOSS));
        }
    }
}
