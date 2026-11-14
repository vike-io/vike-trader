// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-triple-tf-macd-hist-confluence-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the MACD(12, 26, 9) histogram of closed UTC hours, of closed UTC 4-hour bars and of closed UTC days are all above zero,
//! short when all three are below. Each histogram holds its value until its next higher bar closes.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Macd;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct Histogram {
    bars: HigherTimeframeBars,
    macd: Macd,
    value: f64,
}

impl Histogram {
    fn new(minutes: i64) -> Self {
        Histogram {
            bars: HigherTimeframeBars::minutes(minutes),
            macd: Macd::new(12.0, 26.0, 9.0).expect("valid periods"),
            value: f64::NAN,
        }
    }
    fn step(&mut self, bar: &Bar) -> f64 {
        if let Some(closed) = self.bars.push(bar) {
            self.value = self.macd.next(&closed)[2];
        }
        self.value
    }
}

struct S {
    histograms: [Histogram; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { histograms: [60, 240, 1_440].map(Histogram::new) }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let values = [
            self.histograms[0].step(bar),
            self.histograms[1].step(bar),
            self.histograms[2].step(bar),
        ];
        if values.iter().all(|v| *v > 0.0) {
            Signal::Long
        } else if values.iter().all(|v| *v < 0.0) {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
