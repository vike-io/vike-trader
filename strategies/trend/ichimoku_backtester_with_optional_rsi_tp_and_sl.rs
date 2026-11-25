// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Ichimoku Backtester with Optional RSI, TP, and SL" by gregh2, https://www.tradingview.com/script/fsYSuQGL/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-02-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Ichimoku: long 95% of the account's worth when the conversion line (9) crosses up through the base line (26), the close is
//! above its close 26 bars back and above the cloud as it stood 26 bars ago, and the first leading line is above the second
//! (52); short on the mirror. A rule-side flag keeps a second entry of the side last taken from firing, and each entry closes
//! the other side. No costs; starts with 100,000, its account stated in USD.
//!
//! Its RSI filters, stops, targets and date window are all off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Donchian, Ichimoku};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "Long";
const DOWN: &str = "Short";
const OFFSET: usize = 26;

struct S {
    /// The conversion and base lines, and the leading lines as they stood 26 bars ago.
    cloud: Ichimoku,
    /// The second leading line as it stands now.
    second: Donchian,
    cross: Cross,
    /// The closes, newest first, one more than the offset.
    closes: VecDeque<f64>,
    in_long: bool,
    in_short: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        cloud: Ichimoku::default(),
        second: Donchian::new(52.0).expect("a valid length"),
        cross: Cross::new(),
        closes: VecDeque::with_capacity(OFFSET + 1),
        in_long: false,
        in_short: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [tenkan, kijun, old_a, old_b, _] = self.cloud.next(bar);
        let lead_a = (tenkan + kijun) / 2.0;
        let lead_b = self.second.next(bar)[1];
        self.closes.push_front(bar.close);
        self.closes.truncate(OFFSET + 1);
        let old_close = self.closes.get(OFFSET).copied().unwrap_or(f64::NAN);
        // The reference's max and min of a missing value are missing.
        let (top, bottom) = if old_a.is_nan() || old_b.is_nan() {
            (f64::NAN, f64::NAN)
        } else {
            (old_a.max(old_b), old_a.min(old_b))
        };
        let crossed = self.cross.step(tenkan, kijun);
        let momentum = bar.close - old_close;
        let green = lead_a > lead_b;
        let long_entry = matches!(crossed, Signal::Long)
            && momentum > 0.0
            && bar.close > top
            && green
            && !self.in_long;
        let short_entry = matches!(crossed, Signal::Short)
            && momentum < 0.0
            && bar.close < bottom
            && !green
            && !self.in_short;
        let long_exit = short_entry && self.in_long;
        let short_exit = long_entry && self.in_short;
        if long_entry {
            (self.in_long, self.in_short) = (true, false);
        }
        if long_exit {
            self.in_long = false;
        }
        if short_entry {
            (self.in_short, self.in_long) = (true, false);
        }
        if short_exit {
            self.in_short = false;
        }
        let share = share_of_worth(&sizing, 95.0, bar.close);
        if long_entry {
            strategy.entry(UP, Long).size(share);
        }
        if short_entry {
            strategy.entry(DOWN, Short).size(share);
        }
        if long_exit {
            strategy.close(UP);
        }
        if short_exit {
            strategy.close(DOWN);
        }
    }
}
