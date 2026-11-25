// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TSI CCI Hull" by faresg7900, https://www.tradingview.com/script/qwN8CGrU/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-06-01, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth when the TSI's signal line is above itself two bars back, the Hull average of the Hull average
//! is under the close, and the CCI of the Hull average is above 0 and rising on a rising Hull; short on the mirror. Each
//! entry reverses the other, and a position closes on the opposite entry's condition or once the close is 0.5% past its
//! cost in its favour. 0.018% commission; starts with 100,000.
//!
//! The TSI is vike's `Tsi` (50, 50), times 5, and its signal line the EMA(125) of it. The Hull is vike's `Hma` of 26 (WMA(5) of
//! 2 WMA(13) less WMA(26), the 5 being the square root); the CCI is the close less that Hull over 0.015 times the close's 26-bar
//! mean absolute deviation (vike's `Mad`). The second Hull average is the same Hull taken of the first.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Ema, Hma, Mad, Tsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: usize = 26;
const PROFIT: f64 = 1.005;

struct S {
    tsi: Tsi,
    signal: Ema,
    /// The signal line, newest first, three of them.
    lines: VecDeque<f64>,
    deviation: Mad,
    hull: Hma,
    smooth: Hma,
    previous_cci: f64,
    previous_hull: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        tsi: Tsi::new(50.0, 50.0).expect("valid lengths"),
        signal: Ema::new(125.0).expect("a valid length").with_skip_nan(true),
        lines: VecDeque::with_capacity(3),
        deviation: Mad::new(LENGTH as f64).expect("a valid length"),
        hull: Hma::new(LENGTH as f64).expect("a valid length"),
        smooth: Hma::new(LENGTH as f64).expect("a valid length"),
        previous_cci: f64::NAN,
        previous_hull: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.018), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let line = self.signal.next_value(self.tsi.next_value(close) * 5.0);
        self.lines.push_front(line);
        self.lines.truncate(3);
        let before = self.lines.get(2).copied().unwrap_or(f64::NAN);

        let n1 = self.hull.next_value(close);
        let hull = self.smooth.next_value(n1);
        let deviation = self.deviation.next_value(close);
        let cci = (close - n1) / (0.015 * deviation);

        let up = line > before && hull < close && cci > 0.0;
        let down = line < before && hull > close && cci < 0.0;
        let enter_long = up && cci > self.previous_cci && cci > 0.0 && n1 > self.previous_hull;
        let enter_short = down && cci < self.previous_cci && cci < 0.0 && n1 < self.previous_hull;
        (self.previous_cci, self.previous_hull) = (cci, n1);

        let cost = strategy.avg_cost();
        if enter_long {
            strategy.entry("buy", Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if enter_short || close > cost * PROFIT {
            strategy.close("buy");
        }
        if enter_short {
            strategy.entry("sell", Short).size(share_of_worth(&sizing, 100.0, close));
        }
        if enter_long || close < cost / PROFIT {
            strategy.close("sell");
        }
    }
}
