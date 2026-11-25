// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-percentrank-mean-reversion-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Rank each bar's close-to-close change against the 50 changes before it. Long when the rank is under 10 and rising, short
//! when it is over 90 and falling. A long closes once the rank is above 50, a short once it is below.
//!
//! The rank is the percent of the previous `LENGTH` values less than or equal to the current one, vike's `PercentRank` but
//! for `SAME_CHANGE`: two changes within 1e-10 count as equal, so the rank is written here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::VecDeque;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

use vike_marketdata::Bar;
use vike_model::params::Params;

const LENGTH: usize = 50;
/// Two changes equal in cents can differ in their last bits (-5.35 arrives as -5.350000000000136 and -5.349999999999909),
/// so they count as equal. Real changes differ by at least a cent.
const SAME_CHANGE: f64 = 1e-10;

struct S {
    previous_close: f64,
    changes: VecDeque<f64>,
    previous_rank: f64,
    // The position these rules asked for: a signal sees no broker. Whatever it says before trading starts, the first
    // real entry overwrites it, and only the exits read it.
    held: i8,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        previous_close: f64::NAN,
        changes: VecDeque::with_capacity(LENGTH + 1),
        previous_rank: f64::NAN,
        held: 0,
    }))
}

impl S {
    /// `NaN` until `LENGTH` earlier changes are in.
    fn rank(&mut self, change: f64) -> f64 {
        if change.is_nan() {
            return f64::NAN;
        }
        let rank = if self.changes.len() == LENGTH {
            let below = self.changes.iter().filter(|&&old| old <= change + SAME_CHANGE).count();
            100.0 * below as f64 / LENGTH as f64
        } else {
            f64::NAN
        };
        if self.changes.len() == LENGTH {
            self.changes.pop_front();
        }
        self.changes.push_back(change);
        rank
    }
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let previous_close = std::mem::replace(&mut self.previous_close, bar.close);
        let rank = self.rank(bar.close - previous_close);
        let previous_rank = std::mem::replace(&mut self.previous_rank, rank);

        let long = rank < 10.0 && rank > previous_rank;
        let short = rank > 90.0 && rank < previous_rank;
        let exit = (self.held > 0 && rank > 50.0) || (self.held < 0 && rank < 50.0);

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
