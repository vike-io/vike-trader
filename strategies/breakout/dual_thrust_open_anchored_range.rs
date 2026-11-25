// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-dual-thrust-open-anchored-range-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Over the last 4 bars take the larger of (highest high - lowest close) and (highest close - lowest low). Long when the close
//! is above the bar's open plus half of that, short when it is below the open minus half of it. A position's own exit asks for
//! the same bars as the opposite entry, which reverses it, so the exit never decides anything.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const UPPER_FACTOR: f64 = 0.5;
const LOWER_FACTOR: f64 = 0.5;

struct S {
    high_low: IndicatorLine,
    closes: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        high_low: IndicatorLine::new("donchian", &[4.0]),
        closes: Donchian::new(4.0).expect("a valid period"),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // A channel's lines are upper, mid, lower. Fed the close as both prices it spans the closes.
        let high_low = self.high_low.next(bar);
        let closes = self.closes.next_hl(bar.close, bar.close);
        let range = (high_low[0] - closes[2]).max(closes[0] - high_low[2]);

        if bar.close > bar.open + UPPER_FACTOR * range {
            Signal::Long
        } else if bar.close < bar.open - LOWER_FACTOR * range {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
