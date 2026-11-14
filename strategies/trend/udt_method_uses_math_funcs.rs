// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-uses-math-funcs-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat and EMA(9) crosses up through EMA(21), with a stop and a target placed in the same pass, so they are live in the
//! entry bar. The stop lies 1.5 ATR(14) under the signal bar's close and the target 5 times the root of ATR(14) over it, each
//! distance held between 1 and 50 and the level rounded to a whole number. No costs.
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
const LEAST: f64 = 1.0;
const MOST: f64 = 50.0;
const STOP_ATRS: f64 = 1.5;
const TARGET_ROOTS: f64 = 5.0;

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

#[expect(
    clippy::manual_clamp,
    reason = "min then max holds a NaN distance at MOST; clamp would return NaN"
)]
fn held_in_range(distance: f64) -> f64 {
    distance.min(MOST).max(LEAST)
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
            let below = held_in_range(atr.abs() * STOP_ATRS);
            let above = held_in_range(atr.abs().sqrt() * TARGET_ROOTS);
            strategy.entry(NAME, Long).size(1.0);
            strategy
                .exit("bracket")
                .from(NAME)
                .take((bar.close + above).round())
                .stop((bar.close - below).round());
        }
    }
}
