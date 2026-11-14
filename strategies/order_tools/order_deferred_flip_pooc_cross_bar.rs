// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-deferred-flip-pooc-cross-bar-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! The same two daily flips and Monday close as `order_deferred_flip_guaranteed_gap_stops`, with every order filling at the close of
//! the bar that placed it (a run that fills at the close: the rule is the same).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Long;
use vike_model::OrderSide::Short;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::RuleDefaults;
use vike_tactics::Tactics;
use vike_tactics::calendar::clock;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(DeferredFlip)
}

const BUY: &str = "buy";
const SELL: &str = "sell";
/// Counted from Sunday as 0; day 0 of the epoch was a Thursday.
const MONDAY: i64 = 1;
const THURSDAY: i64 = 4;

struct DeferredFlip;

impl Rule for DeferredFlip {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let held = strategy.position();
        if now == (8, 0) {
            strategy.entry(SELL, Short).size(1.0).stop(bar.high * 10.0);
            strategy.cancel(BUY);
            if held > 0.0 {
                strategy.close(BUY);
            }
        }
        if now == (14, 0) {
            strategy.entry(BUY, Long).size(1.0).stop(bar.low * 0.1);
            strategy.cancel(SELL);
            if held < 0.0 {
                strategy.close(SELL);
            }
        }
        let weekday = (bar.timestamp.div_euclid(MS_PER_DAY) + THURSDAY).rem_euclid(7);
        if weekday == MONDAY && now == (0, 0) && held != 0.0 {
            strategy.close_all();
        }
    }
}
