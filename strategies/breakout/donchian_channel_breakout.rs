// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-donchian-channel-breakout-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close breaks above the previous bar's 20-bar highest high, short when it breaks below the previous bar's
//! 20-bar lowest low. A long closes under the previous bar's 10-bar lowest low, a short over its 10-bar highest high.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    entry_channel: IndicatorLine,
    exit_channel: IndicatorLine,
    previous_entry: Vec<f64>,
    previous_exit: Vec<f64>,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        entry_channel: IndicatorLine::new("donchian", &[20.0]),
        exit_channel: IndicatorLine::new("donchian", &[10.0]),
        previous_entry: vec![f64::NAN; 3],
        previous_exit: vec![f64::NAN; 3],
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `donchian` row's lines are upper, mid, lower.
        let entry = std::mem::replace(&mut self.previous_entry, self.entry_channel.next(bar));
        let exit = std::mem::replace(&mut self.previous_exit, self.exit_channel.next(bar));

        let long = bar.close > entry[0];
        let short = bar.close < entry[2];
        let close_out =
            (self.held > 0 && bar.close < exit[2]) || (self.held < 0 && bar.close > exit[0]);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if close_out {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
