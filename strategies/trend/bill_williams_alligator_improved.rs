// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bill Williams Alligator improved" by 03.freeman, https://www.tradingview.com/script/b2ShO5Fv/
// The original's notice: © 03.freeman
// Changed: rewritten by hand in Rust for vike.

//! The Alligator's three lines on Heikin-Ashi candles of the chart's own bars: SMAs of the candle's midpoint over 13 (jaw), 8
//! (teeth) and 5 (lips) bars. The candle direction is up when this candle and the one before are both up, down when both are
//! down, else none. Long 1 when the direction is up and jaw < teeth < lips; closed when jaw > teeth, jaw > lips or teeth >
//! lips. Short 1 and its close are the mirror. No costs; starts with 100,000. The rule's date window, stops and targets are off.
//!
//! The candles start at the rule's first bar, every price on the instrument's tick (`vike_indicators::strategy_kit::HeikinAshiCandles`), and the rule
//! fills at real prices: its call for the Heikin-Ashi series on the chart's own timeframe is the series at this bar. The
//! rule's lines are plain SMAs of the candle's midpoint, not vike's `alligator` (Wilder-smoothed and shifted forward). The
//! close is issued before the opposite entry, as the rule's statements run.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_indicators::strategy_kit::HeikinAshiCandles;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    candles: HeikinAshiCandles,
    /// The last candle was up and down.
    was: (bool, bool),
    jaw: Sma,
    teeth: Sma,
    lips: Sma,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        candles: HeikinAshiCandles::default(),
        was: (false, false),
        jaw: sma(13.0),
        teeth: sma(8.0),
        lips: sma(5.0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Bar { open, high, low, close, .. } = self.candles.next(bar, strategy.tick());
        let (up, down) = (close > open, close < open);
        let direction = if up && self.was.0 {
            1
        } else if down && self.was.1 {
            -1
        } else {
            0
        };
        self.was = (up, down);

        let mid = (high + low) / 2.0;
        let (jaw, teeth, lips) =
            (self.jaw.next_value(mid), self.teeth.next_value(mid), self.lips.next_value(mid));
        if direction == 1 && jaw < teeth && jaw < lips && teeth < lips {
            strategy.entry("Buy", Long).size(1.0);
        }
        if jaw > teeth || jaw > lips || teeth > lips {
            strategy.close("Buy");
        }
        if direction == -1 && jaw > teeth && jaw > lips && teeth > lips {
            strategy.entry("Sell", Short).size(1.0);
        }
        if jaw < teeth || jaw < lips || teeth < lips {
            strategy.close("Sell");
        }
    }
}
