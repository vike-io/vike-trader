// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-pivot-array-unshift-pop-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close moves above the newest pivot high from at or below it, short when it moves below the newest pivot low from
//! at or above it. A long closes once the close is below the pivot low, a short once it is above the pivot high. Each side keeps
//! its 5 newest pivots, newest first, and reads only the first. A pivot has 5 bars on each side.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::params::Params;

const MAX_KEPT: usize = 5;

/// `pivot` is the one this bar confirms, NaN when none.
fn remember(levels: &mut VecDeque<f64>, pivot: f64) {
    if !pivot.is_nan() {
        levels.push_front(pivot);
        levels.truncate(MAX_KEPT);
    }
}

/// The newest level, NaN until one exists: a NaN compares false, so no test below passes against it.
fn newest(levels: &VecDeque<f64>) -> f64 {
    levels.front().copied().unwrap_or(f64::NAN)
}

struct S {
    pivots: ConfirmedPivot,
    resistance: VecDeque<f64>,
    support: VecDeque<f64>,
    previous_close: f64,
    // The position these rules asked for: a signal sees no broker. An entry sets it whatever came before, so the warm-up, which
    // trades nothing, cannot leave it wrong after the first real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"),
        resistance: VecDeque::with_capacity(MAX_KEPT + 1),
        support: VecDeque::with_capacity(MAX_KEPT + 1),
        previous_close: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [pivot_high, pivot_low, ..] = self.pivots.next(bar);
        remember(&mut self.resistance, pivot_high);
        remember(&mut self.support, pivot_low);
        let (top, bottom) = (newest(&self.resistance), newest(&self.support));

        let long = bar.close > top && self.previous_close <= top;
        let short = bar.close < bottom && self.previous_close >= bottom;
        self.previous_close = bar.close;

        // An entry is placed before an exit, so a reversal leaves the exit nothing to close. The same side already held does
        // not enter, which leaves its exit standing.
        let exits = (self.held > 0 && !short && bar.close < bottom)
            || (self.held < 0 && !long && bar.close > top);
        match (long, short) {
            (true, true) if self.held > 0 => {
                self.held = -1;
                Signal::Short
            }
            _ if exits => {
                self.held = 0;
                Signal::Close
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
