// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-stop-open-high-first-path-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat on a 02:45 UTC bar whose open sits strictly nearer its high than its low, a buy stop 0.2% above and a sell stop 0.2%
//! below the close, size 1. Each rests until it fills or its own name is placed again. Anything held at 06:15 closes at the next
//! open.
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

struct OpenByTheHigh;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(OpenByTheHigh)
}

impl Rule for OpenByTheHigh {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let high_is_nearer = (bar.high - bar.open).abs() < (bar.open - bar.low).abs();
        if now == (2, 45) && strategy.position() == 0.0 && high_is_nearer {
            strategy.entry("buy", Long).size(1.0).stop(bar.close * 1.002);
            strategy.entry("sell", Short).size(1.0).stop(bar.close * 0.998);
        }
        if now == (6, 15) && strategy.position() != 0.0 {
            strategy.close_all();
        }
    }
}
