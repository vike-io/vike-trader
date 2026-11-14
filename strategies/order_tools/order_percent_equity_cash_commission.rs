// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-percent-equity-cash-commission-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! On the 08:15 UTC bar, when flat, long a quarter of the account's worth at the next open. Closed at the open after the 12:15
//! bar. 1.5 commission per order.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.25;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::PerOrder(1.5), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let now = clock(bar);
        let held = strategy.position();
        if now == (8, 15) && held == 0.0 {
            let size = sizing.size_for_share(SHARE, bar.close);
            strategy.entry(NAME, Long).size(size);
        }
        if now == (12, 15) && held > 0.0 {
            strategy.close(NAME);
        }
    }
}
