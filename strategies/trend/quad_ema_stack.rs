// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-4emarsi-quad-ema-stack-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the bar EMA(8) > EMA(21) > EMA(55) > EMA(200) becomes true, short on the bar the opposite order becomes true.
//!
//! The averages start cold, each one empty until its length is full and then the mean of those closes, which is vike's `Ema`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct S {
    fan: [Ema; 4],
    was_bullish: bool,
    was_bearish: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fan: [8.0, 21.0, 55.0, 200.0].map(|n| Ema::new(n).expect("a valid period")),
        was_bullish: false,
        was_bearish: false,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let values = self.fan.each_mut().map(|average| average.next(bar));
        // An average still empty compares false, so neither order holds during warm-up.
        let bullish = values.windows(2).all(|pair| pair[0] > pair[1]);
        let bearish = values.windows(2).all(|pair| pair[0] < pair[1]);
        let was_bullish = std::mem::replace(&mut self.was_bullish, bullish);
        let was_bearish = std::mem::replace(&mut self.was_bearish, bearish);
        if bullish && !was_bullish {
            Signal::Long
        } else if bearish && !was_bearish {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
