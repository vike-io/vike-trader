// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Davin's 10/200MA Pullback on SPY Strategy v2.0" by Gold_D_Roger, https://www.tradingview.com/script/Gkib6KgL/
// The original's notice: © Gold_D_Roger
// Changed: rewritten by hand in Rust for vike.

//! Long 10% of the account's worth when flat and either the close is above SMA(200) and below SMA(10), or it is more than 14%
//! under the 52-bar highest high. Closed when the close is above SMA(10) and below the last bar's low, or more than 15% of
//! the close under the entry's fill. 0.1 commission per BTC; starts with 10,000.
//!
//! The entry's fill is the rule's own: the open of the bar after the entry signal. The rule's date filter (2013 to 2099)
//! holds on every bar, and its exit on SMA(50) under SMA(200) is an input left off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const DIP: f64 = 14.0;
const STOP: f64 = 0.15;

struct S {
    long_average: IndicatorLine,
    short_average: IndicatorLine,
    highest: Donchian,
    last_low: f64,
    bought_last_bar: bool,
    buy_price: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        long_average: IndicatorLine::new("sma", &[200.0]),
        short_average: IndicatorLine::new("sma", &[10.0]),
        highest: Donchian::new(52.0).expect("a valid length"),
        last_low: f64::NAN,
        bought_last_bar: false,
        buy_price: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let long_average = self.long_average.next(bar)[0];
        let short_average = self.short_average.next(bar)[0];
        let [highest, _, _] = self.highest.next(bar);
        let close = bar.close;
        let change = (highest - close) / highest * 100.0;
        let held = strategy.position();
        let flat = held == 0.0;
        let buy = (close > long_average && close < short_average && flat) || (flat && change > DIP);
        let sell = close > short_average && held > 0.0 && close < self.last_low;
        let stop = held > 0.0 && (self.buy_price - close) / close > STOP;
        self.last_low = bar.low;

        if buy {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 10.0, close));
        }
        if self.bought_last_bar {
            self.buy_price = bar.open;
        }
        self.bought_last_bar = buy;
        if sell || stop {
            strategy.close(NAME);
            self.buy_price = f64::NAN;
        }
    }
}
