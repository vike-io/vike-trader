// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Long Only Strategy" by coingrabber, https://www.tradingview.com/script/yeBIG7oZ/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-10-23, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when the 2-bar SMA of the close crosses up through EMA(21) + ATR(21); closed when
//! EMA(21) - ATR(21) crosses up through that SMA. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";

struct S {
    price: IndicatorLine,
    average: IndicatorLine,
    atr: IndicatorLine,
    bull: Cross,
    bear: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        price: IndicatorLine::new("sma", &[2.0]),
        average: IndicatorLine::new("ema", &[21.0]),
        atr: IndicatorLine::new("atr", &[21.0]),
        bull: Cross::new(),
        bear: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let price = self.price.next(bar)[0];
        let average = self.average.next(bar)[0];
        let diff = self.atr.next(bar)[0];
        let up = matches!(self.bull.step(price, average + diff), Signal::Long);
        let down = matches!(self.bear.step(average - diff, price), Signal::Long);
        if up {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if down {
            strategy.close(NAME);
        }
    }
}
