// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-daily-ema26-warmup-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the EMA(26) of daily closes has just risen, short when it has just fallen. The average runs on closed UTC days and
//! moves only on a day's last bar, so a position can flip only there.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    days: HigherTimeframeBars,
    average: Ema,
    level: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        days: HigherTimeframeBars::minutes(1_440),
        average: Ema::new(26.0).expect("a valid period"),
        level: f64::NAN,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let Some(day) = self.days.push(bar) else {
            return Signal::Hold;
        };
        let before = self.level;
        self.level = self.average.next(&day);
        if self.level > before {
            Signal::Long
        } else if self.level < before {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
