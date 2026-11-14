// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/stats-eventrades-zero-pnl-count-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 00:00 UTC when flat, remembering that bar's close. While held, a target rests at the remembered
//! close, whatever the entry filled at. No costs.
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

struct S {
    signalled: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { signalled: f64::NAN })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if clock(bar) == (0, 0) && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
            self.signalled = bar.close;
        }
        if strategy.position() > 0.0 && !self.signalled.is_nan() {
            strategy.exit("exit").from(NAME).take(self.signalled);
        }
    }
}
