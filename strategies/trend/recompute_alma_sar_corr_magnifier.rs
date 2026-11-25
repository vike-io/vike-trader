// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/recompute-alma-sar-corr-magnifier-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only. Buy when the close crosses up through ALMA(14, offset 0.85, sigma 6) while the parabolic SAR(start 0.02, step
//! 0.02, cap 0.2) is below the close and the 20-bar correlation of close and volume is above zero. Close when the SAR is above
//! the close. It asks for the bar magnifier, which does not move a market order's fill at the next bar's open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Psar;
use vike_indicators::pairs::Correl;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const ACCEL_START: f64 = 0.02;
const ACCEL_STEP: f64 = 0.02;
const ACCEL_CAP: f64 = 0.2;

struct S {
    alma: IndicatorLine,
    cross: Cross,
    /// `sar_flip_entry`'s stop, tested for a reversal before the clamp, so a bar that touches only the unclamped stop reverses it.
    stop: Psar,
    correlation: Correl,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        alma: IndicatorLine::new("alma", &[14.0, 0.85, 6.0]),
        cross: Cross::new(),
        stop: parabolic_sar(ACCEL_START, ACCEL_STEP, ACCEL_CAP),
        correlation: Correl::with_params(&[20.0]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let alma = self.alma.next(bar)[0];
        let crossed_up = matches!(self.cross.step(bar.close, alma), Signal::Long);
        let stop = self.stop.next(bar);
        let correlation = self.correlation.on_pair_values(bar.close, bar.volume);
        // The stop cannot be on both sides of the close, so one bar never asks for both.
        if crossed_up && stop < bar.close && correlation > 0.0 {
            Signal::Long
        } else if stop > bar.close {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
