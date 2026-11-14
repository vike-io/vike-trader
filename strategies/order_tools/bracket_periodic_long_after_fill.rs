// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-exit-tp-sl-fixed-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 00:15 UTC when flat. While held, a target 0.4% above and a stop 0.4% below the average cost, so they
//! are live from the bar after the entry bar. Closed at the open after 06:15.
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
const WIDTH: f64 = 0.004;

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
            strategy.entry(NAME, Long).size(1.0);
        }
        if held > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("bracket")
                .from(NAME)
                .take(cost * (1.0 + WIDTH))
                .stop(cost * (1.0 - WIDTH));
        }
        if held > 0.0 && now == (6, 15) {
            strategy.close(NAME);
        }
    }
}
