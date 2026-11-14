// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-supertrend-adx-filter-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar Supertrend(10, 3) turns up with ADX(14) above 20, short on the bar it turns down with ADX above 20. A long
//! closes whenever the trend is down and a short whenever it is up, so a turn with a weak ADX only flattens.
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
    supertrend: IndicatorLine,
    adx: IndicatorLine,
    was_up: bool,
    was_down: bool,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        supertrend: IndicatorLine::new("supertrend", &[10.0, 3.0]),
        adx: IndicatorLine::new("adx", &[14.0]),
        was_up: false,
        was_down: false,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `supertrend` row's lines are the band and the direction, +1 up and -1 down. The `adx` row's first line is ADX.
        let direction = self.supertrend.next(bar)[1];
        let adx = self.adx.next(bar)[0];
        let up = direction > 0.0;
        let down = direction < 0.0;
        let was_up = std::mem::replace(&mut self.was_up, up);
        let was_down = std::mem::replace(&mut self.was_down, down);

        let strong = adx > 20.0;
        let long = up && !was_up && strong;
        let short = down && !was_down && strong;
        let exit = (self.held > 0 && down) || (self.held < 0 && up);

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
