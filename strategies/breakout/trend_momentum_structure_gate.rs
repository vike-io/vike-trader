// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-trendmaster-trend-momentum-structure-gate-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(21) is above EMA(55), RSI(14) is above 55 and the close is above the previous bar's 20-bar highest high. Short
//! when EMA(21) is below EMA(55), RSI(14) is below 45 and the close is below the previous bar's 20-bar lowest low.
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
    fast: IndicatorLine,
    slow: IndicatorLine,
    rsi: IndicatorLine,
    channel: IndicatorLine,
    highest_before: f64,
    lowest_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: IndicatorLine::new("ema", &[21.0]),
        slow: IndicatorLine::new("ema", &[55.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        channel: IndicatorLine::new("donchian", &[20.0]),
        highest_before: f64::NAN,
        lowest_before: f64::NAN,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        // The `donchian` row's lines are upper, mid, lower.
        let lines = self.channel.next(bar);
        let highest = std::mem::replace(&mut self.highest_before, lines[0]);
        let lowest = std::mem::replace(&mut self.lowest_before, lines[2]);
        if fast > slow && rsi > 55.0 && bar.close > highest {
            Signal::Long
        } else if fast < slow && rsi < 45.0 && bar.close < lowest {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
