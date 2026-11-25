// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ltf-numeric-float-ratio15-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. When flat and EMA(9) crosses up through EMA(21), buy 1 at the next open, if the closes of the 1-minute bars inside this
//! bar, added up, lie further than 0.1% of this close from 15 times this close. Close when EMA(9) crosses down. No costs.
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
const MINUTES_A_BAR: f64 = 15.0;
const DRIFT_SHARE: f64 = 0.001;
/// The input the rule reads beside its bars: the symbol's 1-minute bars.
const MINUTES: &str = "1m";

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

    fn inputs(&self) -> Vec<String> {
        vec![MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let quick = self.quick.next(bar)[0];
        let steady = self.steady.next(bar)[0];
        match self.cross.step(quick, steady) {
            Signal::Long if strategy.position() == 0.0 => {
                let closes: f64 =
                    strategy.input(MINUTES).since(bar.timestamp).iter().map(|m| m.close).sum();
                if (closes - MINUTES_A_BAR * bar.close).abs() > bar.close * DRIFT_SHARE {
                    strategy.entry(LOT, Long).size(1.0);
                }
            }
            Signal::Short if strategy.position() > 0.0 => {
                strategy.close(LOT);
            }
            _ => {}
        }
    }
}
