// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/oca-raw-strategy-order-reduce-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 07:15 UTC when flat. While held, on every bar: a free sell limit 0.4% above and a free sell stop 0.4%
//! under the average cost, each the size held, in one group. Flat, both are pulled. Closes at the open after 13:15.
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

const NAME: &str = "long";
const TARGET: &str = "target";
const STOP: &str = "stop";
const GROUP: &str = "pair";
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
        if now == (7, 15) && held == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        if held > 0.0 {
            let cost = strategy.avg_cost();
            strategy.order(TARGET, Short).size(held).limit(cost * (1.0 + WIDTH)).group(GROUP);
            strategy.order(STOP, Short).size(held).stop(cost * (1.0 - WIDTH)).group(GROUP);
        }
        if held == 0.0 {
            strategy.cancel(TARGET);
            strategy.cancel(STOP);
        }
        if held > 0.0 && now == (13, 15) {
            strategy.close(NAME);
        }
    }
}
