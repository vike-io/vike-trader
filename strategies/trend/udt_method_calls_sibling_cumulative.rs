// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/udt-method-calls-sibling-cumulative-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(9) crosses up through EMA(21) with the close more than 1 standard deviation above SMA(50), both over the last 50
//! closes, flat when it crosses down. The deviation used is the square root of its own square, and an unknown or zero one scores
//! 0. It never shorts.
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

struct Moments {
    latest: f64,
    centre: f64,
    spread: f64,
}

impl Moments {
    fn absorb(&mut self, latest: f64, centre: f64, spread: f64) {
        (self.latest, self.centre, self.spread) = (latest, centre, spread);
    }
    fn mean(&self) -> f64 {
        self.centre
    }
    fn variance(&self) -> f64 {
        self.spread * self.spread
    }
    fn standard_score(&self) -> f64 {
        let deviation = self.variance().sqrt();
        if deviation.is_nan() || deviation == 0.0 {
            0.0
        } else {
            (self.latest - self.mean()) / deviation
        }
    }
    fn absorb_and_score(&mut self, latest: f64, centre: f64, spread: f64) -> f64 {
        self.absorb(latest, centre, spread);
        self.standard_score()
    }
}

struct S {
    moments: Moments,
    centre: IndicatorLine,
    spread: IndicatorLine,
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        moments: Moments { latest: f64::NAN, centre: f64::NAN, spread: f64::NAN },
        centre: IndicatorLine::new("sma", &[50.0]),
        spread: IndicatorLine::new("stddev", &[50.0]),
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let centre = self.centre.next(bar)[0];
        let spread = self.spread.next(bar)[0];
        let score = self.moments.absorb_and_score(bar.close, centre, spread);
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        match self.cross.step(fast, slow) {
            Signal::Long if score > 1.0 => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
