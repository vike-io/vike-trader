// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-tuple-return-destructure-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! When flat, long 1 when EMA(9) crosses up through EMA(21) and short 1 when it crosses down, each with a stop 1.5 ATR(14) and a
//! target 3 ATR(14) from the signal bar's close, issued once in the same pass, so they are live in the entry bar. No costs.
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
const STOP_ATRS: f64 = 1.5;
const TARGET_ATRS: f64 = 3.0;
const SIZE: f64 = 1.0;

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
        let crossed = self.cross.step(fast, slow);
        if strategy.position() != 0.0 {
            return;
        }
        let (stop, target) = (atr * STOP_ATRS, atr * TARGET_ATRS);
        match crossed {
            Signal::Long => {
                strategy.entry(UP, Long).size(SIZE);
                strategy
                    .exit("long bracket")
                    .from(UP)
                    .take(bar.close + target)
                    .stop(bar.close - stop);
            }
            Signal::Short => {
                strategy.entry(DOWN, Short).size(SIZE);
                strategy
                    .exit("short bracket")
                    .from(DOWN)
                    .take(bar.close - target)
                    .stop(bar.close + stop);
            }
            _ => {}
        }
    }
}
