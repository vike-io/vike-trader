// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-boscurv-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! `pivot_bos_trigger`'s break, taken only on the matching side of `linreg_slope_channel`'s moved regression line: long when the
//! close is above it, short when below. A long closes when the close crosses down through the lower band, a short when it
//! crosses up through the upper band, and an entry on that bar wins over the close.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    structure: StructureBreak,
    channel: CurvedChannel,
    through_upper: Cross,
    through_lower: Cross,
    // The side held on this bar. Entries set it whatever came before, so warm-up signals cannot leave it wrong after the first
    // real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        structure: StructureBreak::new(),
        channel: CurvedChannel::new(),
        through_upper: Cross::new(),
        through_lower: Cross::new(),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (broke_up, broke_down) = self.structure.step(bar);
        let [centre, upper, lower] = self.channel.step(bar);
        let crossed_up = matches!(self.through_upper.step(bar.close, upper), Signal::Long);
        let crossed_down = matches!(self.through_lower.step(bar.close, lower), Signal::Short);
        if broke_up && bar.close > centre {
            self.held = 1;
            Signal::Long
        } else if broke_down && bar.close < centre {
            self.held = -1;
            Signal::Short
        } else if (self.held > 0 && crossed_down) || (self.held < 0 && crossed_up) {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}

const LENGTH: f64 = 50.0;
const SLOPE_MULTIPLE: f64 = 5.0;
const ATR_MULTIPLE: f64 = 2.0;

/// The channel, copied from `linreg_slope_channel`.
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

/// The break detector, copied from `pivot_bos_trigger`.
struct StructureBreak {
    /// Vike's `confirmed_pivot` at 5 bars a side: the older bars may tie and the newer bars must lose strictly, as in
    /// `pivot_confirmed_break`.
    pivots: ConfirmedPivot,
    through_high: Cross,
    through_low: Cross,
}

impl StructureBreak {
    fn new() -> Self {
        StructureBreak {
            pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
            through_high: Cross::new(),
            through_low: Cross::new(),
        }
    }
    /// Whether the close broke up, and whether it broke down. Never both: a pivot confirmed on this bar lies beyond this bar's
    /// close, so a break compares against an unchanged pivot, and that needs the pivot low above the pivot high and the previous
    /// close between them at once. The same reason means a pivot with no predecessor can never break on its first bar.
    fn step(&mut self, bar: &Bar) -> (bool, bool) {
        let [_, _, pivot_high, pivot_low] = self.pivots.next(bar);
        (
            matches!(self.through_high.step(bar.close, pivot_high), Signal::Long),
            matches!(self.through_low.step(bar.close, pivot_low), Signal::Short),
        )
    }
}
