// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/na-deep-history-int-na-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! RSI(14) of a gated source: the close inside the 09:30 to 16:00 New York session; outside it, the in-session close from 499 bars
//! back, or the current close when that bar was out of session or does not exist. Long when flat on RSI crossing up through 30
//! inside the session, close a long on RSI crossing down through 70 at any time.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::helpers::us_minute;
use vike_tactics::signal::{CrossBands, Signal, SignalRule, SignalSource};

const SESSION_MINUTES: std::ops::Range<i64> = 9 * 60 + 30..16 * 60;
/// The bars from the one 499 back through this one.
const HISTORY: usize = 500;

struct S {
    /// The close of every bar in session, NaN for the others.
    gated: Vec<f64>,
    rsi: Rsi,
    levels: CrossBands,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        gated: Vec::new(),
        rsi: Rsi::new(14.0).expect("a valid period"),
        levels: CrossBands::new(30.0, 70.0),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let in_session = SESSION_MINUTES.contains(&us_minute(bar.timestamp, -5));
        self.gated.push(if in_session { bar.close } else { f64::NAN });

        let reached = self.gated.len().checked_sub(HISTORY).map_or(f64::NAN, |at| self.gated[at]);
        let close = match (in_session, reached.is_nan()) {
            (true, _) | (false, true) => bar.close,
            (false, false) => reached,
        };
        let value = self.rsi.next(&Bar { close, ..bar.clone() });

        match self.levels.step(value) {
            Signal::Long if in_session => Signal::Long,
            Signal::Short => Signal::Close,
            _ => Signal::Hold,
        }
    }
}
