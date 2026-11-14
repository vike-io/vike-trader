// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/oca-multi-bracket-isolation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, EMA(9) crosses up through EMA(21) and RSI(14) is under 60. While held, two independent brackets of size 1
//! around the average cost: one at 1 ATR(14), one at 2 ATR(14). A bracket that fires cancels only its own other leg, stays spent,
//! and leaves the other bracket working on what remains.
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
const RSI_BELOW: f64 = 60.0;

struct S {
    rsi: IndicatorLine,
    atr: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    })
}

/// A bracket `atrs` wide around `cost`, for one unit, in its own group.
fn bracket(strategy: &mut Tactics<'_>, name: &str, cost: f64, atr: f64, atrs: f64, group: &str) {
    strategy
        .exit(name)
        .from(NAME)
        .size(1.0)
        .take(cost + atr * atrs)
        .stop(cost - atr * atrs)
        .group(group);
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let up = matches!(self.cross.step(fast, slow), Signal::Long);
        if up && rsi < RSI_BELOW && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(2.0);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            bracket(strategy, "tight", cost, atr, 1.0, "tight group");
            bracket(strategy, "wide", cost, atr, 2.0, "wide group");
        }
    }
}
