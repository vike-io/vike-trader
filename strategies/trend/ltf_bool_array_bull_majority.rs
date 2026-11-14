// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ltf-bool-array-bull-majority-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. Count the 1-minute bars inside this bar that closed over their open. When flat with 9 or more and EMA(9) over EMA(21), buy 1
//! at the next open. When long with 6 or fewer, or with EMA(9) under EMA(21), close. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LOT: &str = "long";
const ENOUGH_UP: usize = 9;
const TOO_FEW_UP: usize = 6;
/// The input the rule reads beside its bars: the symbol's 1-minute bars.
const MINUTES: &str = "1m";

struct S {
    quick: IndicatorLine,
    steady: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        quick: IndicatorLine::new("ema", &[9.0]),
        steady: IndicatorLine::new("ema", &[21.0]),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let quick = self.quick.next(bar)[0];
        let steady = self.steady.next(bar)[0];
        let minutes = strategy.input(MINUTES).since(bar.timestamp);
        let up_minutes = minutes.iter().filter(|m| m.close > m.open).count();
        if up_minutes >= ENOUGH_UP && quick > steady && strategy.position() == 0.0 {
            strategy.entry(LOT, Long).size(1.0);
        }
        if (up_minutes <= TOO_FEW_UP || quick < steady) && strategy.position() > 0.0 {
            strategy.close(LOT);
        }
    }
}
