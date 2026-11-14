// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-vector-stop-breakout-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat and ATR(15) is under 0.82 of its 28-bar average, one buy stop rests 0.12 ATR above the highest high of the 28 bars
//! before this one, moved every bar and pulled when the contraction ends. It takes 5% of the account's worth, sized from that
//! level or from the close if that is higher. While held, a stop 2 ATR under the average cost, moved every bar, and a close at the
//! next open once the lot is 72 bars old. Costs: 0.05% commission, 1 tick. Starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::{Atr, Sma};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "breakout";
const SHARE: f64 = 0.05;
const CONTRACTION: f64 = 0.82;
const BUFFER_ATRS: f64 = 0.12;
const STOP_ATRS: f64 = 2.0;
const MAX_BARS: u32 = 72;

struct VectorBreakout {
    atr: Atr,
    atr_average: Sma,
    channel: IndicatorLine,
    previous_top: f64,
    previous_held: f64,
    /// Whole bars since the entry bar, `None` while flat.
    age: Option<u32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(VectorBreakout {
        atr: Atr::new(15.0).expect("a valid period"),
        atr_average: Sma::new(28.0).expect("a valid period"),
        channel: IndicatorLine::new("donchian", &[28.0]),
        previous_top: f64::NAN,
        previous_held: 0.0,
        age: None,
    })
}

impl Rule for VectorBreakout {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar);
        let average = if atr.is_nan() { f64::NAN } else { self.atr_average.next_value(atr) };
        // The `donchian` row's first line is the upper one, and this bar is in it, so the level reads the previous bar's.
        let top = std::mem::replace(&mut self.previous_top, self.channel.next(bar)[0]);

        let held = strategy.position();
        if held == 0.0 && atr < average * CONTRACTION {
            let level = top + atr * BUFFER_ATRS;
            let on_grid = strategy.snap_stop(Long, level);
            // A stop the close is already through sizes from that close, as a market order does.
            let size = sizing.size_for_share(SHARE, sizing.slipped(Long, on_grid.max(bar.close)));
            strategy.entry(NAME, Long).size(size).stop(level);
        } else if held == 0.0 {
            strategy.cancel(NAME);
        }

        self.age = match (held > 0.0, self.previous_held <= 0.0, self.age) {
            (false, _, _) => None,
            (true, true, _) => Some(0),
            (true, false, age) => age.map(|a| a + 1),
        };
        self.previous_held = held;

        if held > 0.0 {
            let cut = strategy.avg_cost() - atr * STOP_ATRS;
            strategy.exit("guard").from(NAME).stop(cut);
            if self.age.is_some_and(|a| a >= MAX_BARS) {
                strategy.close(NAME);
            }
        }
    }
}
