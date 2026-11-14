// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-stop-cancel-no-regime-close-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While EMA(8) of the close is above EMA(21), every bar places a buy stop at the previous bar's high and pulls the sell stop;
//! otherwise every bar places a sell stop at the previous bar's low and pulls the buy stop. Size 1. Nothing closes a position but
//! the other side's stop.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "up";
const DOWN: &str = "down";

struct Regime {
    fast: Ema,
    slow: Ema,
    last_high: f64,
    last_low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(Regime {
        fast: Ema::new(8.0).expect("a valid period"),
        slow: Ema::new(21.0).expect("a valid period"),
        last_high: f64::NAN,
        last_low: f64::NAN,
    })
}

impl Rule for Regime {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);
        if fast > slow {
            strategy.entry(UP, Long).size(1.0).stop(self.last_high);
            strategy.cancel(DOWN);
        } else {
            strategy.entry(DOWN, Short).size(1.0).stop(self.last_low);
            strategy.cancel(UP);
        }
        self.last_high = bar.high;
        self.last_low = bar.low;
    }
}
