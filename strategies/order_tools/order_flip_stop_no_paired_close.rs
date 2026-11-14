// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-flip-stop-no-paired-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat on the 00:15 UTC bar, a market long of 1. While long on the 00:45 bar, a sell stop one tick below that bar's low,
//! asked for size 1, with nothing closing the long beside it. Anything held on the 12:00 bar closes at the next open.
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

struct FlipByStop;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(FlipByStop)
}

impl Rule for FlipByStop {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let now = clock(bar);
        let held = strategy.position();
        if now == (0, 15) && held == 0.0 {
            strategy.entry("open", Long).size(1.0);
        }
        if now == (0, 45) && held > 0.0 {
            strategy.entry("flip", Short).size(1.0).stop(bar.low - tick);
        }
        if now == (12, 0) && held != 0.0 {
            strategy.close_all();
        }
    }
}
