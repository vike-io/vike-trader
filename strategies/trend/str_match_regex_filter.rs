// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-str-match-regex-filter-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long unless a test named in the rule text "MACD=LONG,RSI>50" vetoes it, otherwise close. "rsi>50" vetoes when RSI(14) is at
//! or below 50, "macd=long" when the MACD(12,26,9) line is at or below its signal. A NaN vetoes nothing. It never shorts.
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

const RULE_TEXT: &str = "MACD=LONG,RSI>50";

struct S {
    rsi: IndicatorLine,
    macd: IndicatorLine,
    tests_rsi: bool,
    tests_macd: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    // Neither pattern holds a regex metacharacter, so each accepts exactly its own letters anywhere in the lowered text.
    let lowered = RULE_TEXT.to_lowercase();
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        macd: IndicatorLine::new("macd", &[12.0, 26.0, 9.0]),
        tests_rsi: lowered.contains("rsi>50"),
        tests_macd: lowered.contains("macd=long"),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let macd = self.macd.next(bar);
        let vetoed = (self.tests_rsi && rsi <= 50.0) || (self.tests_macd && macd[0] <= macd[1]);
        if vetoed { Signal::Close } else { Signal::Long }
    }
}
