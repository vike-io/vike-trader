// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-cascade-tuple-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when the 15-minute close crosses up through a line 0.35 ATR(15) over EMA(27), both of closed 4-hour bars in UTC. While held,
//! closed when the close is under that EMA, and a stop 2 of those ATRs under the average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const LINE_ATRS: f64 = 0.35;
const STOP_ATRS: f64 = 2.0;

struct S {
    four_hours: HigherTimeframeBars,
    average: IndicatorLine,
    range: IndicatorLine,
    // The last closed bucket's values: they move only on the bar that closes one.
    centre: f64,
    atr: f64,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        four_hours: HigherTimeframeBars::minutes(240),
        average: IndicatorLine::new("ema", &[27.0]),
        range: IndicatorLine::new("atr", &[15.0]),
        centre: f64::NAN,
        atr: f64::NAN,
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if let Some(closed) = self.four_hours.push(bar) {
            self.centre = self.average.next(&closed)[0];
            self.atr = self.range.next(&closed)[0];
        }
        let line = self.centre + self.atr * LINE_ATRS;
        if matches!(self.cross.step(bar.close, line), Signal::Long) {
            strategy.entry(NAME, Long).size(SIZE);
        }
        if strategy.position() > 0.0 {
            if bar.close < self.centre {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - self.atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
