// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Pivot Point Reversal + RSI Strategy" by QuantNomad, https://www.tradingview.com/script/3UcIwSrE/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-10-20, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Pivots of 3 bars on each side, on the highs and on the lows. From a pivot high until a high goes above it, every bar whose
//! RSI(14) of 3 bars back is under 70 places a buy stop one tick above it; from a pivot low until a low goes under it, every
//! bar whose RSI(14) of 3 bars back is over 30 places a sell stop one tick under it. Size 1, each reversing the other, and an
//! order not placed again rests as it was. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const BACK: usize = 3;

struct S {
    pivots: ConfirmedPivot,
    rsi: IndicatorLine,
    /// RSI, newest first, `BACK + 1` of them.
    rsis: VecDeque<f64>,
    high: f64,
    low: f64,
    long_armed: bool,
    short_armed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(3.0, 3.0).expect("valid spans"),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        rsis: VecDeque::with_capacity(BACK + 2),
        high: f64::NAN,
        low: f64::NAN,
        long_armed: false,
        short_armed: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let [high, low, _, _] = self.pivots.next(bar);
        if !high.is_nan() {
            (self.high, self.long_armed) = (high, true);
        } else if bar.high > self.high {
            self.long_armed = false;
        }
        if !low.is_nan() {
            (self.low, self.short_armed) = (low, true);
        } else if bar.low < self.low {
            self.short_armed = false;
        }
        self.rsis.push_front(self.rsi.next(bar)[0]);
        self.rsis.truncate(BACK + 1);
        let back = self.rsis.get(BACK).copied().unwrap_or(f64::NAN);
        if self.long_armed && back < 70.0 {
            strategy.entry("long", Long).size(1.0).stop(self.high + tick);
        }
        if self.short_armed && back > 30.0 {
            strategy.entry("short", Short).size(1.0).stop(self.low - tick);
        }
    }
}
