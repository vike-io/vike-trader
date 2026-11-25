// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-kkb-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! `ema_atr_breakout_band`'s entries, a long only while `kalman_filter_1d`'s filter rises on the bar and a short only while it
//! falls.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::indicators::Kalman;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::Cross;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    filter: Kalman,
    /// NaN before the first bar, and a NaN compares false.
    last_estimate: f64,
    band: BreakoutBand,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { filter: filter(), last_estimate: f64::NAN, band: BreakoutBand::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let estimate = self.filter.next_value(bar.close);
        let rising = estimate > self.last_estimate;
        let falling = estimate < self.last_estimate;
        self.last_estimate = estimate;
        match self.band.signal(bar) {
            Signal::Long if rising => Signal::Long,
            Signal::Short if falling => Signal::Short,
            _ => Signal::Hold,
        }
    }
}

const ATR_MULTIPLE: f64 = 1.5;

/// The rule itself, copied from `ema_atr_breakout_band`.
struct BreakoutBand {
    average: Ema,
    atr: IndicatorLine,
    through_upper: Cross,
    through_lower: Cross,
}

impl BreakoutBand {
    fn new() -> Self {
        BreakoutBand {
            average: Ema::new(20.0).expect("a valid period"),
            atr: IndicatorLine::new("atr", &[14.0]),
            through_upper: Cross::new(),
            through_lower: Cross::new(),
        }
    }
}

impl SignalSource for BreakoutBand {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let average = self.average.next(bar);
        let width = self.atr.next(bar)[0] * ATR_MULTIPLE;
        // Both step on every bar, and the close is never above the upper band and below the lower one at once.
        let up = self.through_upper.step(bar.close, average + width);
        let down = self.through_lower.step(bar.close, average - width);
        match (up, down) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}

const PROCESS_VARIANCE: f64 = 0.001;
const MEASUREMENT_VARIANCE: f64 = 0.1;

/// vike's `Kalman` with this rule's variances (it starts on the first close, error variance 1). Copied from `kalman_filter_1d`.
fn filter() -> Kalman {
    Kalman::new(PROCESS_VARIANCE, MEASUREMENT_VARIANCE).expect("valid variances")
}
