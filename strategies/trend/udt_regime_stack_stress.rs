// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-regime-stack-stress-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Scores three votes each bar: RSI(14) against 50, the close against EMA(20), the close against the middle of its 20-bar
//! Bollinger band. Long when the score reaches +2 from below, short when it reaches -2 from above, each only while ATR(14) is under
//! 25 times its SMA(20). A long closes at a score of 0 or less, a short at 0 or more.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Sma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const ENTER_SCORE: i32 = 2;
const MAX_ATR_RATIO: f64 = 25.0;

struct S {
    rsi: IndicatorLine,
    ema: IndicatorLine,
    // A Bollinger band's middle is the SMA of its length.
    band_middle: IndicatorLine,
    atr: Atr,
    atr_mean: Sma,
    last_score: Option<i32>,
    // Which side is held, since a neutral score closes only one side.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        ema: IndicatorLine::new("ema", &[20.0]),
        band_middle: IndicatorLine::new("sma", &[20.0]),
        atr: Atr::new(14.0).expect("a valid period"),
        atr_mean: Sma::new(20.0).expect("a valid period"),
        last_score: None,
        held: 0,
    }))
}

/// +1 above the line, -1 otherwise, including while the line is a NaN.
fn side_of(close: f64, line: f64) -> i32 {
    if close > line { 1 } else { -1 }
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let ema = self.ema.next(bar)[0];
        let middle = self.band_middle.next(bar)[0];
        // Exactly 50, or a NaN, votes neither way.
        let rsi_vote = if rsi > 50.0 {
            1
        } else if rsi < 50.0 {
            -1
        } else {
            0
        };
        let score = rsi_vote + side_of(bar.close, ema) + side_of(bar.close, middle);
        let last = self.last_score.replace(score);

        let atr = self.atr.next(bar);
        // The mean takes known values only: a NaN in its running sum would stay until the sum's next rebuild.
        let atr_mean = if atr.is_nan() { f64::NAN } else { self.atr_mean.next_value(atr) };
        let atr_ratio = if atr_mean > 0.0 { atr / atr_mean } else { 1.0 };
        let calm = atr_ratio < MAX_ATR_RATIO;

        if calm && score >= ENTER_SCORE && last.is_some_and(|s| s < ENTER_SCORE) {
            self.held = 1;
            Signal::Long
        } else if calm && score <= -ENTER_SCORE && last.is_some_and(|s| s > -ENTER_SCORE) {
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
