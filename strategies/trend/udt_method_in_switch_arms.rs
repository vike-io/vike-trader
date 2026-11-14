// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-in-switch-arms-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long from the third bar in a row with EMA(9) more than 0.1% above EMA(21), flat from the third bar in a row with it more than
//! 0.1% below. A bar between those two resets both runs. It never shorts.
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

enum Mood {
    Bull,
    Bear,
    Neutral,
}

struct Runs {
    bull: u32,
    bear: u32,
}

impl Runs {
    fn extend_bull(&mut self) {
        self.bull += 1;
        self.bear = 0;
    }
    fn extend_bear(&mut self) {
        self.bear += 1;
        self.bull = 0;
    }
    fn break_both(&mut self) {
        self.bull = 0;
        self.bear = 0;
    }
}

struct S {
    runs: Runs,
    fast: IndicatorLine,
    slow: IndicatorLine,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        runs: Runs { bull: 0, bear: 0 },
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let mood = if fast > slow * 1.001 {
            Mood::Bull
        } else if fast < slow * 0.999 {
            Mood::Bear
        } else {
            Mood::Neutral
        };
        match mood {
            Mood::Bull => self.runs.extend_bull(),
            Mood::Bear => self.runs.extend_bear(),
            Mood::Neutral => self.runs.break_both(),
        }
        match mood {
            Mood::Bull if self.runs.bull >= 3 => Signal::Long,
            Mood::Bear if self.runs.bear >= 3 => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
