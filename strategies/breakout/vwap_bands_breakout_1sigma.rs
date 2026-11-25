// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/vwap-bands-breakout-1sigma-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the close crosses up through the band one standard deviation above the daily VWAP of the close, close when it
//! crosses down through that VWAP.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

/// Vike's `Vwap` of the close, restarted on each UTC day, and bands `width` of its volume-weighted standard deviation
/// (`Vwap::deviation`) either side.
struct DailyVwapBands {
    width: f64,
    vwap: Vwap,
}

impl DailyVwapBands {
    fn new(width: f64) -> Self {
        DailyVwapBands { width, vwap: Vwap::default().with_source(VwapSource::Close) }
    }

    /// The VWAP, its upper band and its lower band after this bar.
    fn next(&mut self, bar: &Bar) -> (f64, f64, f64) {
        // The bars carry float noise such as 52772.445999999996 for the venue's 0.001 volume grid. On a day's first bar the VWAP is
        // the close but for rounding, so that noise alone can decide a cross against it.
        let volume = (bar.volume * 1000.0).round() / 1000.0;
        let vwap = self.vwap.next_ts_hlcv(bar.timestamp, bar.high, bar.low, bar.close, volume);
        let reach = self.vwap.deviation() * self.width;
        (vwap, vwap + reach, vwap - reach)
    }
}

struct S {
    bands: DailyVwapBands,
    against_upper: Cross,
    against_vwap: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        bands: DailyVwapBands::new(1.0),
        against_upper: Cross::new(),
        against_vwap: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let (vwap, upper, _) = self.bands.next(bar);
        let breakout = self.against_upper.step(bar.close, upper);
        let fell_back = self.against_vwap.step(bar.close, vwap);
        match (breakout, fell_back) {
            (Signal::Long, _) => Signal::Long,
            (_, Signal::Short) => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
