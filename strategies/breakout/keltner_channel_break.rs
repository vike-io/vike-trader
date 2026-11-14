// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-keltner-channel-break-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the Keltner(20, 10, 2) upper band, short when it crosses down through the lower
//! band. A long closes when the close crosses down through the mid line, a short when it crosses up through it.
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
    upper: Cross,
    mid: Cross,
    lower: Cross,
    // Which side is held, since a mid-line cross closes only one side. Entries set it whatever came before, so warm-up
    // signals, which trade nothing, cannot leave it wrong after the first real entry.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        channel: IndicatorLine::new("keltner", &[20.0, 10.0, 2.0]),
        upper: Cross::new(),
        mid: Cross::new(),
        lower: Cross::new(),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `keltner` row's lines are upper, mid, lower.
        let lines = self.channel.next(bar);
        let upper = self.upper.step(bar.close, lines[0]);
        let mid = self.mid.step(bar.close, lines[1]);
        let lower = self.lower.step(bar.close, lines[2]);
        if matches!(upper, Signal::Long) {
            self.held = 1;
            return Signal::Long;
        }
        if matches!(lower, Signal::Short) {
            self.held = -1;
            return Signal::Short;
        }
        match mid {
            Signal::Short if self.held > 0 => {
                self.held = 0;
                Signal::Close
            }
            Signal::Long if self.held < 0 => {
                self.held = 0;
                Signal::Close
            }
            _ => Signal::Hold,
        }
    }
}
