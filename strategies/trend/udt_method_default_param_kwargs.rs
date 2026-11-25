// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-default-param-kwargs-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21), the sum of three thresholds is above 80 and the second is above the first; flat
//! when it crosses down. A threshold is a base plus 1.5 times a multiplier: base 0 times 1, base 0 times ATR(14), base RSI(14)
//! times 2. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const FACTOR: f64 = 1.5;

fn threshold(multiplier: f64, base: f64) -> f64 {
    base + FACTOR * multiplier
}

struct S {
    rsi: IndicatorLine,
    atr: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];

        let plain = threshold(1.0, 0.0);
        let by_atr = threshold(atr, 0.0);
        let on_rsi = threshold(2.0, rsi);

        match self.cross.step(fast, slow) {
            Signal::Long if plain + by_atr + on_rsi > 80.0 && by_atr > plain => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
