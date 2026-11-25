// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-switchback-all-in-reversal-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When EMA(31) crosses up through EMA(97), a long worth the whole account at this close, then a close of any short. When it crosses
//! down, a short the same way, then a close of any long. This port overrides the rule's header: no commission, no slippage, the
//! whole account where the rule states a tenth. Margin is 50%, so these entries are never refused.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[31.0]),
        slow: IndicatorLine::new("ema", &[97.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let size = sizing.size_for_share(1.0, bar.close);
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry(UP, Long).size(size);
                strategy.close(DOWN);
            }
            Signal::Short => {
                strategy.entry(DOWN, Short).size(size);
                strategy.close(UP);
            }
            _ => {}
        }
    }
}
