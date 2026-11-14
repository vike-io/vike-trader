// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-close-all-cancel-all-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On the 04:15 UTC bar, a buy limit 20% under the close and a sell limit 20% above it, size 1, every resting order pulled on the
//! 04:30 bar. While flat on the 05:15 bar, a market long of 1, and while long on the 05:45 bar another. Anything held on the 06:15
//! bar closes at that bar's own close. At most two lots a side.
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

struct Sweep;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(Sweep)
}

impl Rule for Sweep {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let held = strategy.position();
        match clock(bar) {
            (4, 15) => {
                strategy.entry("low bid", Long).size(1.0).limit(bar.close * 0.80);
                strategy.entry("high ask", Short).size(1.0).limit(bar.close * 1.20);
            }
            (4, 30) => strategy.cancel_all(),
            (5, 15) if held == 0.0 => {
                strategy.entry("first", Long).size(1.0);
            }
            (5, 45) if held > 0.0 => {
                strategy.entry("second", Long).size(1.0);
            }
            (6, 15) if held != 0.0 => {
                strategy.close_all().here();
            }
            _ => {}
        }
    }
}
