// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-same-id-exit-replace-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 08:15 UTC when flat. While held, a bracket 1% around the average cost is issued and at once
//! replaced under the same name by one 0.3% around it, so only the second can fire. Closed at the open after 16:15.
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

const NAME: &str = "long";
const WIDE: f64 = 0.01;
const NARROW: f64 = 0.003;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

/// `(take, cut)`.
fn around(cost: f64, width: f64) -> (f64, f64) {
    (cost * (1.0 + width), cost * (1.0 - width))
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        if now == (8, 15) && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            let (take, cut) = around(cost, WIDE);
            strategy.exit("bracket").from(NAME).take(take).stop(cut);
            let (take, cut) = around(cost, NARROW);
            strategy.exit("bracket").from(NAME).take(take).stop(cut);
        }
        if strategy.position() > 0.0 && now == (16, 15) {
            strategy.close(NAME);
        }
    }
}
