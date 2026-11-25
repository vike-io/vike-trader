// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-elder-ray-bull-bear-power-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close is above EMA(50) and bear power(13) is negative but rising, short when the close is below EMA(50) and
//! bull power(13) is positive but falling. A long closes when bear power is positive and falling, a short when bull power is
//! negative and rising.
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
    power: IndicatorLine,
    trend: IndicatorLine,
    previous_bull: f64,
    previous_bear: f64,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        power: IndicatorLine::new("elder_ray", &[13.0]),
        trend: IndicatorLine::new("ema", &[50.0]),
        previous_bull: f64::NAN,
        previous_bear: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `elder_ray` row's lines are bull power, bear power.
        let power = self.power.next(bar);
        let (bull, bear) = (power[0], power[1]);
        let trend = self.trend.next(bar)[0];
        let previous_bull = std::mem::replace(&mut self.previous_bull, bull);
        let previous_bear = std::mem::replace(&mut self.previous_bear, bear);

        let long = bar.close > trend && bear < 0.0 && bear > previous_bear;
        let short = bar.close < trend && bull > 0.0 && bull < previous_bull;
        let exit = (self.held > 0 && bear > 0.0 && bear < previous_bear)
            || (self.held < 0 && bull < 0.0 && bull > previous_bull);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if exit {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
