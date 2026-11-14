// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-orbit-trend-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 6% of the account's worth when the 15-minute EMA(13) crosses up through EMA(34) of closed UTC hours. While held, closed when
//! it crosses back down, and a stop 2.2 ATR(16) under the average cost. 0.05% commission, 1 tick. Starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeBars;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "long";
const SHARE: f64 = 0.06;
const STOP_ATRS: f64 = 2.2;

struct S {
    hours: HigherTimeframeBars,
    hourly: IndicatorLine,
    // The last closed hour's value: it moves only on the bar that closes one.
    higher: f64,
    local: IndicatorLine,
    atr: IndicatorLine,
    rising: Cross,
    falling: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        hours: HigherTimeframeBars::minutes(60),
        hourly: IndicatorLine::new("ema", &[34.0]),
        higher: f64::NAN,
        local: IndicatorLine::new("ema", &[13.0]),
        atr: IndicatorLine::new("atr", &[16.0]),
        rising: Cross::new(),
        falling: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        if let Some(hour) = self.hours.push(bar) {
            self.higher = self.hourly.next(&hour)[0];
        }
        let local = self.local.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        if matches!(self.rising.step(local, self.higher), Signal::Long) {
            let size = sizing.size_for_share(SHARE, sizing.slipped(Long, bar.close));
            strategy.entry(NAME, Long).size(size);
        }
        if strategy.position() > 0.0 {
            // This cross sits behind the position test: it only remembers the bars spent holding.
            if matches!(self.falling.step(local, self.higher), Signal::Short) {
                strategy.close(NAME);
            }
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("stop").from(NAME).stop(cut);
        }
    }
}
