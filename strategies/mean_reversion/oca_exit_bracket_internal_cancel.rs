// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/oca-exit-bracket-internal-cancel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat and RSI(14) crosses up through 30. While held, a target and a stop 1.5 ATR(14) around the average cost, sized
//! at one unit and moved on every bar, in one named group: when either fills the other is pulled.
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
const RSI_LEVEL: f64 = 30.0;
const ATRS: f64 = 1.5;

struct S {
    rsi: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let up = matches!(self.cross.step(rsi, RSI_LEVEL), Signal::Long);
        if up && strategy.position() == 0.0 {
            strategy.entry(NAME, Long).size(1.0);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("bracket")
                .from(NAME)
                .size(1.0)
                .take(cost + atr * ATRS)
                .stop(cost - atr * ATRS)
                .group("bracket group");
        }
    }
}
