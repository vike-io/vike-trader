// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Combo 2/20 EMA & Accelerator Oscillator (AC)" by HPotter, https://www.tradingview.com/script/Cfz1HMO1/
// The original's notice: Copyright by HPotter v1.0 19/01/2022
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-01-19, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two votes, each holding its last side. The first: of this bar's and the previous bar's highs the higher, and of their lows
//! the lower; the low pair's when both lie on one side of EMA(14) of the close, otherwise the high pair's. That price above the
//! previous close votes short, below it long. The second, the accelerator oscillator: the 6-bar less the 33-bar SMA of the
//! median price, less its own 6-bar SMA; above 0 votes long, below 0 short. Long 1 when both vote long, short 1 when both vote
//! short, each reversing the other, and the position closed when they differ. No costs; starts with 1,000,000.
//!
//! A pair with no previous bar is NaN, as the rule's `max` and `min` give it. The rule's start date (2005) holds on every bar
//! here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    ema: Ema,
    /// The previous bar's `(high, low, close)`.
    last: (f64, f64, f64),
    ema_vote: i32,
    fast: Sma,
    slow: Sma,
    smooth: Sma,
    ac_vote: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: Ema::new(14.0).expect("a valid length"),
        last: (f64::NAN, f64::NAN, f64::NAN),
        ema_vote: 0,
        fast: Sma::new(6.0).expect("a valid length"),
        slow: Sma::new(33.0).expect("a valid length"),
        smooth: Sma::new(6.0).expect("a valid length"),
        ac_vote: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (high, low, close) = self.last;
        let average = self.ema.next_value(bar.close);
        let (pair_high, pair_low) = if high.is_nan() {
            (f64::NAN, f64::NAN)
        } else {
            (bar.high.max(high), bar.low.min(low))
        };
        let price = if pair_low > average || pair_high < average { pair_low } else { pair_high };
        if price > close {
            self.ema_vote = -1;
        } else if price < close {
            self.ema_vote = 1;
        }
        self.last = (bar.high, bar.low, bar.close);

        let median = (bar.high + bar.low) / 2.0;
        let diff = self.fast.next_value(median) - self.slow.next_value(median);
        // The SMA of the difference starts with the difference's first number, as the rule's NaN bars never enter it.
        let ac = if diff.is_nan() { f64::NAN } else { diff - self.smooth.next_value(diff) };
        if ac > 0.0 {
            self.ac_vote = 1;
        } else if ac < 0.0 {
            self.ac_vote = -1;
        }

        match (self.ema_vote, self.ac_vote) {
            (1, 1) => {
                strategy.entry("long", Long).size(1.0);
            }
            (-1, -1) => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {
                strategy.close_all();
            }
        }
    }
}
