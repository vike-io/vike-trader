// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend STRATEGY" by KivancOzbilgic, https://www.tradingview.com/script/P5Gu6F8k/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! Supertrend on the bar's midpoint with ATR(10) times 3: long 1 when the trend turns up, short 1 when it turns down, each
//! reversing the other. No costs; starts with 100,000.
//!
//! The lower band is `midpoint - 3 * ATR`, raised to last bar's band while last bar closed above that band; the upper band the
//! mirror. The trend starts up, turns up when the close goes above last bar's upper band and down when it goes below last
//! bar's lower band, each turn judged only against the band the trend would cross, and a band the last bar lacked read as
//! this bar's: vike's `Supertrend` with `Supertrend::with_previous_bands` and `Supertrend::with_gated_turns`. The rule's date
//! range (2018 to 9999) holds on every bar here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MULTIPLIER: f64 = 3.0;

struct S {
    supertrend: Supertrend,
    /// `1` up, `-1` down, `0` before the first bar.
    trend: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        supertrend: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true),
        trend: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = if self.supertrend.next(bar)[1] > 0.0 { 1 } else { -1 };
        if trend == 1 && self.trend == -1 {
            strategy.entry("BUY", Long).size(1.0);
        }
        if trend == -1 && self.trend == 1 {
            strategy.entry("SELL", Short).size(1.0);
        }
        self.trend = trend;
    }
}
