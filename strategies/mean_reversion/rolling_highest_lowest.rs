// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-marketshift-rolling-highest-lowest-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the previous bar's 50-bar lowest low, short when it crosses down through the previous
//! bar's 50-bar highest high.
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

struct S {
    channel: IndicatorLine,
    previous_highest: f64,
    previous_lowest: f64,
    over_lowest: Cross,
    under_highest: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        channel: IndicatorLine::new("donchian", &[50.0]),
        previous_highest: f64::NAN,
        previous_lowest: f64::NAN,
        over_lowest: Cross::new(),
        under_highest: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `donchian` row's lines are upper, mid, lower.
        let lines = self.channel.next(bar);
        let highest = std::mem::replace(&mut self.previous_highest, lines[0]);
        let lowest = std::mem::replace(&mut self.previous_lowest, lines[2]);
        // Both crosses step on every bar, or one skipped would compare against a stale previous value.
        let up = self.over_lowest.step(bar.close, lowest);
        let down = self.under_highest.step(bar.close, highest);
        match (up, down) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Short,
            _ => Signal::Hold,
        }
    }
}
