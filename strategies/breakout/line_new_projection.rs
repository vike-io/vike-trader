// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-trendmaster-line-new-projection-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through a line through the last two confirmed pivot lows, projected to this bar. Short when it
//! crosses down through the same line over the last two confirmed pivot highs. A pivot has 5 bars on each side and is confirmed 5
//! bars after it. An opposite position reverses and the same side already held is ignored.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const SIDE: i64 = 5;

/// The last two pivots of one side as `(bar, price)`: the only part of the drawn lines the rule reads back. Two pivots never share
/// a bar, so the slope's divisor is zero only while the prices are still NaN.
struct PivotLine {
    older: (i64, f64),
    newer: (i64, f64),
}

impl PivotLine {
    fn new() -> Self {
        PivotLine { older: (0, f64::NAN), newer: (0, f64::NAN) }
    }
    /// `pivot` is the one this bar confirms, NaN when none; it sits `SIDE` bars back.
    fn confirm(&mut self, pivot: f64, this_bar: i64) {
        if !pivot.is_nan() {
            self.older = self.newer;
            self.newer = (this_bar - SIDE, pivot);
        }
    }
    /// The line's price at a bar, NaN until two pivots exist.
    fn at(&self, this_bar: i64) -> f64 {
        let slope = (self.newer.1 - self.older.1) / (self.newer.0 - self.older.0) as f64;
        self.newer.1 + slope * (this_bar - self.newer.0) as f64
    }
}

struct S {
    pivots: ConfirmedPivot,
    highs: PivotLine,
    lows: PivotLine,
    over_lows: Cross,
    under_highs: Cross,
    this_bar: i64,
    // Only a bar firing both sides reads it. An entry sets it whatever came before, so the warm-up, which trades nothing, cannot
    // leave it wrong after the first real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(SIDE as f64, SIDE as f64).expect("valid spans"),
        highs: PivotLine::new(),
        lows: PivotLine::new(),
        over_lows: Cross::new(),
        under_highs: Cross::new(),
        this_bar: 0,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [pivot_high, pivot_low, ..] = self.pivots.next(bar);
        self.highs.confirm(pivot_high, self.this_bar);
        self.lows.confirm(pivot_low, self.this_bar);

        let long =
            matches!(self.over_lows.step(bar.close, self.lows.at(self.this_bar)), Signal::Long);
        let short =
            matches!(self.under_highs.step(bar.close, self.highs.at(self.this_bar)), Signal::Short);
        self.this_bar += 1;

        match (long, short) {
            (true, true) if self.held > 0 => {
                self.held = -1;
                Signal::Short
            }
            (true, _) => {
                self.held = 1;
                Signal::Long
            }
            (_, true) => {
                self.held = -1;
                Signal::Short
            }
            _ => Signal::Hold,
        }
    }
}
