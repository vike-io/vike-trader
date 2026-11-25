// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-bb-kc-squeeze-breakout-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Bollinger(20, 2) inside a Keltner channel latches a squeeze. On a bar with the latch set and a Bollinger band outside the
//! channel, go long if momentum is positive or short if it is negative, and clear the latch. A long closes when momentum goes
//! negative and a short when it goes positive; an entry on that bar wins. Momentum is the 20-bar regression line of the
//! close minus the Bollinger middle, read at the newest bar.
//!
//! The channel is vike's `keltner` with its band from an EMA of the true range (`with_range_ema`; unset, Wilder's average):
//! EMA(20) of the close, 1.5 EMA(20)s of the true range either side, and the true range has no value on the first bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Keltner, Linearreg};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const CHANNEL_WIDTH: f64 = 1.5;

struct S {
    bands: IndicatorLine,
    channel: Keltner,
    momentum: Linearreg,
    squeezed: bool,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        bands: IndicatorLine::new("bollinger", &[20.0, 2.0]),
        channel: Keltner::new(20.0, 20.0, CHANNEL_WIDTH)
            .expect("valid lengths")
            .with_range_ema(true),
        momentum: Linearreg::new(20.0).expect("a valid period"),
        squeezed: false,
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // The `bollinger` row's lines are upper, mid, lower.
        let bands = self.bands.next(bar);
        let (upper, mid, lower) = (bands[0], bands[1], bands[2]);
        let [channel_upper, _, channel_lower] = self.channel.next(bar);

        let momentum = self.momentum.next_value(bar.close - mid);

        if lower > channel_lower && upper < channel_upper {
            self.squeezed = true;
        }
        let fired = self.squeezed && (lower < channel_lower || upper > channel_upper);

        let long = fired && momentum > 0.0;
        let short = fired && momentum < 0.0;
        if long || short {
            self.squeezed = false;
        }
        let exit = (self.held > 0 && momentum < 0.0) || (self.held < 0 && momentum > 0.0);

        if long {
            self.held = 1;
            Signal::Long
        } else if short {
            self.held = -1;
            Signal::Short
        } else if exit {
            self.held = 0;
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
