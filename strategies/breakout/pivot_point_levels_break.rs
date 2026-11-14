// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-pivot-point-levels-break-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the previous bar's floor-pivot R1, short when it crosses down through its S1.
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

/// Where R1 and S1 sit in the `pivot_points` row's lines.
const R1: usize = 1;
const S1: usize = 4;

struct S {
    levels: IndicatorLine,
    against_r1: Cross,
    against_s1: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        levels: IndicatorLine::new("pivot_points", &[]),
        against_r1: Cross::new(),
        against_s1: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let levels = self.levels.next(bar);
        let up = self.against_r1.step(bar.close, levels[R1]);
        let down = self.against_s1.step(bar.close, levels[S1]);
        match (up, down) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
