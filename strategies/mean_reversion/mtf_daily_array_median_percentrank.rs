// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-daily-array-median-percentrank-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Keeps the last closed UTC day's close once per 15-minute bar, for the last 20 bars. Long when the newest value's percent rank
//! among them is below 10, short when it is above 90. A bar before the first closed day holds a NaN that ranks below everything.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::PercentRank;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

/// The 19 values before the newest: vike's `PercentRank`. The window never holds more than two days' closes, so this and
/// counting the newest value itself over all 20 cross 10 and 90 on the same bars.
const OTHERS: f64 = 19.0;

struct S {
    days: HigherTimeframeBars,
    last_closed: f64,
    rank: PercentRank,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        days: HigherTimeframeBars::minutes(1_440),
        last_closed: f64::NAN,
        rank: PercentRank::new(OTHERS).expect("a valid length"),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        if let Some(day) = self.days.push(bar) {
            self.last_closed = day.close;
        }
        let rank = self.rank.next_value(self.last_closed);
        if rank < 10.0 {
            Signal::Long
        } else if rank > 90.0 {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
