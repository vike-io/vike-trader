// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/barstate-isconfirmed-magnifier-on-01a/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long only, with latched signals: EMA(9) crossing up through EMA(21) sets a pending entry and crossing down sets a pending
//! exit, each acting once the position lets it. The rule of `barstate_isconfirmed_magnifier_off` with the bar
//! magnifier on: a market order fills at the next bar's open either way, so the chart's own bars are all it needs.
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
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
    pending_entry: bool,
    pending_exit: bool,
    /// The position this bar opened with: an order sent on a bar fills at the next bar's open.
    long: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
        pending_entry: false,
        pending_exit: false,
        long: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long => self.pending_entry = true,
            Signal::Short => self.pending_exit = true,
            _ => {}
        }
        if self.pending_entry && !self.long {
            self.pending_entry = false;
            self.long = true;
            Signal::Long
        } else if self.pending_exit && self.long {
            self.pending_exit = false;
            self.long = false;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
