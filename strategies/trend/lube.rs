// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "LUBE" by Jomy, https://www.tradingview.com/script/i9tJOtjl/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-11, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A friction count: each of the last 500 bars whose range holds this close adds `501 / (i + 500)`, `i` bars back. With
//! the lowest and highest friction of the last 100 bars, the entry level is `1.1 * lowest - 0.1 * highest` and the exit level
//! their midpoint, both read 5 bars back. Friction below the entry level goes long when a 4-3-2-1 weighted average of the
//! last four closes rises and short when it does not; friction above the exit level closes either side. Each entry is twice
//! the account's worth in BTC and reverses the other side; none before the 21st bar. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::Donchian;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BARS_BACK: usize = 500;
const LAG: usize = 5;
const LEVERAGE: f64 = 2.0;

struct S {
    /// `(high, low)` of the bars before this one, newest first.
    ranges: VecDeque<(f64, f64)>,
    friction: Donchian,
    /// `(entry level, exit level)`, newest first, `LAG + 1` of them.
    levels: VecDeque<(f64, f64)>,
    /// The last three closes, newest first.
    closes: VecDeque<f64>,
    fir: f64,
    bars: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ranges: VecDeque::with_capacity(BARS_BACK + 1),
        friction: Donchian::new(100.0).expect("a valid length"),
        levels: VecDeque::with_capacity(LAG + 1),
        closes: VecDeque::with_capacity(4),
        fir: f64::NAN,
        bars: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let mut friction = 0.0;
        for (i, &(high, low)) in self.ranges.iter().enumerate() {
            if high >= close && low <= close {
                friction += (1 + BARS_BACK) as f64 / ((i + 1 + BARS_BACK) as f64);
            }
        }
        self.ranges.push_front((bar.high, bar.low));
        self.ranges.truncate(BARS_BACK);
        let [highest, _, lowest] = self.friction.next_hl(friction, friction);
        self.levels.push_front((lowest * 1.1 + highest * -0.1, lowest * 0.5 + highest * 0.5));
        self.levels.truncate(LAG + 1);
        let (entry, exit) = self.levels.get(LAG).copied().unwrap_or((f64::NAN, f64::NAN));

        // Left: a WMA(4) from the first bar with the missing closes read as 0, where vike's `Wma` waits for four.
        let past = |i: usize| self.closes.get(i).copied().unwrap_or(0.0);
        let fir = (4.0 * close + 3.0 * past(0) + 2.0 * past(1) + past(2)) / 10.0;
        let up = fir > self.fir;
        self.fir = fir;
        self.closes.push_front(close);
        self.closes.truncate(3);
        self.bars += 1;

        let long = friction < entry && up;
        let short = friction < entry && !up;
        let end = friction > exit;
        let size = sizing.floor((strategy.equity() / close * LEVERAGE).clamp(0.000_001, 1e9));
        let started = self.bars > 20;
        if long && started {
            strategy.entry("long", Long).size(size);
        }
        if short || end {
            strategy.close("long");
        }
        if short && started {
            strategy.entry("short", Short).size(size);
        }
        if long || end {
            strategy.close("short");
        }
    }
}
