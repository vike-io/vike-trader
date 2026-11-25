// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-cci-threshold-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the CCI(20) of the close crosses up through -100, short when it crosses down through 100. A long also closes
//! when CCI crosses down through 0, a short when it crosses up through 0, and an entry on that bar wins over the close.
//!
//! Each zero cross only looks at bars on which its side is held, so it compares against the value of the last such bar,
//! however long ago, not the previous bar. That is what closes many trades one bar after they open.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Cci;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, CrossBands, Signal, SignalRule, SignalSource};

struct S {
    cci: Cci,
    bands: CrossBands,
    long_exit: Cross,
    short_exit: Cross,
    /// The position this bar opened with: an order sent on a bar fills at the next bar's open.
    held: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        cci: Cci::new(20.0).expect("a valid period"),
        bands: CrossBands::new(-100.0, 100.0),
        long_exit: Cross::new(),
        short_exit: Cross::new(),
        held: 0,
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // vike's CCI reads the typical price, which is the close when all three inputs are the close.
        let cci = self.cci.next_hlc(bar.close, bar.close, bar.close);
        let entry = self.bands.step(cci);
        let close = (self.held > 0 && matches!(self.long_exit.step(cci, 0.0), Signal::Short))
            || (self.held < 0 && matches!(self.short_exit.step(cci, 0.0), Signal::Long));
        let signal = match entry {
            Signal::Hold if close => Signal::Close,
            other => other,
        };
        match signal {
            Signal::Long => self.held = 1,
            Signal::Short => self.held = -1,
            Signal::Close => self.held = 0,
            Signal::Hold => {}
        }
        signal
    }
}
