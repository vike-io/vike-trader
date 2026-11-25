// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "inwCoin Martingale Strategy ( for Bitcoin )" by Real_inwCoin, https://www.tradingview.com/script/8OS1nbr8/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-26, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A martingale, long only. The price line is EMA(2) of the close, with EMA(12) and EMA(26) of it. From flat, with no round
//! open, buy 10% of the account's worth when the line turns green: the fast above the slow and the line above the fast, where
//! the bar before was not. Each bar the close is 10% or more below the average entry price, buy again, the first size times 2 to
//! the number of buys asked this round, the first size worked out again from this bar's worth. Everything is closed once the
//! close is more than 5% above the average entry price, which ends the round. Up to 100 entries; no costs; starts with 20,000.
//!
//! A buy asked counts towards the round even when the rule does not place it. The rule's date range (2018 on) holds on every
//! bar here, and the account's worth never falls to 0, which would close everything.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const START_PERCENT: f64 = 10.0;
const TAKE_PERCENT: f64 = 5.0;
const DOUBLE_PERCENT: f64 = 10.0;
const MULTIPLIER: f64 = 2.0;

struct S {
    line: Ema,
    fast: Ema,
    slow: Ema,
    was_green: bool,
    /// Buys asked this round, 0 with no round open.
    count: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        line: Ema::new(2.0).expect("a valid length"),
        fast: Ema::new(12.0).expect("a valid length").with_skip_nan(true),
        slow: Ema::new(26.0).expect("a valid length").with_skip_nan(true),
        was_green: false,
        count: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 100, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let line = self.line.next_value(close);
        let fast = self.fast.next_value(line);
        let slow = self.slow.next_value(line);
        let green = fast > slow && line > fast;
        let buy = green && !self.was_green;
        self.was_green = green;

        let avg = strategy.avg_cost();
        let percent = (close - avg) * 100.0 / avg;
        let mut sell = percent > TAKE_PERCENT;
        let more = percent <= -DOUBLE_PERCENT;
        let worth = strategy.equity();
        let unit = worth * (START_PERCENT / 100.0) / close;
        let doubled = unit * libm::pow(MULTIPLIER, f64::from(self.count));
        let held = strategy.position();
        if buy && self.count == 0 && worth >= 0.0 {
            if held <= 0.0 {
                strategy.entry(NAME, Long).size(sizing.floor(unit));
            }
            self.count = 1;
            sell = false;
        }
        if more && self.count > 0 {
            if held > 0.0 && worth >= 0.0 {
                strategy.entry(NAME, Long).size(sizing.floor(doubled));
            }
            self.count += 1;
            sell = false;
        }
        if sell || worth <= 0.0 {
            strategy.close(NAME);
            self.count = 0;
        }
    }
}
