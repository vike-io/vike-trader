// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-4emarsi-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(8) > EMA(21) > EMA(55) > EMA(200) and RSI(14) crosses up through 52 having been below 48 since it last did;
//! short on the mirror, RSI crossing down through 48 having been above 52. Any position is closed after 8 bars.
//!
//! The ready flags and the bar count remember where the run began.
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
use vike_tactics::signal::{CrossBands, Signal, SignalRule, SignalSource};

const BELOW_ARMS_LONG: f64 = 48.0;
const ABOVE_ARMS_SHORT: f64 = 52.0;
const LONG_RECOVERY: f64 = 52.0;
const SHORT_RECOVERY: f64 = 48.0;
const CLOSE_AFTER_BARS: u32 = 8;

struct S {
    averages: [Ema; 4],
    rsi: IndicatorLine,
    recovery: CrossBands,
    long_ready: bool,
    short_ready: bool,
    // The side held on this bar, as this rule's own orders left it.
    held: i32,
    // Keeps counting through a reversal: only a bar with no position resets it.
    bars_held: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        averages: [8.0, 21.0, 55.0, 200.0].map(|n| Ema::new(n).expect("a valid period")),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        recovery: CrossBands::new(LONG_RECOVERY, SHORT_RECOVERY),
        long_ready: false,
        short_ready: false,
        held: 0,
        bars_held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let [a, b, c, d] = [0, 1, 2, 3].map(|i| self.averages[i].next(bar));
        let rsi = self.rsi.next(bar)[0];
        self.long_ready |= rsi < BELOW_ARMS_LONG;
        self.short_ready |= rsi > ABOVE_ARMS_SHORT;
        let crossed = self.recovery.step(rsi);
        let long_fires = self.long_ready && matches!(crossed, Signal::Long);
        let short_fires = self.short_ready && matches!(crossed, Signal::Short);
        // Firing spends the flag whether or not the averages allow the entry.
        self.long_ready &= !long_fires;
        self.short_ready &= !short_fires;

        self.bars_held = if self.held != 0 { self.bars_held + 1 } else { 0 };
        // shortcut: the close wins over an entry on the same bar, unproven; check it once a trade shows that case.
        if self.held != 0 && self.bars_held >= CLOSE_AFTER_BARS {
            self.held = 0;
            Signal::Close
        } else if long_fires && a > b && b > c && c > d && self.held <= 0 {
            self.held = 1;
            Signal::Long
        } else if short_fires && a < b && b < c && c < d && self.held >= 0 {
            self.held = -1;
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
