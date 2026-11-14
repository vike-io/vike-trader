// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Pivot Point SuperTrend [Backtest]" by LonesomeTheBlue, https://www.tradingview.com/script/DwdC6FT4/
// The original's notice: © LonesomeTheBlue
// Changed: rewritten by hand in Rust for vike.

//! A centre line from the pivots (2 bars a side): the first pivot sets it, each later one moves it a third of the way there, a
//! high taken before a low on a bar that confirms both. Bands 3 ATR(10) either side of it ratchet as a supertrend does: the
//! lower one only rises while the previous close stays above its previous value, the upper one only falls while the previous
//! close stays below. The trend turns up when the close passes the previous upper band and down when it falls under the
//! previous lower band: vike's `Supertrend` judged against the last bar's bands (`Supertrend::with_previous_bands`) about the
//! centre line (`Supertrend::next_centred`). Each turn closes everything and enters 1 the new way. No costs; starts with 100,000.
//!
//! The rule's half close at the centre line is off by default, and so is long-only.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Supertrend};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FACTOR: f64 = 3.0;

struct S {
    pivots: ConfirmedPivot,
    atr: IndicatorLine,
    center: f64,
    line: Supertrend,
    trend: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(2.0, 2.0).expect("valid spans"),
        atr: IndicatorLine::new("atr", &[10.0]),
        center: f64::NAN,
        line: Supertrend::new(10.0, FACTOR).expect("valid parameters").with_previous_bands(true),
        trend: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [high, low, _, _] = self.pivots.next(bar);
        let atr = self.atr.next(bar)[0];
        // A pivot of exactly 0 counts as none, as the rule's truth test makes it.
        let pivot = [high, low].into_iter().find(|p| !p.is_nan() && *p != 0.0);
        if let Some(p) = pivot {
            self.center = if self.center.is_nan() { p } else { (self.center * 2.0 + p) / 3.0 };
        }
        let last_trend = self.trend;
        self.trend =
            if self.line.next_centred(bar.close, self.center, atr)[1] > 0.0 { 1 } else { -1 };
        if last_trend != 0 && self.trend != last_trend {
            strategy.close_all();
            let side = if self.trend > 0 { Long } else { Short };
            strategy.entry(if side == Long { "buy" } else { "sell" }, side).size(1.0);
        }
    }
}
