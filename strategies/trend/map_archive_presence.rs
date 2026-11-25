// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/map-archive-presence-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the chosen regime is in the table of RSI limits, RSI(15) crosses up through its limit and the close is above
//! EMA(46). Closed when RSI(15) is more than 9 below the limit, the close is below the EMA, or the close is more than 2.3 ATR(18)
//! under the average cost. A regime the table lacks reads a fallback limit and never enters. 0.05% commission, 1 tick.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use std::collections::HashMap;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const REGIME: &str = "balanced";
const FALLBACK_LIMIT: f64 = 63.0;
const LEAVE_UNDER_LIMIT: f64 = 9.0;
const MOST_ATRS_AGAINST: f64 = 2.3;

struct S {
    limits: HashMap<&'static str, f64>,
    rsi: IndicatorLine,
    trend: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        limits: HashMap::from([("patient", 61.5), ("balanced", 56.5), ("responsive", 52.0)]),
        rsi: IndicatorLine::new("rsi", &[15.0]),
        trend: IndicatorLine::new("ema", &[46.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let found = self.limits.get(REGIME).copied();
        let limit = found.unwrap_or(FALLBACK_LIMIT);
        let rsi = self.rsi.next(bar)[0];
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        // The cross sits behind the table test: it only remembers the bars that test passed.
        let enter = found.is_some()
            && matches!(self.cross.step(rsi, limit), Signal::Long)
            && bar.close > trend;
        let ended = rsi < limit - LEAVE_UNDER_LIMIT || bar.close < trend;
        let held = strategy.position();
        let too_far = held > 0.0 && bar.close < strategy.avg_cost() - atr * MOST_ATRS_AGAINST;
        if held == 0.0 && enter {
            strategy.entry(NAME, Long).size(SIZE);
        } else if held > 0.0 && (ended || too_far) {
            strategy.close(NAME);
        }
    }
}
