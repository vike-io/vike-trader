// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-close-immediate-vs-next-bar-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Two long 1 round trips a day when flat. One enters at the open after 02:15 UTC and leaves at the open after 03:15. The other
//! enters at the open after 10:15 and leaves at the close of the 11:15 bar itself.
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

const LATER: &str = "leaves at the next open";
const NOW: &str = "leaves at this close";

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
        if now == (2, 15) && held == 0.0 {
            strategy.entry(LATER, Long).size(1.0);
        }
        if now == (3, 15) && held > 0.0 {
            strategy.close(LATER);
        }
        if now == (10, 15) && held == 0.0 {
            strategy.entry(NOW, Long).size(1.0);
        }
        if now == (11, 15) && held > 0.0 {
            strategy.close(NOW).here();
        }
    }
}
