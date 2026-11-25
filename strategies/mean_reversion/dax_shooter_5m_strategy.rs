// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "DAX Shooter 5M Strategy" by th3web, https://www.tradingview.com/script/CfgKji30/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-09-09, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! 1 lot on a limit order that lives one bar. Long at the upper band of a 50-bar Bollinger band of two standard deviations when
//! RSI(7) crosses up through 30, ADX(14, 14) is over 32 and the low is under the lower band; short at the lower band when RSI(7)
//! crosses down through 70, ADX is over 32 and the high is over the upper band. Each is pulled on the next bar its condition does
//! not hold, and the two share a group in which one fill pulls the other. An entry reverses the other side; nothing else closes.
//! No costs; starts with 100,000.
//!
//! Each limit lies beyond the price on its own side, so it fills at the next open unless that open has gapped past it.
//!
//! The ADX is the rule's own: Wilder averages of the directional moves and of the true range, which is NaN on the first bar,
//! the DI lines holding their last value through a NaN (`Adx::with_held_lines`), and the ADX over `|+DI - -DI| / (+DI + -DI)`
//! with a zero sum read as 1.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Adx;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "COMPRA";
const SHORT_NAME: &str = "VENDI";
const GROUP: &str = "DaxShooter";
const ADX_OVER: f64 = 32.0;

struct S {
    adx: Adx,
    rsi: IndicatorLine,
    basis: IndicatorLine,
    deviation: IndicatorLine,
    oversold: Cross,
    overbought: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        adx: Adx::new(14.0).expect("a valid length").with_held_lines(true),
        rsi: IndicatorLine::new("rsi", &[7.0]),
        basis: IndicatorLine::new("sma", &[50.0]),
        deviation: IndicatorLine::new("stddev", &[50.0]),
        oversold: Cross::new(),
        overbought: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [adx, _, _] = self.adx.next(bar);
        let rsi = self.rsi.next(bar)[0];
        let basis = self.basis.next(bar)[0];
        let deviation = 2.0 * self.deviation.next(bar)[0];
        let (upper, lower) = (basis + deviation, basis - deviation);
        let up = matches!(self.oversold.step(rsi, 30.0), Signal::Long);
        let down = matches!(self.overbought.step(rsi, 70.0), Signal::Short);
        if up && adx > ADX_OVER && bar.low < lower {
            strategy.entry(LONG_NAME, Long).size(1.0).limit(upper).group(GROUP);
        } else {
            strategy.cancel(LONG_NAME);
        }
        if down && adx > ADX_OVER && bar.high > upper {
            strategy.entry(SHORT_NAME, Short).size(1.0).limit(lower).group(GROUP);
        } else {
            strategy.cancel(SHORT_NAME);
        }
    }
}
