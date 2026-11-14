// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-stop-entry-touch-boundary-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 02:15 UTC bar, a buy stop one tick above the previous bar's high, which pulls any resting sell stop. While
//! flat on the 14:15 bar, a sell stop one tick below the previous bar's low, which pulls any resting buy stop. Size 1. A long held
//! on the 06:15 bar closes at the next open, and so does a short held on the 18:15 bar.
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

const BUY: &str = "buy";
const SELL: &str = "sell";

struct TouchBoundary {
    previous_high: f64,
    previous_low: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(TouchBoundary { previous_high: f64::NAN, previous_low: f64::NAN })
}

impl Rule for TouchBoundary {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let now = clock(bar);
        let held = strategy.position();
        if now == (2, 15) && held == 0.0 {
            strategy.entry(BUY, Long).size(1.0).stop(self.previous_high + tick);
            strategy.cancel(SELL);
        }
        if now == (14, 15) && held == 0.0 {
            strategy.entry(SELL, Short).size(1.0).stop(self.previous_low - tick);
            strategy.cancel(BUY);
        }
        if now == (6, 15) && held > 0.0 {
            strategy.close(BUY);
        }
        if now == (18, 15) && held < 0.0 {
            strategy.close(SELL);
        }
        self.previous_high = bar.high;
        self.previous_low = bar.low;
    }
}
