// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bracket-atr-trailing-stop-state-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through its EMA(20), short when it crosses down, each reversing what is held. A stop
//! 2 ATR(14) from the close then trails the held side, never moving back, and a close beyond it flattens.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const STOP_IN_ATRS: f64 = 2.0;

struct S {
    atr: IndicatorLine,
    average: Ema,
    cross: Cross,
    /// The side the last cross asked for, cleared when a long's stop is hit. A short's stop leaves it false.
    wants_long: bool,
    /// NaN until the first cross.
    stop: f64,
    // The position this bar opened with: a signal sees no broker, and an order fills at the next bar's open. Whatever it
    // says before trading starts, the first real entry overwrites it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        atr: IndicatorLine::new("atr", &[14.0]),
        average: Ema::new(20.0).expect("a valid period"),
        cross: Cross::new(),
        wants_long: false,
        stop: f64::NAN,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let close = bar.close;
        let distance = self.atr.next(bar)[0] * STOP_IN_ATRS;
        let average = self.average.next_value(close);

        let mut signal = self.cross.step(close, average);
        match signal {
            Signal::Long => {
                self.wants_long = true;
                self.stop = close - distance;
            }
            Signal::Short => {
                self.wants_long = false;
                self.stop = close + distance;
            }
            _ => {}
        }

        // A cross just above set the stop at this close, so neither test can overrule its entry.
        if self.wants_long && self.held > 0 {
            // `f64::max` and `f64::min` answer the number when the stop is still NaN.
            self.stop = self.stop.max(close - distance);
            if close < self.stop {
                signal = Signal::Close;
                self.wants_long = false;
            }
        }
        if !self.wants_long && self.held < 0 {
            self.stop = self.stop.min(close + distance);
            if close > self.stop {
                signal = Signal::Close;
            }
        }

        match signal {
            Signal::Long => self.held = 1,
            Signal::Short => self.held = -1,
            Signal::Close => self.held = 0,
            Signal::Hold => {}
        }
        signal
    }
}
