// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-receives-ta-series-param-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through EMA(21) with RSI(14) between 40 and 90 inclusive, the close above that EMA and ATR(14)
//! above zero, flat when the close crosses down through it. It never shorts.
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

struct EntryGate {
    rsi_low: f64,
    rsi_high: f64,
    atr_low: f64,
}

impl EntryGate {
    fn admits(&self, rsi: f64, close: f64, ema: f64, atr: f64) -> bool {
        rsi >= self.rsi_low && rsi <= self.rsi_high && close > ema && atr > self.atr_low
    }
}

struct S {
    gate: EntryGate,
    rsi: IndicatorLine,
    ema: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        gate: EntryGate { rsi_low: 40.0, rsi_high: 90.0, atr_low: 0.0 },
        rsi: IndicatorLine::new("rsi", &[14.0]),
        ema: IndicatorLine::new("ema", &[21.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let ema = self.ema.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        match self.cross.step(bar.close, ema) {
            Signal::Long if self.gate.admits(rsi, bar.close, ema, atr) => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
