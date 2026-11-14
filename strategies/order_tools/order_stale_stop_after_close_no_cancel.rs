// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-stale-stop-after-close-no-cancel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 02:15 UTC bar, a buy stop one tick above its high, whose long closes at the next open if held on the 06:15
//! bar. While flat on the 14:15 bar, a sell stop one tick below its low, whose short closes at the next open if held on the 18:15
//! bar. Size 1. Nothing pulls a stop left resting.
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

const UP: &str = "up";
const DOWN: &str = "down";

struct LeftResting;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(LeftResting)
}

impl Rule for LeftResting {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let held = strategy.position();
        match clock(bar) {
            (2, 15) if held == 0.0 => {
                strategy.entry(UP, Long).size(1.0).stop(bar.high + tick);
            }
            (6, 15) if held > 0.0 => {
                strategy.close(UP);
            }
            (14, 15) if held == 0.0 => {
                strategy.entry(DOWN, Short).size(1.0).stop(bar.low - tick);
            }
            (18, 15) if held < 0.0 => {
                strategy.close(DOWN);
            }
            _ => {}
        }
    }
}
