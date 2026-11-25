// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-scalping-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when EMA(5) crosses up through EMA(13), short 1 when it crosses down, either one reversing the other side, with a target 15
//! ticks and a stop 7 ticks from the average cost while held. The same rule as `tight_tp_sl_points`.
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
/// The target's and the stop's distances from the entry, in ticks of the instrument.
const TARGET_TICKS: f64 = 15.0;
const STOP_TICKS: f64 = 7.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[5.0]),
        slow: IndicatorLine::new("ema", &[13.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let (target, stop) = (TARGET_TICKS * tick, STOP_TICKS * tick);
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => {
                strategy.entry(UP, Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry(DOWN, Short).size(1.0);
            }
            _ => {}
        }
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy.exit("long bracket").from(UP).take(cost + target).stop(cost - stop);
        } else if held < 0.0 {
            strategy.exit("short bracket").from(DOWN).take(cost - target).stop(cost + stop);
        }
    }
}
