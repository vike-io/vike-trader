// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-modification-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Each bar of the 02:00 UTC hour while flat moves one buy stop to a tick above that bar's high, and each bar of the 14:00 hour
//! while flat moves one sell stop to a tick below its low, so only the latest level is live. Size 1. A long held on the 06:15 bar and
//! a short held on the 18:15 bar close at the next open.
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

const LONG_NAME: &str = "moving long";
const SHORT_NAME: &str = "moving short";

struct LatestLevel;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(LatestLevel)
}

impl Rule for LatestLevel {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let held = strategy.position();
        match clock(bar) {
            (2, _) if held == 0.0 => {
                strategy.entry(LONG_NAME, Long).size(1.0).stop(bar.high + tick);
            }
            (14, _) if held == 0.0 => {
                strategy.entry(SHORT_NAME, Short).size(1.0).stop(bar.low - tick);
            }
            (6, 15) if held > 0.0 => {
                strategy.close(LONG_NAME);
            }
            (18, 15) if held < 0.0 => {
                strategy.close(SHORT_NAME);
            }
            _ => {}
        }
    }
}
