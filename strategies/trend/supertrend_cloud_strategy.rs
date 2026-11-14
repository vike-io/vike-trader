// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend Cloud Strategy" by jhanson107, https://www.tradingview.com/script/sO5mkXTE/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-09-20, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two Supertrend lines (multipliers 2.4 and 4.8 on ATR(4), vike's `Supertrend`; the rule writes the same band arithmetic out
//! itself and the two give the same trades). Long all of the account's worth when the close
//! crosses up through one line while over the other, short when it crosses down through one while under the other, each
//! reversing the other; every position closes when the close crosses a line to the side that leaves it between the two.
//! Only on the bars from 2019-01-01 to 2020-01-01. No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    first: Supertrend,
    second: Supertrend,
    /// The close and the two lines of the last bar.
    last: [f64; 3],
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        first: Supertrend::new(4.0, 2.4).expect("valid parameters"),
        second: Supertrend::new(4.0, 4.8).expect("valid parameters"),
        last: [f64::NAN; 3],
        from: timestamp(2019, 1, 1, 0, 0),
        to: timestamp(2020, 1, 1, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (l1, l2) = (self.first.next(bar)[0], self.second.next(bar)[0]);
        let [c0, p1, p2] = self.last;
        self.last = [bar.close, l1, l2];
        let c = bar.close;
        let over = |line: f64, previous: f64| c > line && c0 <= previous;
        let under = |line: f64, previous: f64| c < line && c0 >= previous;
        let long = over(l1, p1) && c > l2 || over(l2, p2) && c > l1;
        let short = under(l1, p1) && c < l2 || under(l2, p2) && c < l1;
        let cloud = under(l1, p1) && c > l2
            || over(l1, p1) && c < l2
            || under(l2, p2) && c > l1
            || over(l2, p2) && c < l1;
        if bar.timestamp >= self.from && bar.timestamp <= self.to {
            if long {
                strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, c));
            }
            if short {
                strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, c));
            }
        }
        if cloud {
            strategy.close("long");
            strategy.close("short");
        }
    }
}
