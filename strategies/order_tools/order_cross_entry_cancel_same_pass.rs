// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-cross-entry-cancel-same-pass-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Four daily stops of size 1, each issued with a cancel of its own name in one pass, in either order. While flat at 02:15 UTC a
//! buy stop one tick over the bar's high, then its cancel; at 04:15 the cancel, then the same stop. A long held at 06:15 closes at
//! the next open. The mirror short at 14:15 and 16:15 one tick under the low, closed from 18:15.
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

const BUY_THEN_PULL: &str = "buy then pull";
const PULL_THEN_BUY: &str = "pull then buy";
const SELL_THEN_PULL: &str = "sell then pull";
const PULL_THEN_SELL: &str = "pull then sell";

struct EntryAndCancel;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(EntryAndCancel)
}

impl Rule for EntryAndCancel {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let now = clock(bar);
        let held = strategy.position();
        let over = bar.high + tick;
        let under = bar.low - tick;
        if now == (2, 15) && held == 0.0 {
            strategy.entry(BUY_THEN_PULL, Long).size(1.0).stop(over);
            strategy.cancel(BUY_THEN_PULL);
        }
        if now == (4, 15) && held == 0.0 {
            strategy.cancel(PULL_THEN_BUY);
            strategy.entry(PULL_THEN_BUY, Long).size(1.0).stop(over);
        }
        if now == (6, 15) && held > 0.0 {
            strategy.close_all();
        }
        if now == (14, 15) && held == 0.0 {
            strategy.entry(SELL_THEN_PULL, Short).size(1.0).stop(under);
            strategy.cancel(SELL_THEN_PULL);
        }
        if now == (16, 15) && held == 0.0 {
            strategy.cancel(PULL_THEN_SELL);
            strategy.entry(PULL_THEN_SELL, Short).size(1.0).stop(under);
        }
        if now == (18, 15) && held < 0.0 {
            strategy.close_all();
        }
    }
}
