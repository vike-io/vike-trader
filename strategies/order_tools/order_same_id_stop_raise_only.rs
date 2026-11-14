// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-raise-only-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On the 02:15, 02:30 and 02:45 UTC bars while flat, one buy stop moves to a tick above that bar's high; on the 14:15, 14:30 and
//! 14:45 bars while flat, one sell stop moves to a tick below its low. The minute-zero bar of each hour places nothing. Size 1. A
//! long held on the 06:15 bar and a short held on the 18:15 bar close at the next open.
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

const RAISED: &str = "raised";
const LOWERED: &str = "lowered";

struct RaiseOnly;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(RaiseOnly)
}

impl Rule for RaiseOnly {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let held = strategy.position();
        let in_window = matches!(minute, 15 | 30 | 45) && held == 0.0;
        if hour == 2 && in_window {
            strategy.entry(RAISED, Long).size(1.0).stop(bar.high + tick);
        }
        if hour == 14 && in_window {
            strategy.entry(LOWERED, Short).size(1.0).stop(bar.low - tick);
        }
        if held > 0.0 && (hour, minute) == (6, 15) {
            strategy.close(RAISED);
        }
        if held < 0.0 && (hour, minute) == (18, 15) {
            strategy.close(LOWERED);
        }
    }
}
