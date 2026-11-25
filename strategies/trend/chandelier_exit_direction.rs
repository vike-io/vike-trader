// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-chandelier-exit-direction-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A direction turns up when the close is above the previous bar's short stop and down when it is below the previous bar's
//! long stop, the down test checked last. Go long on the bar it turns up, short on the bar it turns down. The stops are the
//! 22-bar highest high minus 3 ATR(22) and the 22-bar lowest low plus 3 ATR(22).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    stops: IndicatorLine,
    previous_long_stop: f64,
    previous_short_stop: f64,
    direction: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        stops: IndicatorLine::new("chandelier", &[22.0, 3.0]),
        previous_long_stop: f64::NAN,
        previous_short_stop: f64::NAN,
        direction: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `chandelier` row's lines are the long stop and the short stop.
        let stops = self.stops.next(bar);
        let previous_direction = self.direction;
        if bar.close > self.previous_short_stop {
            self.direction = 1;
        }
        if bar.close < self.previous_long_stop {
            self.direction = -1;
        }
        (self.previous_long_stop, self.previous_short_stop) = (stops[0], stops[1]);

        if self.direction == 1 && previous_direction != 1 {
            Signal::Long
        } else if self.direction == -1 && previous_direction != -1 {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
