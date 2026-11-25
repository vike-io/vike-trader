// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-kkb-ema-atr-breakout-band-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through EMA(20) + 1.5 ATR(14), short when it crosses down through EMA(20) - 1.5 ATR(14).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const ATR_MULTIPLE: f64 = 1.5;

/// The rule itself, also copied into `combo_4`.
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

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(BreakoutBand::new()))
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
