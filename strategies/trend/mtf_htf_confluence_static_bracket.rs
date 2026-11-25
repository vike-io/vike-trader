// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/mtf-htf-confluence-static-bracket-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 1 when flat, EMA(9) crosses up through EMA(21) and the last closed UTC hour's close is above the EMA(20) of hourly closes. A
//! stop 2 ATR(14) under and a target 4 ATR above the signal bar's close are issued with the entry, once, and never moved. No costs.
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
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const HOUR_MINUTES: i64 = 60;
const STOP_ATRS: f64 = 2.0;
const TARGET_ATRS: f64 = 4.0;

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
    hours: HigherTimeframeBars,
    hour_average: IndicatorLine,
    // NaN until the first closed hour, and a NaN compares false.
    hour_close: f64,
    hour_level: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[9.0]),
        slow: IndicatorLine::new("ema", &[21.0]),
        atr: IndicatorLine::new("atr", &[14.0]),
        cross: Cross::new(),
        hours: HigherTimeframeBars::minutes(HOUR_MINUTES),
        hour_average: IndicatorLine::new("ema", &[20.0]),
        hour_close: f64::NAN,
        hour_level: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        if let Some(hour) = self.hours.push(bar) {
            self.hour_close = hour.close;
            self.hour_level = self.hour_average.next(&hour)[0];
        }
        let crossed_up = matches!(self.cross.step(fast, slow), Signal::Long);
        let confluent = self.hour_close > self.hour_level;
        if crossed_up && confluent && strategy.position() == 0.0 && !atr.is_nan() {
            strategy.entry(NAME, Long).size(1.0);
            strategy
                .exit("bracket")
                .from(NAME)
                .take(bar.close + atr * TARGET_ATRS)
                .stop(bar.close - atr * STOP_ATRS);
        }
    }
}
