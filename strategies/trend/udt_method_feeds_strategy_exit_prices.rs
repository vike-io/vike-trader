// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-feeds-strategy-exit-prices-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat and EMA(9) crosses up through EMA(21), with a stop 1.5 ATR(14) and a target 3 ATR(14) from the signal bar's
//! close, issued once in the same pass, so they are live in the entry bar and never move. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const STOP_ATRS: f64 = 1.5;
const TARGET_ATRS: f64 = 3.0;

struct S {
    atr: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[14.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.atr.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let up = matches!(self.cross.step(fast, slow), Signal::Long);
        if up && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
            strategy
                .exit("bracket")
                .from(NAME)
                .take(bar.close + atr * TARGET_ATRS)
                .stop(bar.close - atr * STOP_ATRS);
        }
    }
}
