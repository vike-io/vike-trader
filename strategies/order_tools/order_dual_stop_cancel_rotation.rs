// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-stop-cancel-rotation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Before 12:00 UTC every bar places a buy stop one tick above its high and pulls the sell stop; from 12:00 on every bar places a
//! sell stop one tick below its low and pulls the buy stop. Size 1. A position on the wrong side of the day's half closes at the
//! next open, beside the stop that pass placed, which keeps the size the position gave it.
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

const MORNING: &str = "morning";
const AFTERNOON: &str = "afternoon";

struct Rotation;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(Rotation)
}

impl Rule for Rotation {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let morning = clock(bar).0 < 12;
        if morning {
            strategy.entry(MORNING, Long).size(1.0).stop(bar.high + tick);
            strategy.cancel(AFTERNOON);
        } else {
            strategy.entry(AFTERNOON, Short).size(1.0).stop(bar.low - tick);
            strategy.cancel(MORNING);
        }
        let held = strategy.position();
        if held > 0.0 && !morning {
            strategy.close(MORNING);
        }
        if held < 0.0 && morning {
            strategy.close(AFTERNOON);
        }
    }
}
