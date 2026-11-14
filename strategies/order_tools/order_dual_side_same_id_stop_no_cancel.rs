// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-side-same-id-stop-no-cancel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On every bar of the 02:00 UTC hour while flat, a buy stop one tick above that bar's high, moved under one name. On every bar of
//! the 14:00 hour while flat, a sell stop one tick below its low, also under one name. Size 1, and neither pulls the other. A long
//! held on the 06:15 bar and a short held on the 18:15 bar close at the next open.
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

struct BothSides;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(BothSides)
}

impl Rule for BothSides {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let held = strategy.position();
        if hour == 2 && held == 0.0 {
            strategy.entry(BUY, Long).size(1.0).stop(bar.high + tick);
        }
        if hour == 14 && held == 0.0 {
            strategy.entry(SELL, Short).size(1.0).stop(bar.low - tick);
        }
        if held > 0.0 && (hour, minute) == (6, 15) {
            strategy.close(BUY);
        }
        if held < 0.0 && (hour, minute) == (18, 15) {
            strategy.close(SELL);
        }
    }
}
