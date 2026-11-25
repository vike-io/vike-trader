// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Tristan's Multi-Indicator Reversal Strategy" by OhRayOhRay, https://www.tradingview.com/script/1wc9v4rw/
// The original's notice: © OhRayOhRay
// Changed: rewritten by hand in Rust for vike.

//! Four reversal readings, counted each way: RSI(14) under 30 and rising, MACD(12, 26, 9)'s histogram above 0, Williams %R(14)
//! under -80 and rising, and this close or the last under the lower Bollinger band (20, 2) make the bullish count; RSI over
//! 70 and falling, the histogram under 0, %R over -20 and falling, and a close over the upper band the bearish one. Long all
//! of the account's worth when two or more are bullish, short when two or more are bearish, each reversing the other, only
//! on bars opening from 09:30 to 15:30 UTC. No costs; starts with 100,000.
//!
//! The rule allows ten entries a side, but each would be the whole account again, which a margin of 100% refuses: one lot a
//! side holds the same. The histogram is above 0 whenever MACD crosses up through its signal, so that cross adds nothing.
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

struct S {
    rsi: Rsi,
    macd: Macd,
    wr: Williams,
    bb: Bollinger,
    previous_rsi: f64,
    previous_wr: f64,
    previous_close: f64,
    previous_bands: [f64; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        wr: Williams::new(14.0).expect("a valid length"),
        bb: Bollinger::new(20.0, 2.0).expect("valid settings"),
        previous_rsi: f64::NAN,
        previous_wr: f64::NAN,
        previous_close: f64::NAN,
        previous_bands: [f64::NAN; 3],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let rsi = self.rsi.next(bar);
        let [_, _, hist] = self.macd.next(bar);
        let wr = self.wr.next(bar);
        let bands @ [upper, _, lower] = self.bb.next(bar);
        let [previous_upper, _, previous_lower] = self.previous_bands;
        let bullish = [
            rsi < 30.0 && rsi > self.previous_rsi,
            hist > 0.0,
            wr < -80.0 && wr > self.previous_wr,
            close < lower || self.previous_close < previous_lower,
        ];
        let bearish = [
            rsi > 70.0 && rsi < self.previous_rsi,
            hist < 0.0,
            wr > -20.0 && wr < self.previous_wr,
            close > upper || self.previous_close > previous_upper,
        ];
        (self.previous_rsi, self.previous_wr, self.previous_close, self.previous_bands) =
            (rsi, wr, close, bands);
        let (hour, minute) = clock(bar);
        let minutes = hour * 60 + minute;
        if !(570..960).contains(&minutes) {
            return;
        }
        let count = |v: [bool; 4]| v.iter().filter(|&&b| b).count();
        if count(bullish) >= 2 {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, close));
        }
        if count(bearish) >= 2 {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 100.0, close));
        }
    }
}
