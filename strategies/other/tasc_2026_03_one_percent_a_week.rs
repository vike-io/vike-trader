// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TASC 2026.03 One Percent A Week" by PineCodersTASC, https://www.tradingview.com/script/nVECqIQx/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-02-18, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! On each Monday's first bar (00:00 UTC), a long worth a tenth of the account rests as a limit 1% under that bar's open,
//! replacing last week's if it never filled. Once filled, a target 1% over the entry is set on the next first bar of a day (or on
//! the fill's own bar if it is one); a bar whose low goes 0.5% under the entry moves the target to the entry itself, once per
//! fill, and from then on the 1% target is not set. Whatever is held at Friday's last bar (23:30 UTC) is closed at its close.
//! No costs; starts with 1,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::helpers::weekday;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Buy";
const SHARE: f64 = 10.0;
const DIP: f64 = 0.99;
const TARGET: f64 = 1.01;
const DRAWDOWN: f64 = 0.995;
const MONDAY: i64 = 0;
const FRIDAY: i64 = 4;

struct S {
    previous_held: f64,
    send_target: bool,
    break_even_sent: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { previous_held: 0.0, send_target: false, break_even_sent: false })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let first_bar = clock(bar) == (0, 0);
        if weekday(bar.timestamp) == MONDAY && first_bar {
            let level = strategy.snap_limit(Long, bar.open * DIP);
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, SHARE, level)).limit(level);
        }

        let held = strategy.position();
        let filled = held != self.previous_held;
        self.previous_held = held;
        if held > 0.0 && filled {
            self.send_target = true;
            self.break_even_sent = false;
        }
        let cost = strategy.avg_cost();
        if self.send_target && first_bar && !self.break_even_sent {
            strategy.exit("profit").from(NAME).take(cost * TARGET);
            self.send_target = false;
        }
        if bar.low < cost * DRAWDOWN && !self.break_even_sent {
            strategy.exit("profit").from(NAME).take(cost);
            self.break_even_sent = true;
        }

        if weekday(bar.timestamp) == FRIDAY && held > 0.0 && clock(bar) == (23, 30) {
            strategy.close_all().here();
        }
    }
}
