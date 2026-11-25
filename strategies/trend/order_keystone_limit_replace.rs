// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-keystone-limit-replace-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat, EMA(48) rising and the close above it, a buy limit rests 0.55 ATR(18) under that EMA, moved every bar and pulled
//! when the trend fails. It takes 5% of the account's worth, sized from that level. While held, a stop 2.3 ATR under and a target
//! 2 ATR over the average cost, moved every bar. Costs: 0.05% commission, 1 tick. Starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "pullback";
const SHARE: f64 = 0.05;
const PULLBACK_ATRS: f64 = 0.55;
const STOP_ATRS: f64 = 2.3;
const TARGET_ATRS: f64 = 2.0;

struct KeystoneLimit {
    trend: Ema,
    atr: Atr,
    previous_trend: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(KeystoneLimit {
        trend: Ema::new(48.0).expect("a valid period"),
        atr: Atr::new(18.0).expect("a valid period"),
        previous_trend: f64::NAN,
    })
}

impl Rule for KeystoneLimit {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let trend = self.trend.next(bar);
        let previous_trend = std::mem::replace(&mut self.previous_trend, trend);
        let atr = self.atr.next(bar);
        let held = strategy.position();
        if held == 0.0 && trend > previous_trend && bar.close > trend {
            let level = trend - atr * PULLBACK_ATRS;
            let size = sizing.size_for_share(SHARE, strategy.snap_limit(Long, level));
            strategy.entry(NAME, Long).size(size).limit(level);
        } else if held == 0.0 {
            strategy.cancel(NAME);
        }
        if held > 0.0 {
            let cost = strategy.avg_cost();
            strategy
                .exit("risk")
                .from(NAME)
                .take(cost + atr * TARGET_ATRS)
                .stop(cost - atr * STOP_ATRS);
        }
    }
}
