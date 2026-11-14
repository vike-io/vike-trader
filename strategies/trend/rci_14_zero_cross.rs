// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-rci-14-zero-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the rank correlation of the last 14 closes against their order in time crosses up through 0, short when it
//! crosses down.
//!
//! The index is vike's `rank_correlation` giving tied closes their average rank (`with_mean_rank_ties`; unset, ties are
//! ranked by age, which moves the crossing on the bars with a tie in the window).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::RankCorrelation;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const LENGTH: f64 = 14.0;

struct S {
    index: RankCorrelation,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        index: RankCorrelation::new(LENGTH).expect("a valid period").with_mean_rank_ties(true),
        cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let index = self.index.next_value(bar.close);
        self.cross.step(index, 0.0)
    }
}
