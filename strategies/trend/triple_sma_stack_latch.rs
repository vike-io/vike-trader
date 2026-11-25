// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-triple-sma-stack-latch-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! SMA(10) above SMA(20) above SMA(50) is a bull stack, the reverse a bear stack. Long on the bar a bull stack starts, short on
//! the bar a bear stack starts. A long closes once the bull stack is gone, a short once the bear stack is gone.
//!
//! The averages are vike's `Sma` summing its window afresh (`with_fresh_sum`): its running sum's last bits can flip a
//! comparison of two nearly equal averages, ending a trade a bar late.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    fast: Sma,
    middle: Sma,
    slow: Sma,
    was_bull: bool,
    was_bear: bool,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

/// `NaN` until `length` closes are in.
fn average(length: f64) -> Sma {
    Sma::new(length).expect("a valid period").with_fresh_sum(true)
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: average(10.0),
        middle: average(20.0),
        slow: average(50.0),
        was_bull: false,
        was_bear: false,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (fast, middle, slow) = (
            self.fast.next_value(bar.close),
            self.middle.next_value(bar.close),
            self.slow.next_value(bar.close),
        );
        let bull = fast > middle && middle > slow;
        let bear = fast < middle && middle < slow;
        let was_bull = std::mem::replace(&mut self.was_bull, bull);
        let was_bear = std::mem::replace(&mut self.was_bear, bear);

        let long = bull && !was_bull;
        let short = bear && !was_bear;
        let exit = (self.held > 0 && !bull) || (self.held < 0 && !bear);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if exit {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
