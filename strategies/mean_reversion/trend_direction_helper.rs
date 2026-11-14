// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Trend Direction Helper" by Daveatt, https://www.tradingview.com/script/dzpmnane/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-01-07, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A turn of the bar's own stochastic, `v = 100 - |100 * (close - high) / (high - low)|`, the close's place in its own range.
//! A bar marks up when `v` is above 70 and the latest of the last 50 bars on which `v` was outside 10 to 70 had it under 10,
//! and marks down on the mirror (under 10, the latest outside one above 70). Long 10 BTC on the bar after an up mark, short
//! 10 BTC on the bar after a down mark, each reversing the other; the long is closed when SMA(7) of the close crosses down
//! through SMA(20), the short when it crosses up. 0.2% commission; starts with 100,000 in a USD account.
//!
//! The rule takes the range over `abs(10 / 180) + 1` bars, which is one bar: the integer division is 0. Its zigzag, pivots,
//! support and resistance levels and their drawings decide nothing.
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
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const SIZE: f64 = 10.0;
const LOOKBACK: usize = 50;
const LOW: f64 = 10.0;
const HIGH: f64 = 70.0;

struct S {
    fast: Sma,
    slow: Sma,
    cross: Cross,
    /// `v` on the bars before this one, newest first.
    values: VecDeque<f64>,
    /// The bar before marked up, or down.
    marked: (bool, bool),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(7.0).expect("a valid length"),
        slow: Sma::new(20.0).expect("a valid length"),
        cross: Cross::new(),
        values: VecDeque::with_capacity(LOOKBACK + 1),
        marked: (false, false),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.2), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let v = 100.0 - (100.0 * (bar.close - bar.high) / (bar.high - bar.low)).abs();
        #[expect(
            clippy::manual_range_contains,
            reason = "a NaN `v` (a bar with no range) is not outside"
        )]
        let outside = self.values.iter().find(|&&x| x < LOW || x > HIGH).copied();
        // With none outside, the rule reads the bar before, which is then not outside either.
        let latest = outside.unwrap_or(f64::NAN);
        let up = v > HIGH && latest < LOW;
        let down = v < LOW && latest > HIGH;
        self.values.push_front(v);
        self.values.truncate(LOOKBACK);
        let (was_up, was_down) = std::mem::replace(&mut self.marked, (up, down));
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);
        let cross = self.cross.step(fast, slow);
        if was_up {
            strategy.entry("long", Long).size(SIZE);
        }
        if matches!(cross, Signal::Short) {
            strategy.close("long");
        }
        if was_down {
            strategy.entry("short", Short).size(SIZE);
        }
        if matches!(cross, Signal::Long) {
            strategy.close("short");
        }
    }
}
