// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/session-borough-new-york-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat on the first bar of the 09:30 to 16:00 New York session, with the close above EMA(38) and RSI(14) above 52.
//! Closed on the first bar after the session. A bar belongs to the session by its open time. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const OPENS: i64 = 9 * 60 + 30;
const CLOSES: i64 = 16 * 60;
const LEAST_RSI: f64 = 52.0;

struct S {
    trend: IndicatorLine,
    rsi: IndicatorLine,
    was_inside: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        trend: IndicatorLine::new("ema", &[38.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        was_inside: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = self.trend.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let inside = (OPENS..CLOSES).contains(&us_minute(bar.timestamp, -5));
        let was_inside = std::mem::replace(&mut self.was_inside, inside);
        let held = strategy.position();
        if held == 0.0 && inside && !was_inside && bar.close > trend && rsi > LEAST_RSI {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && was_inside && !inside {
            strategy.close(NAME);
        }
    }
}
