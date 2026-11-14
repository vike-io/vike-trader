// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Supertrend advance" by JS_TechTrading, https://www.tradingview.com/script/IRCgOETG/
// The original's notice: © JS_TechTrading
// Changed: rewritten by hand in Rust for vike.

//! Supertrend(10, 3) pullbacks, entered only when flat, 1 BTC a trade. Long when the last bar's low crosses down through the up
//! trend's line, this bar's high is over the last bar's, and four filters agree: the close over EMA(200), RSI(14) at or over 50,
//! the MACD(12, 26) line over 0 and CCI(20) of the typical price over 200. Short is the mirror: the last bar's high crossing up
//! through the down trend's line, this bar's low under the last bar's, the close under the EMA, RSI at or under 50, the MACD line
//! under 0 and CCI under -200. While held, a stop 1% and a target 2% from the average cost. No costs; starts with 1,000,000.
//!
//! A trend's line is there only while that trend lasts, and the rule's cross compares with the last bar both sides were there,
//! so a pullback can fire on the first bar of a new trend against the end of the last one.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{CrossDefined, truthy};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "enter long";
const SHORT_NAME: &str = "enter short";
const QTY: f64 = 1.0;
const STOP: f64 = 0.01;
const TAKE: f64 = 0.02;

struct S {
    supertrend: Supertrend,
    ema: IndicatorLine,
    rsi: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cci: IndicatorLine,
    /// Last bar's low and high.
    low: f64,
    high: f64,
    pull_up: CrossDefined,
    pull_down: CrossDefined,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        supertrend: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        ema: IndicatorLine::new("ema", &[200.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        fast: IndicatorLine::new("ema", &[12.0]),
        slow: IndicatorLine::new("ema", &[26.0]),
        cci: IndicatorLine::new("cci", &[20.0]),
        low: f64::NAN,
        high: f64::NAN,
        pull_up: CrossDefined::default(),
        pull_down: CrossDefined::default(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The rule's direction: 1 down, -1 up.
        let [line, up] = self.supertrend.next(bar);
        let direction = -up;
        let ema = self.ema.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let macd = self.fast.next(bar)[0] - self.slow.next(bar)[0];
        let cci = self.cci.next(bar)[0];
        let (up_line, down_line) =
            if direction < 0.0 { (line, f64::NAN) } else { (f64::NAN, line) };
        let pull_buy =
            self.pull_up.step(self.low, up_line, false) && truthy(up_line) && bar.high > self.high;
        let pull_sell = self.pull_down.step(self.high, down_line, true)
            && truthy(down_line)
            && bar.low < self.low;
        let buy = pull_buy && rsi >= 50.0 && bar.close > ema && macd > 0.0 && cci > 200.0;
        let sell = pull_sell && rsi <= 50.0 && bar.close < ema && macd < 0.0 && cci < -200.0;
        (self.low, self.high) = (bar.low, bar.high);

        if buy && strategy.position() == 0.0 {
            strategy.entry(LONG_NAME, Long).size(QTY);
        }
        if sell && strategy.position() == 0.0 {
            strategy.entry(SHORT_NAME, Short).size(QTY);
        }
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy
                .exit("Close Long")
                .from(LONG_NAME)
                .take(cost * (1.0 + TAKE))
                .stop(cost * (1.0 - STOP));
        }
        if held < 0.0 {
            strategy
                .exit("Close Short")
                .from(SHORT_NAME)
                .take(cost * (1.0 - TAKE))
                .stop(cost * (1.0 + STOP));
        }
    }
}
