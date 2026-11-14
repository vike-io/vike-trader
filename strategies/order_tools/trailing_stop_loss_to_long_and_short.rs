// SPDX-License-Identifier: MIT
// Copyright 2019 Mauricio Pimenta | exit490. Licensed under the MIT license; its text, with this notice, is LICENSE-MIT in the strategies folder.
// Derived from "TRAILING STOP LOSS TO LONG AND SHORT" by exit490, https://www.tradingview.com/script/0KrB3hnV/
// Changed: rewritten by hand in Rust for vike.

//! Always long 1: flat, it buys at the next open, remembering this close as its reference and a stop 3% under it. Held, it
//! closes at the next open on a close at or under the stop; otherwise, once the close is more than 1% over the reference,
//! the stop rises to `reference * (1 + (gain - 4) / 100)` (gain in percent, never lower than it was) and becomes the new
//! reference. Every level is judged on closes, not as a resting order. No costs; starts with 100,000.
//!
//! The rule's position type is long; its date range (2014 to 9999) holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::max;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
/// The first stop, in percent under the entry close.
const FIRST_STOP: f64 = -3.0;

struct S {
    /// The price the gain is measured from, and the stop, both `0` while flat.
    reference: f64,
    stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { reference: 0.0, stop: 0.0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let close = bar.close;
        let held = strategy.position() != 0.0;
        if held && close <= self.stop {
            (self.reference, self.stop) = (0.0, 0.0);
            strategy.close(NAME);
        }
        if !held {
            self.reference = close;
            self.stop = close + (close * FIRST_STOP) / 100.0;
            strategy.entry(NAME, Long).size(1.0);
        }
        let gain = (close - self.reference) / self.reference * 100.0;
        if held && gain > 1.0 {
            let percent = FIRST_STOP + gain - 1.0;
            self.stop = max(self.stop, self.reference + (self.reference * percent) / 100.0);
            self.reference = self.stop;
        }
    }
}
