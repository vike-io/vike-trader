// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/composite-marketshift-integration-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! `pivot_state_machine`'s state, traded on its changes: long on the bar it turns bullish, or while bullish when the close crosses
//! up through the previous bar's 50-bar lowest low. Short on the bar it turns bearish, or while bearish when the close crosses down
//! through the previous bar's 50-bar highest high.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

struct S {
    state: PivotState,
    state_before: i32,
    channel: IndicatorLine,
    highest_before: f64,
    lowest_before: f64,
    over_lowest: Cross,
    under_highest: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        state: PivotState::new(),
        state_before: 0,
        channel: IndicatorLine::new("donchian", &[50.0]),
        highest_before: f64::NAN,
        lowest_before: f64::NAN,
        over_lowest: Cross::new(),
        under_highest: Cross::new(),
    }))
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let state = self.state.step(bar);
        let changed = state != std::mem::replace(&mut self.state_before, state);
        // The `donchian` row's lines are upper, mid, lower.
        let lines = self.channel.next(bar);
        let highest = std::mem::replace(&mut self.highest_before, lines[0]);
        let lowest = std::mem::replace(&mut self.lowest_before, lines[2]);
        // Both crosses step on every bar, or one skipped would compare against a stale previous value.
        let rebound = matches!(self.over_lowest.step(bar.close, lowest), Signal::Long);
        let fade = matches!(self.under_highest.step(bar.close, highest), Signal::Short);
        match state {
            1 if changed || rebound => Signal::Long,
            -1 if changed || fade => Signal::Short,
            _ => Signal::Hold,
        }
    }
}

/// The state alone, copied from `pivot_state_machine`.
struct PivotState {
    pivots: ConfirmedPivot,
    state: i32,
}

impl PivotState {
    fn new() -> Self {
        PivotState { pivots: ConfirmedPivot::new(5.0, 5.0).expect("valid spans"), state: 0 }
    }
    /// 1 bullish, -1 bearish, 0 before the first break.
    fn step(&mut self, bar: &Bar) -> i32 {
        // The newest pivot of each side: NaN until the first, and a NaN compares false.
        let [_, _, last_high, last_low] = self.pivots.next(bar);
        if bar.close > last_high {
            self.state = 1;
        } else if bar.close < last_low {
            self.state = -1;
        }
        self.state
    }
}
