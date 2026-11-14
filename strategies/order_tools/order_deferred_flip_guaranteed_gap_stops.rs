// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-deferred-flip-guaranteed-gap-stops-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Two daily flips by a stop set so far through the market that it fires at the next open. On the 08:00 UTC bar a sell stop at ten
//! times the high, pulling the buy stop, and a close of any long. On the 14:00 bar a buy stop at a tenth of the low, pulling the
//! sell stop, and a close of any short. Size 1, plus whatever opposite position is held when placed. Anything held on the Monday
//! 00:00 bar closes.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BUY: &str = "buy";
const SELL: &str = "sell";
/// Counted from Sunday as 0; day 0 of the epoch was a Thursday.
const MONDAY: i64 = 1;
const THURSDAY: i64 = 4;

struct DeferredFlip;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(DeferredFlip)
}

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
