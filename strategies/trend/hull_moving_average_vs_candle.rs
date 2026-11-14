// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Hull Moving Average vs Candle" by SeaSide420, https://www.tradingview.com/script/lc6FSl0Z/
// The original's notice: © SeaSide420. Any timeFrame/pair , Hull Moving Average vs Candle
// Changed: rewritten by hand in Rust for vike.

//! The open against HMA(50) of the open and the last closed day's open. Long all of the account's worth while the open is
//! above the Hull average, the average above the day's open and the open above the last one; short on the mirror, each
//! reversing the other. A long is closed once the open is under the average and the average under the day's open, a short
//! on the mirror. 1 per order and 1 tick of slippage; starts with 100,000.
//!
//! The day is the run's own symbol's daily bar, read once it has closed (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Hma;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleDefaultsFee, Tactics};

struct S {
    days: HigherTimeframeSourceCalendar,
    /// The last closed day's open.
    candle: f64,
    hma: Hma,
    last_open: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        candle: f64::NAN,
        hma: Hma::new(50.0).expect("a valid length"),
        last_open: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::PerOrder(1.0), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        if let Some(day) = self.days.next(bar, strategy.input(higher::DAY)).last() {
            self.candle = day.open;
        }
        let (price, candle) = (bar.open, self.candle);
        let hma = self.hma.next_value(price);
        let rising = price > self.last_open;
        let falling = price < self.last_open;
        self.last_open = price;
        if price < hma && hma < candle {
            strategy.close("BUY");
        }
        if price > hma && hma > candle {
            strategy.close("SELL");
        }
        if price > hma && hma > candle && rising {
            strategy.entry("BUY", Long).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Long, bar.close),
            ));
        }
        if price < hma && hma < candle && falling {
            strategy.entry("SELL", Short).size(share_of_worth(
                &sizing,
                100.0,
                sizing.slipped(Short, bar.close),
            ));
        }
    }
}
