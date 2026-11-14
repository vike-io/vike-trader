// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trailing SL Strategy [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/n3vj0MIi/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-02-26, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A trailing level 4% of the close away from the price, always in one of two states. Long: the level is the highest of
//! `low - 4% of close` since the state began, and the state turns short when a low goes under the previous bar's level.
//! Short: the lowest of `high + 4% of close`, turning long when a high goes over it. A turn starts the level again from that
//! bar's extreme. Every bar a stop entry rests at the level against the state, a sell stop while long and a buy stop while
//! short, each sized at half of the account's worth and reversing the other. No costs; starts with 100,000.
//!
//! The rule starts in neither state, with its level at 0, so its first bar turns long and its first order is a sell stop.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const PERCENT: f64 = 4.0;
const SHARE: f64 = 50.0;

struct S {
    /// `1` long, `-1` short, `0` before the first bar.
    state: i32,
    level: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { state: 0, level: 0.0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let gap = bar.close * PERCENT / 100.0;
        let (state, previous) = (self.state, self.level);
        let up = state != 1 && bar.high > previous;
        let down = state != -1 && bar.low < previous;
        self.level = if down {
            bar.high + gap
        } else if up {
            bar.low - gap
        } else if state == 1 {
            (bar.low - gap).max(previous)
        } else if state == -1 {
            (bar.high + gap).min(previous)
        } else {
            previous
        };
        self.state = if up {
            1
        } else if down {
            -1
        } else {
            state
        };
        if self.state != 1 {
            let level = strategy.snap_stop(Long, self.level);
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, level)).stop(level);
        }
        if self.state != -1 {
            let level = strategy.snap_stop(Short, self.level);
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, level)).stop(level);
        }
    }
}
