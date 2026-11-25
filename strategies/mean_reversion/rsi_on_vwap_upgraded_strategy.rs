// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI on VWAP Upgraded strategy" by UnknownUnicorn4889556, https://www.tradingview.com/script/Q5TPHHLs/
// The original's notice: © Mysteriown
// Changed: rewritten by hand in Rust for vike.

//! RSI(20) of the day's VWAP of the close, its call on the chart's own timeframe and so the chart's value. Long 1 when it
//! crosses up through 30, adding up to 4; the longs closed when it crosses down through 85, or 28 bars after it crossed up
//! through 85. Short 1 when it crosses down through 85, up to 4, closed when it crosses up through 30 or 28 bars after it
//! crossed down through 30. Each side's entry reverses the other. 0.08% commission; starts with 100,000.
//!
//! The VWAP restarts on each UTC day: the sum of close times volume over the sum of volume, from the day's first bar (vike's
//! `Vwap` of the close).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Rsi, Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LATE: usize = 28;

struct S {
    vwap: Vwap,
    rsi: Rsi,
    low: Cross,
    high: Cross,
    /// Whether the RSI crossed up through 85, and down through 30, on each of the last `LATE + 1` bars, newest last.
    crossed: VecDeque<(bool, bool)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        vwap: Vwap::default().with_source(VwapSource::Close),
        rsi: Rsi::new(20.0).expect("a valid length"),
        low: Cross::new(),
        high: Cross::new(),
        crossed: VecDeque::with_capacity(LATE + 2),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.08), pyramiding: 4, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next_value(self.vwap.next(bar));
        let low = self.low.step(rsi, 30.0);
        let high = self.high.step(rsi, 85.0);
        self.crossed.push_back((matches!(high, Signal::Long), matches!(low, Signal::Short)));
        if self.crossed.len() > LATE + 1 {
            self.crossed.pop_front();
        }
        let late = if self.crossed.len() > LATE { self.crossed[0] } else { (false, false) };
        if matches!(low, Signal::Long) {
            strategy.entry("Long", Long).size(1.0);
        }
        if late.0 || matches!(high, Signal::Short) {
            strategy.close("Long");
        }
        if matches!(high, Signal::Short) {
            strategy.entry("Short", Short).size(1.0);
        }
        if late.1 || matches!(low, Signal::Long) {
            strategy.close("Short");
        }
    }
}
