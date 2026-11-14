// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-minute-zero-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 02:00 UTC bar, a buy stop one tick above its high, which a long held at 06:15 closes at the next open. While
//! flat on the 14:00 bar, a sell stop one tick below its low, which a short held at 18:15 closes at the next open. Size 1.
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

struct MinuteZero;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(MinuteZero)
}

impl Rule for MinuteZero {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let held = strategy.position();
        match clock(bar) {
            (2, 0) if held == 0.0 => {
                strategy.entry(BUY, Long).size(1.0).stop(bar.high + tick);
            }
            (6, 15) if held > 0.0 => {
                strategy.close(BUY);
            }
            (14, 0) if held == 0.0 => {
                strategy.entry(SELL, Short).size(1.0).stop(bar.low - tick);
            }
            (18, 15) if held < 0.0 => {
                strategy.close(SELL);
            }
            _ => {}
        }
    }
}
