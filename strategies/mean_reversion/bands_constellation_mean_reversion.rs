// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/bands-constellation-mean-reversion-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 5% of the account's worth when the close crosses up through the lower Bollinger band (31, 2.15) while under the middle band.
//! While held, moved on every bar: a target 85% of the way from the average cost to the middle band, and a stop 1.35 standard
//! deviations(31) under the average cost. 0.05% commission, 1 tick. Starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.05;
const LENGTH: f64 = 31.0;
const WIDTH: f64 = 2.15;
const STOP_DEVIATIONS: f64 = 1.35;
const TARGET_WAY: f64 = 0.85;

struct S {
    bands: IndicatorLine,
    deviation: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bands: IndicatorLine::new("bollinger", &[LENGTH, WIDTH]),
        deviation: IndicatorLine::new("stddev", &[LENGTH]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        // The `bollinger` row's lines are upper, mid, lower.
        let bands = self.bands.next(bar);
        let (middle, lower) = (bands[1], bands[2]);
        let deviation = self.deviation.next(bar)[0];
        let back_inside = matches!(self.cross.step(bar.close, lower), Signal::Long);
        if back_inside && bar.close < middle {
            let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
            strategy.entry(NAME, Long).size(size);
        }
        if strategy.position() > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("risk")
                .from(NAME)
                .take(cost + (middle - cost) * TARGET_WAY)
                .stop(cost - deviation * STOP_DEVIATIONS);
        }
    }
}
