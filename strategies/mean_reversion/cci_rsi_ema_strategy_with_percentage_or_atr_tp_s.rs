// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "CCI+RSI+EMA Strategy with Percentage or ATR TP/SL [Alifer]" by UnknownUnicorn55719247, https://www.tradingview.com/script/hHfPXUTx/
// The original's notice: © alifer123
// Changed: rewritten by hand in Rust for vike.

//! CCI(14) of the typical price `(high + low + close) / 3`: long 10% of the account's worth when it crosses up through -140 with
//! the close over EMA(55) of the typical price, short when it crosses down through 150 with the close under it, each reversing
//! the other. While held, a target and a stop 10% either side of the average cost. A long also closes when the CCI crosses up
//! through 150, a short when it crosses down through -140. 0.045% commission; starts with 10,000.
//!
//! The CCI is `(x - SMA(x)) / (0.015 * mean absolute deviation from it)`, both over 14 bars (vike's `Cci`). The rule's RSI filter
//! is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Cci, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const OVERBOUGHT: f64 = 150.0;
const OVERSOLD: f64 = -140.0;
const SHARE: f64 = 10.0;
const BAND: f64 = 10.0;

struct S {
    cci: Cci,
    ema: Ema,
    oversold: Cross,
    overbought: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        cci: Cci::new(14.0).expect("a valid length"),
        ema: Ema::new(55.0).expect("a valid length"),
        oversold: Cross::new(),
        overbought: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.045), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = (bar.high + bar.low + bar.close) / 3.0;
        let cci = self.cci.next(bar);
        let ema = self.ema.next_value(src);
        let oversold = self.oversold.step(cci, OVERSOLD);
        let overbought = self.overbought.step(cci, OVERBOUGHT);

        if matches!(oversold, Signal::Long) && bar.close > ema {
            strategy.entry("Buy", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if matches!(overbought, Signal::Short) && bar.close < ema {
            strategy.entry("Sell", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        let cost = strategy.avg_cost();
        if strategy.position() > 0.0 {
            strategy
                .exit("Long Exit")
                .from("Buy")
                .take(cost * (1.0 + BAND / 100.0))
                .stop(cost * (1.0 - BAND / 100.0));
        }
        if strategy.position() < 0.0 {
            strategy
                .exit("Short Exit")
                .from("Sell")
                .take(cost * (1.0 - BAND / 100.0))
                .stop(cost * (1.0 + BAND / 100.0));
        }
        if matches!(overbought, Signal::Long) {
            strategy.close("Buy");
        }
        if matches!(oversold, Signal::Short) {
            strategy.close("Sell");
        }
    }
}
