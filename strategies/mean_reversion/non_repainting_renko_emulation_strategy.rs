// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by PineIndicators, https://www.tradingview.com/script/cwGp0ul5/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-03-03, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Renko bricks of 3.0 built from the closes: the level starts at the first close and moves by whole bricks toward the close
//! whenever it is a brick or more away, the brick's direction the move's. On a bar that lays a brick, with every "then" read
//! 3, 6 and 9 bars back: long all of the account's worth when not long, if the direction turned up from 3 bars ago and the
//! level now is under its value then, which is under its value 6 bars ago, which is over its value 9 bars ago; short the
//! mirror when not short. The same bar closes a short on a turn up and a long on a turn down. 0.01% commission, 2 ticks of
//! slippage; starts with 10,000.
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

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BRICK: f64 = 3.0;
/// The rule reads `[3]`, `[6]` and `[9]`: a brick's size used as a bar count.
const BACK: usize = 3;

struct S {
    level: f64,
    dir: i32,
    /// `(level, dir)` of this bar and the 9 before it, newest first; NaN and 0 before the first bar.
    seen: VecDeque<(f64, i32)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { level: f64::NAN, dir: 0, seen: VecDeque::with_capacity(3 * BACK + 1) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        if self.level.is_nan() {
            self.level = bar.close;
        }
        let diff = bar.close - self.level;
        let laid = diff.abs() >= BRICK;
        if laid {
            let bricks = (diff.abs() / BRICK).floor();
            self.level += bricks * BRICK * diff.signum();
            self.dir = diff.signum() as i32;
        }
        self.seen.push_front((self.level, self.dir));
        self.seen.truncate(3 * BACK + 1);
        let back = |n: usize| self.seen.get(n * BACK).copied();
        let (Some((l3, d3)), Some((l6, _)), Some((l9, _))) = (back(1), back(2), back(3)) else {
            return;
        };
        if !laid {
            return;
        }
        let (level, dir) = (self.level, self.dir);
        let held = strategy.position();
        if d3 < dir && l3 < l6 && level < l3 && l6 < l9 && held <= 0.0 {
            strategy.entry("Long", Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        } else if d3 > dir && l3 > l6 && level > l3 && l6 > l9 && held >= 0.0 {
            strategy.entry("Short", Short).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Short, bar.close),
            ));
        }
        if d3 < dir {
            strategy.close("Short");
        } else if d3 > dir {
            strategy.close("Long");
        }
    }
}
