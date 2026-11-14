// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-entry-exit-same-pass-attach-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 08:15 UTC and short 1 at the open after 16:15, each with a target and a stop 0.5% from the
//! signal bar's close placed in the same pass, so they are live in the entry bar. Flatten at the open after 23:45.
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
const WIDTH: f64 = 0.005;

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
        let (above, below) = (bar.close * (1.0 + WIDTH), bar.close * (1.0 - WIDTH));
        if now == (8, 15) && held == 0.0 {
            strategy.entry(UP, Long).size(1.0);
            strategy.exit("long bracket").from(UP).take(above).stop(below);
        }
        if now == (16, 15) && held == 0.0 {
            strategy.entry(DOWN, Short).size(1.0);
            strategy.exit("short bracket").from(DOWN).take(below).stop(above);
        }
        if held != 0.0 && now == (23, 45) {
            strategy.close_all();
        }
    }
}
