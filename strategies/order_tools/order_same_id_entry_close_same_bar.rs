// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-entry-close-same-bar-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 00:15 UTC when flat; on the 00:45 bar, a second entry under the same name and a close of that name
//! in one pass. Flatten at the open after 06:15. The same again short at 12:15 and 12:45, with the flatten after 18:15.
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

const UP: &str = "long";
const DOWN: &str = "short";

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
        if now == (0, 15) && held == 0.0 {
            strategy.entry(UP, Long).size(1.0);
        }
        if now == (0, 45) && held > 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy.close(UP);
        }
        if now == (6, 15) && held != 0.0 {
            strategy.close_all();
        }
        if now == (12, 15) && held == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
        }
        if now == (12, 45) && held < 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.close(DOWN);
        }
        if now == (18, 15) && held != 0.0 {
            strategy.close_all();
        }
    }
}
