// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-map-regime-threshold-lookup-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when EMA(20) is above SMA(50) and 10-bar momentum is above +1.5, short when EMA(20) is not above SMA(50) and momentum is
//! below -1.5. Each limit is read from a map keyed by the regime's name, and a key the map lacks would read as 0.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::HashMap;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

use vike_marketdata::Bar;
use vike_model::params::Params;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    momentum: IndicatorLine,
    limit_of: HashMap<String, f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        fast: IndicatorLine::new("ema", &[20.0]),
        slow: IndicatorLine::new("sma", &[50.0]),
        momentum: IndicatorLine::new("mom", &[10.0]),
        limit_of: HashMap::from([
            ("bullish_limit".to_string(), 1.5),
            ("bearish_limit".to_string(), -1.5),
        ]),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        // A NaN compares false, so a warming average reads as the bearish regime.
        let bullish = self.fast.next(bar)[0] > self.slow.next(bar)[0];
        let regime = if bullish { "bullish" } else { "bearish" };
        let limit = self.limit_of.get(&format!("{regime}_limit")).copied().unwrap_or(0.0);
        let momentum = self.momentum.next(bar)[0];
        if bullish && momentum > limit {
            Signal::Long
        } else if !bullish && momentum < limit {
            Signal::Short
        } else {
            Signal::Hold
        }
    }
}
