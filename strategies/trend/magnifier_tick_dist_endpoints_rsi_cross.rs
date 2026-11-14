// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/magnifier-tick-dist-endpoints-rsi-cross-08a/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. When flat and RSI(14) crosses up through 50, buy 1 at the next open, with a sell stop issued once at the mean of this bar's
//! open and high, live in the entry bar. Close when RSI(14) crosses down through 50. No costs, bar magnifier on.
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
const MIDLINE: f64 = 50.0;

struct S {
    rsi: IndicatorLine,
    through_mid: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { rsi: IndicatorLine::new("rsi", &[14.0]), through_mid: Cross::new() })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        match self.through_mid.step(rsi, MIDLINE) {
            Signal::Long if strategy.position() == 0.0 => {
                strategy.entry(LOT, Long).size(1.0);
                let level = (bar.open + bar.high) * 0.5;
                strategy.exit(STOP).from(LOT).stop(level);
            }
            Signal::Short if strategy.position() > 0.0 => {
                strategy.close(LOT);
            }
            _ => {}
        }
    }
}
