// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-opposite-entry-close-same-pass-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 00:15 UTC bar, a market long of 1. While long on the 00:45 bar, a sell stop one tick below its low, asked for
//! size 1, and then a close of that long. While flat on the 06:15 bar, a market long of 1. While long on the 06:45 bar, a close of
//! that long, and then a sell stop one tick below its low, asked for size 1. A short held on the 12:15 bar closes at the next open.
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

const NIGHT: &str = "night";
const NIGHT_STOP: &str = "night stop";
const DAY: &str = "day";
const DAY_STOP: &str = "day stop";

struct SamePass;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SamePass)
}

impl Rule for SamePass {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let under = bar.low - tick;
        let held = strategy.position();
        match clock(bar) {
            (0, 15) if held == 0.0 => {
                strategy.entry(NIGHT, Long).size(1.0);
            }
            (0, 45) if held > 0.0 => {
                strategy.entry(NIGHT_STOP, Short).size(1.0).stop(under);
                strategy.close(NIGHT);
            }
            (6, 15) if held == 0.0 => {
                strategy.entry(DAY, Long).size(1.0);
            }
            (6, 45) if held > 0.0 => {
                strategy.close(DAY);
                strategy.entry(DAY_STOP, Short).size(1.0).stop(under);
            }
            (12, 15) if held < 0.0 => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}
