// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Triple EMA Scalper" by SoftKill21, https://www.tradingview.com/script/xK5UA23y/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-09-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! SMAs of 25, 50 and 100 closes. Long when the fastest crosses up through both others on one bar, short when it crosses down
//! through both, on a bar opening from 03:00 to 16:00 UTC, Monday to Friday (the London and New York sessions together; a
//! session with no days stated is the weekdays in this language version). The size is a whole thousand units: 1% of the
//! capital plus the closed profit, risked over 100 ticks, in hundred-thousands, at least 1000.
//! Each entry carries a target 50 ticks and a stop 100 ticks from its fill. At most 2 fills a UTC day; the fill that reaches it
//! ends that day's trading. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! fills_a_day = 2
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::helpers::weekday;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const TARGET_TICKS: f64 = 50.0;
const STOP_TICKS: f64 = 100.0;

struct S {
    fast: Sma,
    middle: Sma,
    slow: Sma,
    over_middle: Cross,
    over_slow: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        fast: sma(25.0),
        middle: sma(50.0),
        slow: sma(100.0),
        over_middle: Cross::new(),
        over_slow: Cross::new(),
    })
}

/// The rule's size from its closed profit: `balance * 1% / 100 ticks * 100,000`, floored to a thousand, at least 1000.
fn size(banked: f64) -> f64 {
    let lots = (100_000.0 + banked) * 0.01 / STOP_TICKS * 100_000.0;
    let size = lots - lots % 1000.0;
    if size < 1000.0 { 1000.0 } else { size }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar);
        let middle = self.middle.next(bar);
        let slow = self.slow.next(bar);
        let a = self.over_middle.step(fast, middle);
        let b = self.over_slow.step(fast, slow);
        let (hour, _) = clock(bar);
        // A session given with no days is Monday to Friday in this rule's language version.
        if !(3..16).contains(&hour) || weekday(bar.timestamp) > 4 {
            return;
        }
        let banked = strategy.closed_net().unwrap_or(0.0);
        match (a, b) {
            (Signal::Long, Signal::Long) => {
                strategy.entry("long", Long).size(size(banked));
            }
            (Signal::Short, Signal::Short) => {
                strategy.entry("short", Short).size(size(banked));
            }
            _ => {}
        }
    }

    /// The exits of an entry, from its fill price.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let Some(lot) = strategy.lots().first() else { return };
        let (name, side, price) = (lot.name.clone(), f64::from(lot.side), lot.price);
        strategy
            .exit(&format!("tp/sl {name}"))
            .from(&name)
            .take(price + side * TARGET_TICKS * tick)
            .stop(price - side * STOP_TICKS * tick);
    }
}
