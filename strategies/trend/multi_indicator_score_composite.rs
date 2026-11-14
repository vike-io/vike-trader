// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-multi-indicator-score-composite-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Scores three votes each bar: RSI(14) against 50, the close against EMA(20), the close against SMA(20). Long when the score
//! reaches +2 from below, short when it reaches -2 from above. A long closes at a score of 0 or less, a short at 0 or more.
//!
//! Before the averages have a value the close counts as not above them.
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
    rsi: IndicatorLine,
    ema: IndicatorLine,
    sma: IndicatorLine,
    last_score: Option<i32>,
    // Which side is held, since a neutral score closes only one side.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        ema: IndicatorLine::new("ema", &[20.0]),
        sma: IndicatorLine::new("sma", &[20.0]),
        last_score: None,
        held: 0,
    }))
}

/// +1 above the average, -1 otherwise, including while the average is a NaN.
fn vote(close: f64, average: f64) -> i32 {
    if close > average { 1 } else { -1 }
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let ema = self.ema.next(bar)[0];
        let sma = self.sma.next(bar)[0];
        // Exactly 50, or a NaN, votes neither way.
        let rsi_vote = if rsi > 50.0 {
            1
        } else if rsi < 50.0 {
            -1
        } else {
            0
        };
        let score = rsi_vote + vote(bar.close, ema) + vote(bar.close, sma);
        let last = self.last_score.replace(score);

        if score >= 2 && last.is_some_and(|s| s < 2) {
            self.held = 1;
            Signal::Long
        } else if score <= -2 && last.is_some_and(|s| s > -2) {
            self.held = -1;
            Signal::Short
        } else if (self.held > 0 && score <= 0) || (self.held < 0 && score >= 0) {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
