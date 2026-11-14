// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 PineForge
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/order-harbor-stop-limit-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! While flat, and while the close sits above the middle of the 35 bars before this one and under the trigger, one buy
//! stop-limit rests: its trigger 0.10 ATR(14) above the highest high of those 35 bars, its limit 0.28 ATR above the trigger,
//! moved every bar and pulled when the setup ends. It takes 5% of the account's worth, sized bare from the trigger on the
//! grid (snapped as a limit, `Desk::add_lot_beyond_at`), the price a fill bounded by its limit pays. While held, a stop 2.4 ATR under the average cost, moved every bar, and a
//! close at the next open once the lot is 64 bars old. Costs: 0.05% commission, 1 tick. Starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Atr;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "breakout";
const SHARE: f64 = 0.05;
const RANGE_BARS: f64 = 35.0;
const TRIGGER_ATRS: f64 = 0.10;
const LIMIT_ATRS: f64 = 0.28;
const STOP_ATRS: f64 = 2.4;
const MAX_BARS: u32 = 64;

struct HarborStopLimit {
    atr: Atr,
    range: IndicatorLine,
    /// The previous bar's `donchian` lines: upper, mid, lower.
    previous_range: Vec<f64>,
    previous_held: f64,
    /// Whole bars since the entry bar, `None` while flat.
    age: Option<u32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(HarborStopLimit {
        atr: Atr::new(14.0).expect("a valid period"),
        range: IndicatorLine::new("donchian", &[RANGE_BARS]),
        previous_range: vec![f64::NAN; 3],
        previous_held: 0.0,
        age: None,
    })
}

impl Rule for HarborStopLimit {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let atr = self.atr.next(bar);
        // This bar is in the channel, so the range reads the previous bar's lines.
        let range = std::mem::replace(&mut self.previous_range, self.range.next(bar));
        let (top, bottom) = (range[0], range[2]);
        let trigger = top + atr * TRIGGER_ATRS;
        let limit = trigger + atr * LIMIT_ATRS;
        let setup = bar.close > (top + bottom) / 2.0 && bar.close < trigger;

        let held = strategy.position();
        if held == 0.0 && setup {
            // The order language snaps a stop-limit's trigger as a limit, so this is the price it rests and fills at.
            let size = sizing.size_for_share(SHARE, strategy.snap_limit(Long, trigger));
            strategy.entry(NAME, Long).size(size).stop(trigger).limit(limit);
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
