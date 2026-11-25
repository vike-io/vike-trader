// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Vegas SuperTrend Enhanced - strategy [presentTrading]" by PresentTrading, https://www.tradingview.com/script/ec57c7xL/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! A SuperTrend around the typical price whose multiplier widens with the 100-bar channel `SMA(100) ± stdev(100)`: `5 + 5 *
//! width / SMA`, times ATR(10). The trend turns up when the close passes over the last bar's upper band, down when it passes
//! under the last bar's lower band; in an up trend the lower band only rises, in a down trend the upper band only falls. Long
//! 10,000 worth while the trend is up, short 10,000 worth while it is down, each reversing the other; the bar the trend
//! turns also closes everything. 0.1% commission and 1 tick of slippage; starts with 10,000 in a USD account.
//!
//! The SuperTrend is vike's judged against the last bar's bands and carried on the trend (`Supertrend::with_previous_bands`,
//! `Supertrend::with_carry_on_trend`), fed this bar's multiplier (`Supertrend::next_centred_with_mult`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Sma, Stddev, Supertrend};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CASH: f64 = 10_000.0;
const BASE: f64 = 5.0;
const ADJUSTMENT: f64 = 5.0;

struct S {
    average: Sma,
    deviation: Stddev,
    atr: Atr,
    line: Supertrend,
    trend: i32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Sma::new(100.0).expect("a valid length"),
        deviation: Stddev::new(100.0).expect("a valid length"),
        atr: Atr::new(10.0).expect("a valid length"),
        line: Supertrend::default().with_previous_bands(true).with_carry_on_trend(true),
        trend: 1,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let average = self.average.next(bar);
        let sd = self.deviation.next(bar);
        let width = (average + sd) - (average - sd);
        let multiplier = BASE + ADJUSTMENT * (width / average);
        let atr = self.atr.next(bar);
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        let last = self.trend;
        let [_, direction] = self.line.next_centred_with_mult(bar.close, typical, atr, multiplier);
        self.trend = if direction > 0.0 { 1 } else { -1 };

        if self.trend == 1 {
            strategy.entry("long", Long).size(cash_worth(&sizing, CASH, bar.close));
        } else {
            strategy.entry("short", Short).size(cash_worth(&sizing, CASH, bar.close));
        }
        if self.trend != last {
            strategy.close_all();
        }
    }
}
