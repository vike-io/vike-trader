// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-rangefinder-daily-reference-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the prior day's range is at least 1.2% of its low and the 15-minute close crosses up through the prior
//! day's high. Closed when the close is below that day's midpoint, or more than 2.5 ATR(17) under the average cost. The prior day
//! is the one before the last closed UTC day, so its levels step once a day on the day's last bar. 0.05% commission, 1 tick.
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
const MIN_RANGE_PERCENT: f64 = 1.2;
const MOST_ATRS_AGAINST: f64 = 2.5;

struct S {
    days: HigherTimeframeBars,
    /// `(high, low)` of the last closed day, and of the one before it.
    closed: (f64, f64),
    prior: (f64, f64),
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeBars::minutes(1_440),
        closed: (f64::NAN, f64::NAN),
        prior: (f64::NAN, f64::NAN),
        atr: IndicatorLine::new("atr", &[17.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if let Some(day) = self.days.push(bar) {
            self.prior = self.closed;
            self.closed = (day.high, day.low);
        }
        let (high, low) = self.prior;
        let range_percent = if low > 0.0 { (high - low) / low * 100.0 } else { 0.0 };
        let atr = self.atr.next(bar)[0];
        // The cross sits behind the range test: it only remembers the bars that test passed.
        let enter = range_percent >= MIN_RANGE_PERCENT
            && matches!(self.cross.step(bar.close, high), Signal::Long);
        let held = strategy.position();
        let lost = bar.close < (high + low) * 0.5;
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (lost || too_far) {
            strategy.close(NAME);
        }
    }
}
