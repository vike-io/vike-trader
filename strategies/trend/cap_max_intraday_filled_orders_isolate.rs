// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/cap-max-intraday-filled-orders-isolate-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when SMA(5) crosses up through SMA(13) and not already long, short 1 when it crosses down and not already short. The fifth
//! fill of a UTC day closes what is held at the next price and ends trading until the day is over.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! fills_a_day = 5
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("sma", &[5.0]),
        slow: IndicatorLine::new("sma", &[13.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let held = strategy.position();
        match self.cross.step(fast, slow) {
            Signal::Long if held <= 0.0 => {
                strategy.entry(UP, Long).size(1.0);
            }
            Signal::Short if held >= 0.0 => {
                strategy.entry(DOWN, Short).size(1.0);
            }
            _ => {}
        }
    }
}
