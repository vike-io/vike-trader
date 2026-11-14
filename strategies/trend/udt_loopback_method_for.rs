// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-loopback-method-for-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A vote over five horizons, 4, 8, 12, 16 and 20 bars back: +1 when the close is above the close that far back, otherwise -1, and
//! 0 while that bar does not exist yet, averaged over the five. Long 2 when flat, the average crosses up through 0.55 and the close
//! is above EMA(51). Closed when the average is under 0, the close is under the EMA, or the close is more than 2.4 ATR(15) under the
//! average cost. 0.05% commission, 1 tick.
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
const HORIZONS: usize = 5;
const SPACING: usize = 4;
const ENTER_ABOVE: f64 = 0.55;
const MOST_ATRS_AGAINST: f64 = 2.4;

struct S {
    closes: Vec<f64>,
    trend: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

impl S {
    /// After this bar's close is pushed.
    fn average_vote(&self, close: f64) -> f64 {
        let votes: f64 = (1..=HORIZONS)
            .map(|horizon| match self.closes.len().checked_sub(1 + horizon * SPACING) {
                Some(at) if close > self.closes[at] => 1.0,
                Some(_) => -1.0,
                None => 0.0,
            })
            .sum();
        votes / HORIZONS as f64
    }
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: Vec::new(),
        trend: IndicatorLine::new("ema", &[51.0]),
        atr: IndicatorLine::new("atr", &[15.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.closes.push(bar.close);
        let average = self.average_vote(bar.close);
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.cross.step(average, ENTER_ABOVE), Signal::Long);
        let enter = crossed_up && bar.close > trend;
        let ended = average < 0.0 || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
