// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Advanced VWAP-Momentum_Pullback Strategy_Trend-Template Qualifier" by JS_TechTrading, https://www.tradingview.com/script/KafnxzJR/
// The original's notice: copyright JS-TechTrading 2023
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-03-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A pullback buy, long only and only when flat: long all of the account's worth when SMA(7) of the close crosses up through
//! the day's VWAP and RSI(14) was at or under 30 within the last 10 bars. Nothing closes it: the rule's stop, trailing stop
//! and target are off by default, and so are its nine trend-template qualifiers. No costs; starts with 100,000 in an account
//! kept in USD. Over the reference's window it opens one long and never closes it: no trade closes, as in the reference.
//!
//! The VWAP is the UTC day's running sum of the typical price `(high + low + close) / 3` times volume over its volume
//! (vike's `Vwap`, `VwapSource::TypicalDivided`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Sma, Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::BarsSince;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const OVERSOLD: f64 = 30.0;
/// The most bars since the RSI was oversold.
const RECENT: usize = 10;

struct S {
    rsi: IndicatorLine,
    oversold: BarsSince,
    average: Sma,
    vwap: Vwap,
    /// SMA(7) and the VWAP on the bar before.
    last: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        oversold: BarsSince::default(),
        average: Sma::new(7.0).expect("a valid length"),
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        last: (f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let was_oversold = self.oversold.step(rsi <= OVERSOLD).is_some_and(|n| n <= RECENT);
        let average = self.average.next_value(bar.close);
        let vwap = self.vwap.next_ts_hlcv(bar.timestamp, bar.high, bar.low, bar.close, bar.volume);
        let (last_average, last_vwap) = self.last;
        self.last = (average, vwap);
        let crossed_up = average > vwap && last_average <= last_vwap;
        if strategy.equity() > 0.0 && strategy.position() == 0.0 && was_oversold && crossed_up {
            let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
            strategy.entry("Long", Long).size(size);
        }
    }
}
