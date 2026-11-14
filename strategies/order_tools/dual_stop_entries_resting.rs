// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-stop-both-touch-priority-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat on the 02:45 UTC bar, a buy stop 0.2% above and a sell stop 0.2% below the close, and on the 14:45 bar another pair
//! at 0.4%. Each rests, size 1, until it fills or its own name is placed again. Flatten at the opens after 06:15 and 18:15.
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

const NEAR: f64 = 0.002;
const FAR: f64 = 0.004;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let held = strategy.position();
        if now == (2, 45) && held == 0.0 {
            strategy.entry("near long", Long).size(1.0).stop(bar.close * (1.0 + NEAR));
            strategy.entry("near short", Short).size(1.0).stop(bar.close * (1.0 - NEAR));
        }
        if held != 0.0 && now == (6, 15) {
            strategy.close_all();
        }
        if now == (14, 45) && held == 0.0 {
            strategy.entry("far long", Long).size(1.0).stop(bar.close * (1.0 + FAR));
            strategy.entry("far short", Short).size(1.0).stop(bar.close * (1.0 - FAR));
        }
        if held != 0.0 && now == (18, 15) {
            strategy.close_all();
        }
    }
}
