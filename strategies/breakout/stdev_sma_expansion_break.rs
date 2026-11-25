// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-stdev-sma-expansion-break-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close is above SMA(20) plus 2 standard deviations of the last 20 closes, short when it is below SMA(20) minus
//! 2 of them.
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
    baseline: IndicatorLine,
    deviation: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        baseline: IndicatorLine::new("sma", &[20.0]),
        deviation: IndicatorLine::new("stddev", &[20.0]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let baseline = self.baseline.next(bar)[0];
        let width = self.deviation.next(bar)[0] * 2.0;
        if bar.close > baseline + width {
            Signal::Long
        } else if bar.close < baseline - width {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
