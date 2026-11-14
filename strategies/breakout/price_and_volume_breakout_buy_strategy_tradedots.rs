// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Price and Volume Breakout Buy Strategy [TradeDots]" by tradedots, https://www.tradingview.com/script/jc2hs2qK/
// The original's notice: © tradedots
// Changed: rewritten by hand in Rust for vike.

//! Long only. Long 70% of the account's worth when the close breaks above the highest high of the 60 bars before this one,
//! the volume above the highest volume of the 60 bars before, the close above SMA(200) of the close and no trade is open.
//! The long is closed once this close and the four before it are all below this bar's SMA(200). 0.01% commission; starts
//! with 10,000.
//!
//! The rule reads its open trades (`strategy.opentrades`): the engine's position.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "Long";

struct S {
    highs: Donchian,
    volumes: Donchian,
    sma: Sma,
    /// The highest high and volume of the 60 bars up to the bar before.
    last_high: f64,
    last_volume: f64,
    /// The four closes before this one, newest first.
    closes: [f64; 4],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        highs: Donchian::new(60.0).expect("a valid length"),
        volumes: Donchian::new(60.0).expect("a valid length"),
        sma: Sma::new(200.0).expect("a valid length"),
        last_high: f64::NAN,
        last_volume: f64::NAN,
        closes: [f64::NAN; 4],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let high = self.highs.next_hl(bar.high, bar.low)[0];
        let volume = self.volumes.next_hl(bar.volume, bar.volume)[0];
        let sma = self.sma.next_value(bar.close);
        if bar.close > self.last_high
            && bar.volume > self.last_volume
            && bar.close > sma
            && strategy.position() == 0.0
        {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 70.0, bar.close));
        }
        if bar.close < sma && self.closes.iter().all(|&c| c < sma) && strategy.position() > 0.0 {
            strategy.close(NAME);
        }
        self.closes = [bar.close, self.closes[0], self.closes[1], self.closes[2]];
        (self.last_high, self.last_volume) = (high, volume);
    }
}
