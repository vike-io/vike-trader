// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-parity-edge-margin-50-pct-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Each Monday's 00:00 UTC bar, when flat, long half the account's worth at the close, rounded to 3 decimals. Closed on the
//! first bar after the one the entry filled on. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_model::time::utc_weekday;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const THOUSANDTHS_OF_WORTH_PER_PRICE: f64 = 500.0;

struct S;

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S)
}

fn is_monday(bar: &Bar) -> bool {
    utc_weekday(bar.timestamp) == 0
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if is_monday(bar) && clock(bar) == (0, 0) && strategy.position() == 0.0 {
            let size =
                (strategy.equity() * THOUSANDTHS_OF_WORTH_PER_PRICE / bar.close).round() / 1000.0;
            strategy.entry(NAME, Long).size(size);
        }
        if strategy.lots().first().is_some_and(|lot| bar.timestamp > lot.entry_timestamp) {
            strategy.close(NAME);
        }
    }
}
