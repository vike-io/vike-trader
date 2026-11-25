// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-boscurv-linreg-slope-channel-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the upper band, short when it crosses down through the lower band. The bands are the
//! 50-bar regression line of the close moved by 5 times the regression's slope per bar, then 2 ATR(14) either way.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const LENGTH: f64 = 50.0;
const SLOPE_MULTIPLE: f64 = 5.0;
const ATR_MULTIPLE: f64 = 2.0;

/// The channel, also copied into `combo_2`.
struct CurvedChannel {
    line: IndicatorLine,
    slope: IndicatorLine,
    atr: IndicatorLine,
}

impl CurvedChannel {
    fn new() -> Self {
        CurvedChannel {
            line: IndicatorLine::new("linearreg", &[LENGTH]),
            slope: IndicatorLine::new("linearreg_slope", &[LENGTH]),
            atr: IndicatorLine::new("atr", &[14.0]),
        }
    }
    /// The moved regression line, the upper band and the lower band.
    fn step(&mut self, bar: &Bar) -> [f64; 3] {
        let centre = self.line.next(bar)[0] + self.slope.next(bar)[0] * SLOPE_MULTIPLE;
        let width = self.atr.next(bar)[0] * ATR_MULTIPLE;
        [centre, centre + width, centre - width]
    }
}

struct S {
    channel: CurvedChannel,
    through_upper: Cross,
    through_lower: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        channel: CurvedChannel::new(),
        through_upper: Cross::new(),
        through_lower: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [_, upper, lower] = self.channel.step(bar);
        // Both step on every bar, and the close is never above the upper band and below the lower one at once.
        let up = self.through_upper.step(bar.close, upper);
        let down = self.through_lower.step(bar.close, lower);
        match (up, down) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
