// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "INTELLECT_city - World Cycle - ATH & ATL - Timeframe 1D and 1W - Logarithmic - Strategy." by INTELLECT_city, https://www.tradingview.com/script/824WmIi7/
// The original's notice: © GooD @ABIX_GOOD
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-06-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Four daily lines of the close: twice SMA(350) and SMA(111), then 0.745 times SMA(471) and EMA(150). Short 1 when twice SMA(350)
//! crosses down through SMA(111); long 1 when 0.745 times SMA(471) crosses up through EMA(150), each reversing the other. The
//! rule reads the lines from the chart's own timeframe (a daily chart asking for the daily bar), so they are the chart's.
//! No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    long_ath: Sma,
    short_ath: Sma,
    long_atl: Sma,
    short_atl: Ema,
    ath: Cross,
    atl: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        long_ath: sma(350.0),
        short_ath: sma(111.0),
        long_atl: sma(471.0),
        short_atl: Ema::new(150.0).expect("a valid length"),
        ath: Cross::new(),
        atl: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (long_ath, short_ath) = (self.long_ath.next(bar) * 2.0, self.short_ath.next(bar));
        let (long_atl, short_atl) = (self.long_atl.next(bar) * 0.745, self.short_atl.next(bar));
        if matches!(self.ath.step(long_ath, short_ath), Signal::Short) {
            strategy.entry("Sell", Short).size(1.0);
        }
        if matches!(self.atl.step(long_atl, short_atl), Signal::Long) {
            strategy.entry("Buy", Long).size(1.0);
        }
    }
}
