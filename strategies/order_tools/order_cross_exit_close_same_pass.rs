// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-cross-exit-close-same-pass-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 at the open after 00:15 UTC and again at the open after 06:15, each under its own name. On 00:45 and 06:45,
//! if held, a bracket 2% around the average cost and a close of that lot are issued in one pass, the bracket first in the morning
//! and the close first after that. The close goes first at the next open either way, and the bracket dies with its lot.
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

const FIRST: &str = "first";
const SECOND: &str = "second";
const WIDTH: f64 = 0.02;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

fn around(cost: f64) -> (f64, f64) {
    (cost * (1.0 + WIDTH), cost * (1.0 - WIDTH))
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let held = strategy.position();
        if now == (0, 15) && held == 0.0 {
            strategy.entry(FIRST, Long).size(1.0);
        }
        if now == (0, 45) && held > 0.0 {
            let (take, cut) = around(strategy.avg_cost());
            strategy.exit("first bracket").from(FIRST).take(take).stop(cut);
            strategy.close(FIRST);
        }
        if now == (6, 15) && held == 0.0 {
            strategy.entry(SECOND, Long).size(1.0);
        }
        if now == (6, 45) && held > 0.0 {
            strategy.close(SECOND);
            let (take, cut) = around(strategy.avg_cost());
            strategy.exit("second bracket").from(SECOND).take(take).stop(cut);
        }
    }
}
