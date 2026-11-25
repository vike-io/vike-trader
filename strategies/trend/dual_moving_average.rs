// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Dual Moving Average" by sixpathssenin, https://www.tradingview.com/script/WEALnfiN/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-03-26, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when SMA(8) of the close crossed up through SMA(21) on any of the last 15 bars before this one and the slope of
//! SMA(21) is steeper than 7 degrees; short 1 when it crossed down on one of them and the slope is below -7 degrees, each
//! reversing the other. The slope is `atan((SMA(21) - SMA(21) two bars back) / ATR(10) / 2)` in degrees. No costs; starts
//! with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use std::f64::consts::PI;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const ANGLE: f64 = 7.0;
const SLOPE_BARS: usize = 2;
const LOOKBACK: usize = 15;

struct S {
    fast: Sma,
    slow: Sma,
    atr: Atr,
    /// The slow average of the last `SLOPE_BARS + 1` bars, newest last.
    slow_seen: VecDeque<f64>,
    cross: Cross,
    /// The cross of each of the last `LOOKBACK` bars, newest last.
    crosses: VecDeque<Signal>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Sma::new(8.0).expect("a valid length"),
        slow: Sma::new(21.0).expect("a valid length"),
        atr: Atr::new(10.0).expect("a valid length"),
        slow_seen: VecDeque::new(),
        cross: Cross::new(),
        crosses: VecDeque::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let atr = self.atr.next(bar);
        self.slow_seen.push_back(slow);
        if self.slow_seen.len() > SLOPE_BARS + 1 {
            self.slow_seen.pop_front();
        }
        let back = if self.slow_seen.len() > SLOPE_BARS { self.slow_seen[0] } else { f64::NAN };
        let angle = 180.0 / PI * libm::atan((slow - back) / atr / SLOPE_BARS as f64);
        let crossed_up = self.crosses.iter().any(|s| matches!(s, Signal::Long));
        let crossed_down = self.crosses.iter().any(|s| matches!(s, Signal::Short));
        self.crosses.push_back(self.cross.step(fast, slow));
        if self.crosses.len() > LOOKBACK {
            self.crosses.pop_front();
        }
        if crossed_up && angle > ANGLE {
            strategy.entry("long", Long).size(1.0);
        }
        if crossed_down && angle < -ANGLE {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
