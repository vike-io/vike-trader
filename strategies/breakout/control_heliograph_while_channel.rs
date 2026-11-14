// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/control-heliograph-while-channel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when the close crosses up through WMA(28) plus 1.25 standard deviations of the last 28 closes. Closed when the close crosses
//! down through the WMA or 160 bars have passed since the entry bar. While held, a stop 2.4 ATR(17) under the average cost, moved
//! every bar. 0.05% commission, 1 tick. Starts with 100,000.
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
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const BAND_WIDTH: f64 = 1.25;
const STOP_ATRS: f64 = 2.4;
const MOST_BARS: i64 = 160;

struct S {
    center: IndicatorLine,
    spread: IndicatorLine,
    atr: IndicatorLine,
    over_band: Cross,
    under_center: Cross,
    bar_index: i64,
    opened_at: Option<i64>,
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        center: IndicatorLine::new("wma", &[28.0]),
        spread: IndicatorLine::new("stddev", &[28.0]),
        atr: IndicatorLine::new("atr", &[17.0]),
        over_band: Cross::new(),
        under_center: Cross::new(),
        bar_index: 0,
        opened_at: None,
        held_before: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let center = self.center.next(bar)[0];
        let upper = center + self.spread.next(bar)[0] * BAND_WIDTH;
        let atr = self.atr.next(bar)[0];
        let held = strategy.position();
        if held > 0.0 && self.held_before == 0.0 {
            self.opened_at = Some(self.bar_index);
        }
        let entering = matches!(self.over_band.step(bar.close, upper), Signal::Long);
        let leaving = matches!(self.under_center.step(bar.close, center), Signal::Short);
        let expired =
            held > 0.0 && self.opened_at.is_some_and(|at| self.bar_index - at >= MOST_BARS);
        if entering {
            strategy.entry(NAME, Long).size(SIZE);
        }
        if held > 0.0 {
            if leaving || expired {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        } else {
            self.opened_at = None;
        }
        self.held_before = held;
        self.bar_index += 1;
    }
}
