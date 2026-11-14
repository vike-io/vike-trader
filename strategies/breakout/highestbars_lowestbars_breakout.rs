// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-highestbars-lowestbars-breakout-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on a bar whose high is the highest of the last 20 while the close is above EMA(50), short on a bar whose low is the
//! lowest of the last 20 while the close is below it. A long closes under the middle of that 20-bar channel, a short over it.
//!
//! A bar that only ties an earlier bar of the 20 is not the extreme, the earlier bar is. vike has no bars-since-extreme
//! indicator, so "this bar is the extreme" is read as beating the 19 bars before it.
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
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    channel: IndicatorLine,
    earlier: IndicatorLine,
    previous_earlier: Vec<f64>,
    average: Ema,
    // The position these rules asked for: a signal sees no broker.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        channel: IndicatorLine::new("donchian", &[20.0]),
        earlier: IndicatorLine::new("donchian", &[19.0]),
        previous_earlier: vec![f64::NAN; 3],
        average: Ema::new(50.0).expect("a valid period"),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `donchian` row's lines are upper, mid, lower.
        let mid = self.channel.next(bar)[1];
        let earlier = std::mem::replace(&mut self.previous_earlier, self.earlier.next(bar));
        let average = self.average.next_value(bar.close);

        let long = bar.high > earlier[0] && bar.close > average;
        let short = bar.low < earlier[2] && bar.close < average;
        let close_long = self.held > 0 && bar.close < mid;
        let close_short = self.held < 0 && bar.close > mid;

        // An entry on the side already held adds nothing, so the close beside it still runs; a reversal replaces it.
        if (close_long && !short) || (close_short && !long) {
            self.held = 0;
            Signal::Close
        } else if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
