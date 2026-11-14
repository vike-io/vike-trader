// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-nvi-pvi-cross-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the positive volume index crosses up through its EMA(255), short when the negative volume index crosses down
//! through its own EMA(255). The opposite crossing of either index does nothing.
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
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    positive: IndicatorLine,
    negative: IndicatorLine,
    positive_average: Ema,
    negative_average: Ema,
    positive_cross: Cross,
    negative_cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        positive: IndicatorLine::new("pvi", &[]),
        negative: IndicatorLine::new("nvi", &[]),
        positive_average: Ema::new(255.0).expect("a valid period"),
        negative_average: Ema::new(255.0).expect("a valid period"),
        positive_cross: Cross::new(),
        negative_cross: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let positive = self.positive.next(bar)[0];
        let negative = self.negative.next(bar)[0];
        let positive_average = self.positive_average.next_value(positive);
        let negative_average = self.negative_average.next_value(negative);
        let up = self.positive_cross.step(positive, positive_average);
        let down = self.negative_cross.step(negative, negative_average);

        // Both on one bar: the short is asked for last, so it is the position left standing.
        match (up, down) {
            (_, Signal::Short) => Signal::Short,
            (Signal::Long, _) => Signal::Long,
            _ => Signal::Hold,
        }
    }
}
