// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-same-id-stop-after-flat-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On every bar of the 02:00 and the 08:00 UTC hours while flat, a buy stop one tick above that bar's high, always under one name,
//! so the second window rebuilds it once the first long has closed. A long held on the 06:15 or 12:15 bar closes at the next open.
//! Size 1.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "rebuilt";

struct AfterFlat;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(AfterFlat)
}

impl Rule for AfterFlat {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (hour, minute) = clock(bar);
        let held = strategy.position();
        if (hour == 2 || hour == 8) && held == 0.0 {
            strategy.entry(NAME, Long).size(1.0).stop(bar.high + tick);
        }
        if ((hour, minute) == (6, 15) || (hour, minute) == (12, 15)) && held > 0.0 {
            strategy.close(NAME);
        }
    }
}
