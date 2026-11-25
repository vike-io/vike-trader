// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-extra-primitive-args-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat and the close crosses up through SMA(20), with a target and a stop 5 steps of 0.10 from the signal bar's close
//! placed in the same pass, so they are live in the entry bar. No costs.
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
const STEPS: f64 = 5.0;
const STEP: f64 = 0.10;

struct S {
    average: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { average: IndicatorLine::new("sma", &[20.0]), cross: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let average = self.average.next(bar)[0];
        let up = matches!(self.cross.step(bar.close, average), Signal::Long);
        if up && strategy.position() == 0.0 {
            let reach = STEP * STEPS;
            strategy.entry(NAME, Long).size(1.0);
            strategy.exit("bracket").from(NAME).take(bar.close + reach).stop(bar.close - reach);
        }
    }
}
