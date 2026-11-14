// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-four-bar-stop-no-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On each bar of the 02:00 UTC hour, while flat, one buy stop moves to one tick above that bar's high; on each bar of the 14:00
//! hour, while flat, one sell stop moves to one tick below that bar's low. Size 1, and each rests until it fills or moves. Anything
//! held on the 23:45 bar closes at the next open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct TwoWindows;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(TwoWindows)
}

impl Rule for TwoWindows {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let flat = strategy.position() == 0.0;
        if hour == 2 && minute <= 45 && flat {
            strategy.entry("buy", Long).size(1.0).stop(bar.high + tick);
        }
        if hour == 14 && minute <= 45 && flat {
            strategy.entry("sell", Short).size(1.0).stop(bar.low - tick);
        }
        if (hour, minute) == (23, 45) && !flat {
            strategy.close_all();
        }
    }
}
