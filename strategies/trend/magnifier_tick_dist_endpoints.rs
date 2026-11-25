// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/magnifier-tick-dist-endpoints-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. When flat and EMA(9) crosses up through EMA(21), buy 1 at the next open, with a sell stop issued once, a half of the way
//! from this bar's open up to its high: it is live in the entry bar and, when that level is over the open, leaves at the open. Close
//! when EMA(9) crosses down. No costs, bar magnifier on.
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

const LOT: &str = "long";
const STOP: &str = "halfway stop";

struct S {
    quick: IndicatorLine,
    steady: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        quick: IndicatorLine::new("ema", &[9.0]),
        steady: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let quick = self.quick.next(bar)[0];
        let steady = self.steady.next(bar)[0];
        match self.cross.step(quick, steady) {
            Signal::Long if strategy.position() == 0.0 => {
                strategy.entry(LOT, Long).size(1.0);
                let level = bar.open + (bar.high - bar.open) * 0.5;
                strategy.exit(STOP).from(LOT).stop(level);
            }
            Signal::Short if strategy.position() > 0.0 => {
                strategy.close(LOT);
            }
            _ => {}
        }
    }
}
