// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-aperture-cog-linreg-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the center of gravity of the last 13 closes crosses up through its EMA(7), the 38-bar regression line is
//! rising and the close is above it. Closed when the center of gravity crosses down through its EMA, the line stops rising, or the
//! close is more than 2.5 ATR(20) under the average cost. 0.05% commission, 1 tick.
//!
//! The line's value one bar back on the same fit is its value now less its slope, so "rising" is a positive slope. The center of
//! gravity is written here: vike has no such indicator.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const COG_WINDOW: usize = 13;
const LINE_LENGTH: f64 = 38.0;
const MOST_ATRS_AGAINST: f64 = 2.5;

struct S {
    /// Newest first.
    closes: VecDeque<f64>,
    signal: Ema,
    line: IndicatorLine,
    slope: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: VecDeque::with_capacity(COG_WINDOW + 1),
        // The center's NaNs before its window is full are skipped, not pushed.
        signal: Ema::new(7.0).expect("a valid period").with_skip_nan(true),
        line: IndicatorLine::new("linearreg", &[LINE_LENGTH]),
        slope: IndicatorLine::new("linearreg_slope", &[LINE_LENGTH]),
        atr: IndicatorLine::new("atr", &[20.0]),
        cross: Cross::new(),
    })
}

impl S {
    /// The center of gravity, NaN until the window is full.
    fn center_of_gravity(&mut self, close: f64) -> f64 {
        self.closes.push_front(close);
        self.closes.truncate(COG_WINDOW);
        if self.closes.len() < COG_WINDOW {
            return f64::NAN;
        }
        let moment: f64 = self.closes.iter().zip(1_i32..).map(|(c, i)| c * f64::from(i)).sum();
        -moment / self.closes.iter().sum::<f64>()
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let cog = self.center_of_gravity(bar.close);
        let signal = self.signal.next_value(cog);
        let line = self.line.next(bar)[0];
        let rising = self.slope.next(bar)[0] > 0.0;
        let atr = self.atr.next(bar)[0];
        let crossed = self.cross.step(cog, signal);
        let enter = matches!(crossed, Signal::Long) && rising && bar.close > line;
        let ended = matches!(crossed, Signal::Short) || !rising;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
