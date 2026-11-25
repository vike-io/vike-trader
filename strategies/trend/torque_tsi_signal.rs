// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-torque-tsi-signal-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat and the true strength index crosses up through its Wilder average of 9, above zero, with the close above
//! EMA(68). Closed when it crosses down or the close falls below the EMA. The index smooths the close-to-close change with an
//! EMA(29) first and an EMA(14) second. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Ema, Smma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const LEAST_STRENGTH: f64 = 0.0;
/// The `tsi` row answers in percent; the rule's strength runs from -1 to 1.
const PERCENT: f64 = 100.0;

struct S {
    strength: IndicatorLine,
    average: Smma,
    trend: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        // The `tsi` row's parameters are `long`, the first smoothing, then `short`, the second.
        strength: IndicatorLine::new("tsi", &[29.0, 14.0]),
        // The average seeds from the first defined values: the index's warm-up NaNs are skipped, not pushed.
        average: Smma::new(9.0).expect("a valid period").with_skip_nan(true),
        trend: Ema::new(68.0).expect("a valid period"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = self.trend.next_value(bar.close);
        let strength = self.strength.next(bar)[0] / PERCENT;
        let average = self.average.next_value(strength);
        let crossed = self.cross.step(strength, average);
        let held = strategy.position();
        let enter =
            matches!(crossed, Signal::Long) && strength > LEAST_STRENGTH && bar.close > trend;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (matches!(crossed, Signal::Short) || bar.close < trend) {
            strategy.close(NAME);
        }
    }
}
