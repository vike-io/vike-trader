// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-one-side-four-bar-far-opposite-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 02:00 UTC bar, a sell stop rests at half that bar's low, size 1, which price should never reach. On each of
//! the four bars of the 02:00 hour, while flat, one buy stop moves to one tick above that bar's high, size 1. A long still held at
//! 06:15 closes at the next open.
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

const BREAKOUT: &str = "breakout";

struct FarOppositeStop;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(FarOppositeStop)
}

impl Rule for FarOppositeStop {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let flat = strategy.position() == 0.0;
        if (hour, minute) == (2, 0) && flat {
            strategy.entry("far", Short).size(1.0).stop(bar.low * 0.5);
        }
        if hour == 2 && minute <= 45 && flat {
            strategy.entry(BREAKOUT, Long).size(1.0).stop(bar.high + tick);
        }
        if (hour, minute) == (6, 15) && strategy.position() > 0.0 {
            strategy.close(BREAKOUT);
        }
    }
}
