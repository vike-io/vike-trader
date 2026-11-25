// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Reversal Trading Bot Strategy [BullByte] Final Version" by BullByte, https://www.tradingview.com/script/cXVnf1WH/
// The original's notice: © BullByte
// Changed: rewritten by hand in Rust for vike.

//! Long 30% of the account's worth on a bullish divergence, short it on a bearish one, each reversing the other, filled at
//! the signal bar's close. Bullish: the lowest low of the 5 bars before this one is under that of the 5 before the last, while
//! the lowest RSI(8) over the same bars, the RSI read a bar late, is above. Bearish is the mirror with the highest high and
//! RSI. While held, from the bar after the fill, a stop 1% and a target 2% from the average cost. No costs; starts with
//! 1,000, version 6, margined at 100% (`main.rs`'s rule 4). The rule's filters are all off.
//!
//! The rule acts only on a confirmed bar, which a closed bar always is.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Donchian, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LOOKBACK: usize = 5;
const STOP: f64 = 0.01;
const TARGET: f64 = 0.02;

struct S {
    rsi: Rsi,
    last_rsi: f64,
    /// The extremes of the last 5 bars' prices and of the RSI series.
    price: Donchian,
    osc: Donchian,
    /// `[highest high, lowest low, highest rsi, lowest rsi]` of this bar and the two before, newest first.
    seen: VecDeque<[f64; 4]>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let donchian = || Donchian::new(LOOKBACK as f64).expect("a valid length");
    Box::new(S {
        rsi: Rsi::new(8.0).expect("a valid length"),
        last_rsi: f64::NAN,
        price: donchian(),
        osc: donchian(),
        seen: VecDeque::with_capacity(4),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = std::mem::replace(&mut self.last_rsi, self.rsi.next(bar));
        let [high, _, low] = self.price.next(bar);
        let [rsi_high, _, rsi_low] = self.osc.next_hl(rsi, rsi);
        self.seen.push_front([high, low, rsi_high, rsi_low]);
        self.seen.truncate(3);
        // The windows ending a bar back and two back.
        let at = |bars_back: usize, line: usize| {
            self.seen.get(bars_back).map_or(f64::NAN, |lines| lines[line])
        };
        let bull = at(1, 1) < at(2, 1) && at(1, 3) > at(2, 3);
        let bear = at(1, 0) > at(2, 0) && at(1, 2) < at(2, 2);

        let (held, cost) = (strategy.position(), strategy.avg_cost());
        if bull {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 30.0, bar.close));
        }
        if bear {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 30.0, bar.close));
        }
        if held > 0.0 {
            strategy
                .exit("Long Exit")
                .from("Long")
                .take(cost * (1.0 + TARGET))
                .stop(cost * (1.0 - STOP));
        }
        if held < 0.0 {
            strategy
                .exit("Short Exit")
                .from("Short")
                .take(cost * (1.0 - TARGET))
                .stop(cost * (1.0 + STOP));
        }
    }
}
