// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-cross-before-modify-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat on the 03:15 UTC bar, a buy stop at its close; still flat on the 03:30 bar, the same name moves to one tick over that
//! bar's high. A long held on the 06:15 bar closes at the next open. The mirror short at 15:15 and 15:30, one tick under the low,
//! closed from 18:15. Size 1.
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

struct CrossBeforeModify;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(CrossBeforeModify)
}

impl Rule for CrossBeforeModify {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let held = strategy.position();
        match clock(bar) {
            (3, 15) if held == 0.0 => {
                strategy.entry(BUY, Long).size(1.0).stop(bar.close);
            }
            (3, 30) if held == 0.0 => {
                strategy.entry(BUY, Long).size(1.0).stop(bar.high + tick);
            }
            (15, 15) if held == 0.0 => {
                strategy.entry(SELL, Short).size(1.0).stop(bar.close);
            }
            (15, 30) if held == 0.0 => {
                strategy.entry(SELL, Short).size(1.0).stop(bar.low - tick);
            }
            (6, 15) if held > 0.0 => {
                strategy.close(BUY);
            }
            (18, 15) if held < 0.0 => {
                strategy.close(SELL);
            }
            _ => {}
        }
    }
}
