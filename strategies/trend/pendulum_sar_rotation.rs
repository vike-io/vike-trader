// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/ta-pendulum-sar-rotation-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long 2 when flat, the close crosses up through the parabolic SAR(start 0.018, step 0.021, cap 0.19) and is above EMA(73).
//! Closed when, while held, the close crosses down through the SAR or is below the EMA, or 96 bars have passed since the signal bar.
//! 0.05% commission, 1 tick.
//!
//! The downward cross only remembers bars on which the long is held. A bar that enters on the same bar it first sees the book flat
//! forgets its entry bar straight away, so that trade has no age limit.
//!
//! The SAR is `sar_flip_entry`'s: vike's `psar` with its start apart from its step, tested for a reversal before the clamp.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Psar;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const ACCEL_START: f64 = 0.018;
const ACCEL_STEP: f64 = 0.021;
const ACCEL_CAP: f64 = 0.19;
const MOST_BARS_HELD: i64 = 96;

struct S {
    stop: Psar,
    trend: IndicatorLine,
    up: Cross,
    down: Cross,
    bar_number: i64,
    entered_on: Option<i64>,
    last_held: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stop: parabolic_sar(ACCEL_START, ACCEL_STEP, ACCEL_CAP),
        trend: IndicatorLine::new("ema", &[73.0]),
        up: Cross::new(),
        down: Cross::new(),
        bar_number: 0,
        entered_on: None,
        last_held: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.bar_number += 1;
        let stop = self.stop.next(bar);
        let trend = self.trend.next(bar)[0];
        let crossed_up = matches!(self.up.step(bar.close, stop), Signal::Long);
        let held = strategy.position();
        if held == 0.0 && crossed_up && bar.close > trend {
            strategy.entry(NAME, Long).size(SIZE);
            self.entered_on = Some(self.bar_number);
        }
        let aged =
            held > 0.0 && self.entered_on.is_some_and(|at| self.bar_number - at >= MOST_BARS_HELD);
        let rotated = held > 0.0
            && (matches!(self.down.step(bar.close, stop), Signal::Short) || bar.close < trend);
        if rotated || aged {
            strategy.close(NAME);
        }
        if held == 0.0 && self.last_held > 0.0 {
            self.entered_on = None;
        }
        self.last_held = held;
    }
}
