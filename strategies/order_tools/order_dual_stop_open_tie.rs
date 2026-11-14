// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-dual-stop-open-tie-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat on the 04:45 UTC bar, a buy stop and then a sell stop, both at that bar's close; when flat on the 16:45 bar, a sell
//! stop and then a buy stop at its close. Size 1, each resting until it fills or its name is placed again. Anything held on the
//! 08:15 or 20:15 bar closes at the next open.
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

struct OpenTie;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(OpenTie)
}

impl Rule for OpenTie {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let now = clock(bar);
        let flat = strategy.position() == 0.0;
        if now == (4, 45) && flat {
            strategy.entry("morning buy", Long).size(1.0).stop(bar.close);
            strategy.entry("morning sell", Short).size(1.0).stop(bar.close);
        }
        if now == (16, 45) && flat {
            strategy.entry("evening sell", Short).size(1.0).stop(bar.close);
            strategy.entry("evening buy", Long).size(1.0).stop(bar.close);
        }
        if (now == (8, 15) || now == (20, 15)) && !flat {
            strategy.close_all();
        }
    }
}
