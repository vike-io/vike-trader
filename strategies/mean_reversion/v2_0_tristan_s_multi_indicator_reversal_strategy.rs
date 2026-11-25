// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "v2.0—Tristan's Multi-Indicator Reversal Strategy" by OhRayOhRay, https://www.tradingview.com/script/MmfqVjSS/
// The original's notice: © OhRayOhRay
// Changed: rewritten by hand in Rust for vike.

//! Four reversal readings each way: RSI(14) below 30 and rising, the MACD (12, 26, 9) histogram above 0, Williams %R(14) below
//! -80 and rising, and this or the last close below the lower 20-bar Bollinger band (2 deviations); the bearish four mirror
//! them (above 70 and falling, below 0, above -20 and falling, above the upper band). With two or more bullish readings in the
//! bars opening 09:30-16:00 UTC, long all of the account's worth; with two or more bearish, short all of it; each reverses the
//! other. The rule's stop and target are off by default and stay off. No costs; starts with 100,000.
//!
//! The rule's MACD cross is read only when the histogram is not above 0 (its `or` stops at the first true side), where it can
//! never fire, so the histogram alone decides. Adds up to ten entries are allowed, but each is all of the account's worth and
//! the reference's 100% margin refuses it, so one lot is held.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Bollinger, Macd, Rsi, Williams};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";

struct S {
    rsi: Rsi,
    macd: Macd,
    williams: Williams,
    bands: Bollinger,
    last_rsi: f64,
    last_williams: f64,
    /// The last close against the last bar's bands: `(below lower, above upper)`.
    last_outside: (bool, bool),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        williams: Williams::new(14.0).expect("a valid length"),
        bands: Bollinger::new(20.0, 2.0).expect("valid parameters"),
        last_rsi: f64::NAN,
        last_williams: f64::NAN,
        last_outside: (false, false),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let [_, _, hist] = self.macd.next(bar);
        let wr = self.williams.next(bar);
        let [upper, _, lower] = self.bands.next(bar);
        let below = bar.close < lower;
        let above = bar.close > upper;
        let bullish = [
            rsi < 30.0 && rsi > self.last_rsi,
            hist > 0.0,
            wr < -80.0 && wr > self.last_williams,
            below || self.last_outside.0,
        ];
        let bearish = [
            rsi > 70.0 && rsi < self.last_rsi,
            hist < 0.0,
            wr > -20.0 && wr < self.last_williams,
            above || self.last_outside.1,
        ];
        (self.last_rsi, self.last_williams, self.last_outside) = (rsi, wr, (below, above));
        let (hour, minute) = clock(bar);
        let in_session = (570..960).contains(&(hour * 60 + minute));
        let count = |v: [bool; 4]| v.iter().filter(|&&b| b).count();
        if in_session && count(bullish) >= 2 {
            strategy.entry(UP, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if in_session && count(bearish) >= 2 {
            strategy.entry(DOWN, Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
