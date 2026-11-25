// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Crypto Crew University" by Bangsoul, https://www.tradingview.com/script/pVHd5BHO/
// The original's notice: © BTA crypto
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when MFI(14), the RSI(14) of the typical price, and the stochastic RSI's %K and %D (RSI(14) of the close, its 14-bar
//! stochastic smoothed by SMA(3), then SMA(3) again) are all below 20; short 1 when all four are above 80, each reversing the
//! other. A signal on the side already held stops it 20% from the average cost; a signal while flat or on the other side sets
//! no stop, the cost being unknown or the other side's. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Mfi, Rsi, Stochastic};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const LOW: f64 = 20.0;
const HIGH: f64 = 80.0;
const STOP: f64 = 0.2;

struct S {
    mfi: Mfi,
    typical_rsi: Rsi,
    rsi: Rsi,
    stoch: Stochastic,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        mfi: Mfi::new(14.0).expect("a valid length"),
        typical_rsi: Rsi::new(14.0).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        stoch: Stochastic::new(14.0, 3.0, 3.0).expect("valid lengths"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let mfi = self.mfi.next(bar);
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        let typical_rsi = self.typical_rsi.next(&Bar { close: typical, ..bar.clone() });
        let rsi = self.rsi.next(bar);
        let [k, d] = self.stoch.next_hlc(rsi, rsi, rsi);
        let all = [mfi, typical_rsi, k, d];
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if all.iter().all(|&v| v < LOW) {
            strategy.entry(UP, Long).size(1.0);
            if held > 0.0 {
                strategy.exit("long stop").from(UP).stop(cost * (1.0 - STOP));
            }
        }
        if all.iter().all(|&v| v > HIGH) {
            strategy.entry(DOWN, Short).size(1.0);
            if held < 0.0 {
                strategy.exit("short stop").from(DOWN).stop(cost * (1.0 + STOP));
            }
        }
    }
}
