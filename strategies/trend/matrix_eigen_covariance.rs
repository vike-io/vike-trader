// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/matrix-eigen-covariance-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! The 26-bar correlation of the one-bar percent change in price and in volume, taken as 0 while undefined, fills a 2 by 2 matrix
//! with ones on its diagonal. Long 2 when flat, the matrix's largest eigenvalue crosses up through 1.28 and the close is above
//! EMA(58). Closed when that eigenvalue is under 1.08, the close is under the EMA, or the close is more than 2.6 ATR(19) under the
//! average cost. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Roc;
use vike_indicators::pairs::Correl;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const ENTER_ABOVE: f64 = 1.28;
const LEAVE_BELOW: f64 = 1.08;
const MOST_ATRS_AGAINST: f64 = 2.6;

/// `[[1, c], [c, 1]]` has the eigenvalues `1 + c` and `1 - c`, so no iteration is needed.
fn largest_eigenvalue(c: f64) -> f64 {
    (1.0 + c).max(1.0 - c)
}

struct S {
    price_change: Roc,
    volume_change: Roc,
    correlation: Correl,
    trend: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        price_change: Roc::new(1.0).expect("a valid period"),
        volume_change: Roc::new(1.0).expect("a valid period"),
        correlation: Correl::with_params(&[26.0]),
        trend: IndicatorLine::new("ema", &[58.0]),
        atr: IndicatorLine::new("atr", &[19.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let price_change = self.price_change.next_value(bar.close);
        let volume_change = self.volume_change.next_value(bar.volume);
        let correlation = self.correlation.on_pair_values(price_change, volume_change);
        let largest = largest_eigenvalue(if correlation.is_nan() { 0.0 } else { correlation });
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed_up = matches!(self.cross.step(largest, ENTER_ABOVE), Signal::Long);
        let enter = crossed_up && bar.close > trend;
        let ended = largest < LEAVE_BELOW || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
