// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-stop-entry-reversal-grouping-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 00:15 UTC bar, a market long of 1, and while long on the 00:30 bar a second one. While long on the 00:45 bar, a
//! sell stop at that bar's high, asked for size 1; while short on the 01:15 bar, a buy stop at that bar's low, asked for size 1.
//! Anything held on the 01:45 bar closes at the next open. At most two lots a side.
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

struct Reversals;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(Reversals)
}

impl Rule for Reversals {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let held = strategy.position();
        match clock(bar) {
            (0, 15) if held == 0.0 => {
                strategy.entry("first", Long).size(1.0);
            }
            (0, 30) if held > 0.0 => {
                strategy.entry("second", Long).size(1.0);
            }
            (0, 45) if held > 0.0 => {
                strategy.entry("turn down", Short).size(1.0).stop(bar.high);
            }
            (1, 15) if held < 0.0 => {
                strategy.entry("turn up", Long).size(1.0).stop(bar.low);
            }
            (1, 45) if held != 0.0 => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}
