// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-window-four-bars-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On each of the four bars of the 02:00 UTC hour, while flat, the same buy stop moves to one tick above that bar's high, size 1.
//! It rests until it fills or moves again. A long still held at 06:15 closes at the next open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ENTRY: &str = "breakout";

struct MovingStop;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(MovingStop)
}

impl Rule for MovingStop {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        if hour == 2 && minute <= 45 && strategy.position() == 0.0 {
            strategy.entry(ENTRY, Long).size(1.0).stop(bar.high + tick);
        }
        if (hour, minute) == (6, 15) && strategy.position() > 0.0 {
            strategy.close(ENTRY);
        }
    }
}
