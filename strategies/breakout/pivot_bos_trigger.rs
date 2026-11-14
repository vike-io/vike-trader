// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-boscurv-pivot-bos-trigger-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the last confirmed pivot high, short when it crosses down through the last confirmed
//! pivot low. A pivot has 5 bars on each side and is confirmed 5 bars after it.
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

/// The break detector, also copied into `combo_2`.
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

struct S {
    structure: StructureBreak,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { structure: StructureBreak::new() }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        match self.structure.step(bar) {
            (true, _) => Signal::Long,
            (_, true) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
