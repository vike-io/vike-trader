// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Pivot Reversal Strategy + alerts via TradingConnector to indices" by Peter_O, https://www.tradingview.com/script/PIWdkQtS/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-09-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A pivot high with 4 bars before it and 2 after arms a long 1 resting as a stop a tick above it; the stop is issued again on
//! every bar until a bar's high passes the pivot, and a newer pivot high moves it. Pivot lows arm a short 1 the mirror way, a
//! tick below. Either entry reverses the other side. No costs.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "PivRevLE";
const DOWN: &str = "PivRevSE";

struct S {
    pivots: ConfirmedPivot,
    high: f64,
    low: f64,
    long_armed: bool,
    short_armed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(4.0, 2.0).expect("valid spans"),
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
        let [pivot_high, pivot_low, _, _] = self.pivots.next(bar);
        if !pivot_high.is_nan() {
            self.high = pivot_high;
            self.long_armed = true;
        } else if self.long_armed && bar.high > self.high {
            self.long_armed = false;
        }
        if self.long_armed {
            strategy.entry(UP, Long).size(1.0).stop(self.high + tick);
        }
        if !pivot_low.is_nan() {
            self.low = pivot_low;
            self.short_armed = true;
        } else if self.short_armed && bar.low < self.low {
            self.short_armed = false;
        }
        if self.short_armed {
            strategy.entry(DOWN, Short).size(1.0).stop(self.low - tick);
        }
    }
}
