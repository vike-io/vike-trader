// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge contributors
// The script states no licence of its own: the source repository's LICENSE and LEGAL.md put it under Apache-2.0.
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/drawing-box-zone-reentry/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! A bar whose low is above the high two bars back adds a zone, from that high up to this low. Long while flat when the close is
//! inside the newest zone, including its edges. While long, a close above the newest zone goes flat and removes that zone, which
//! makes the zone before it the newest. Only the latest 20 zones are kept. It never shorts.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const MAX_ZONES: usize = 20;

struct Zone {
    top: f64,
    bottom: f64,
}

struct S {
    // The last two highs, oldest first, and NaN until two bars have passed.
    highs: [f64; 2],
    zones: Vec<Zone>,
    // The position these rules asked for: a signal sees no broker, and a market order fills on the next bar's open.
    long: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S { highs: [f64::NAN; 2], zones: Vec::new(), long: false }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let high_two_back = self.highs[0];
        self.highs = [self.highs[1], bar.high];
        if bar.low > high_two_back {
            self.zones.push(Zone { top: bar.low, bottom: high_two_back });
        }

        let mut signal = Signal::Hold;
        if let Some(zone) = self.zones.last() {
            if !self.long && bar.close <= zone.top && bar.close >= zone.bottom {
                self.long = true;
                signal = Signal::Long;
            } else if self.long && bar.close > zone.top {
                self.long = false;
                self.zones.pop();
                signal = Signal::Close;
            }
        }
        if self.zones.len() > MAX_ZONES {
            self.zones.remove(0);
        }
        signal
    }
}
