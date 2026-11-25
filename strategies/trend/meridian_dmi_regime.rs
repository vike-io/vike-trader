// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-meridian-dmi-regime-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when +DI(14) crosses up through -DI(14) with ADX above 19 and the close above EMA(48). Closed when +DI crosses down, ADX
//! is below 14 or the close is below the EMA. While held, a stop 2.2 ATR(18) under the average cost. 0.05% commission, 1 tick.
//!
//! The directional lines are vike's `adx` with the ADX smoothed by 12 apart from the DI lines' 14 (`with_adx_period`; unset, one
//! period for both).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Adx;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const DI_LENGTH: f64 = 14.0;
const ADX_SMOOTHING: f64 = 12.0;
const ENTRY_STRENGTH: f64 = 19.0;
const EXIT_STRENGTH: f64 = 14.0;
const STOP_ATRS: f64 = 2.2;

struct S {
    directional: Adx,
    trend: IndicatorLine,
    atr: IndicatorLine,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        directional: Adx::new(DI_LENGTH)
            .expect("a valid period")
            .with_adx_period(ADX_SMOOTHING)
            .expect("a valid smoothing"),
        trend: IndicatorLine::new("ema", &[48.0]),
        atr: IndicatorLine::new("atr", &[18.0]),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [adx, plus, minus] = self.directional.next(bar);
        let trend = self.trend.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let crossed = self.cross.step(plus, minus);
        if matches!(crossed, Signal::Long) && adx > ENTRY_STRENGTH && bar.close > trend {
            strategy.entry(NAME, Long).size(SIZE);
        }
        let held = strategy.position() > 0.0;
        let ended = matches!(crossed, Signal::Short) || adx < EXIT_STRENGTH || bar.close < trend;
        if held && ended {
            strategy.close(NAME);
        }
        if held {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
        }
    }
}
