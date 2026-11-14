// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Buy Sell Strategy With Z-Score [TradeDots]" by tradedots, https://www.tradingview.com/script/tb0HYAkd/
// The original's notice: © tradedots
// Changed: rewritten by hand in Rust for vike.

//! The z-score of the close over 80 bars, against the sample standard deviation. Over 2.8: close every long and add a short; under
//! -2.8: close every short and add a long; each entry 30% of the account's worth, any number of them on one side. A side that has
//! just signalled waits: a second signal of the same side within 5 bars of the side's last one only counts the bar, and an
//! opposite signal makes the other side ready at once. 0.03% commission; starts with 10,000.
//!
//! The reference takes the mean and the variance from running sums of the closes and of their squares,
//! `q / (n - 1) - p * m / (n - 1)`; vike's `Zscore` with the sample deviation is the same number to the rounding of those sums.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Zscore;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const LONG_NAME: &str = "Buy";
const SHORT_NAME: &str = "Sell";
const THRESHOLD: f64 = 2.8;
const COOL_DOWN: u32 = 5;
const SHARE: f64 = 30.0;

struct S {
    zscore: Zscore,
    buy_wait: u32,
    sell_wait: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        zscore: Zscore::new(80.0).expect("a valid length").with_sample_deviation(true),
        buy_wait: COOL_DOWN,
        sell_wait: COOL_DOWN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            fee: RuleDefaultsFee::Percent(0.03),
            pyramiding: 9999,
            ..RuleDefaults::PLAIN
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let z = self.zscore.next(bar);
        if z > THRESHOLD {
            if self.sell_wait >= COOL_DOWN {
                strategy.close(LONG_NAME);
                strategy.entry(SHORT_NAME, Short).size(share_of_worth(&sizing, SHARE, bar.close));
                (self.sell_wait, self.buy_wait) = (0, COOL_DOWN);
            } else {
                self.sell_wait += 1;
            }
        } else if z < -THRESHOLD {
            if self.buy_wait >= COOL_DOWN {
                strategy.close(SHORT_NAME);
                strategy.entry(LONG_NAME, Long).size(share_of_worth(&sizing, SHARE, bar.close));
                (self.sell_wait, self.buy_wait) = (COOL_DOWN, 0);
            } else {
                self.buy_wait += 1;
            }
        }
    }
}
