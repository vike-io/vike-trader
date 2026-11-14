// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/analyzer-parity-stop-limit-timing-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 at the open after 00:00 or 12:00 UTC when flat, once ATR(14) has a value, with a stop 1 ATR below and a target 2 ATR above
//! the signal bar's close placed in the same pass, so they are live in the entry bar. No other exit.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const STOP_ATRS: f64 = 1.0;
const TARGET_ATRS: f64 = 2.0;

struct S {
    atr: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { atr: IndicatorLine::new("atr", &[14.0]) })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar)[0];
        let (hour, minute) = clock(bar);
        let window = (hour == 0 || hour == 12) && minute == 0;
        if window && strategy.position() == 0.0 && !atr.is_nan() {
            strategy.entry(NAME, Long).size(1.0);
            strategy
                .exit("bracket")
                .from(NAME)
                .take(bar.close + atr * TARGET_ATRS)
                .stop(bar.close - atr * STOP_ATRS);
        }
    }
}
