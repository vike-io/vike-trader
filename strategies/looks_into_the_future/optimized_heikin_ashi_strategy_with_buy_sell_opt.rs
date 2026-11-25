// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Optimized Heikin Ashi Strategy with Buy/Sell Options and Alerts" by markontie, https://www.tradingview.com/script/f4SEwIH3/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-09-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Daily candles smoothed by a 2-bar SMA of each daily open and close. Long all of the account's worth when the smoothed candle
//! is up (close over open) and flat; the long closes when the candle is down. Only on the bars from 2023-01-01 to 2024-01-01. A
//! stop 2% under the position's average price guards it from the bar after it opens. No costs; starts with 1.
//!
//! **It looks into the future.** The rule reads the day with lookahead on, so every chart bar of a day sees that day's FINAL
//! open and close, which no live run can know (`vike_tactics::higher::HigherTimeframeSourceAhead`, over the run's whole daily
//! series, its history before the window included). So it runs in a backtest only, and its backtest is better than reality.
//!
//! The rule also asks for a target 4% over, as a second exit of the same entry. The reference never fills it: the first exit
//! (the stop) is handed the entry's whole quantity and the second is left none. Taken as it ran, the row matches (73 trades,
//! 7.450178); with the target as well, 153 trades and 5.62.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = "looks into the future: reads the day in progress at its final values; a backtest is better than reality"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::higher::HigherTimeframeSourceAhead;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

struct S {
    days: HigherTimeframeSourceAhead,
    /// The 2-bar SMAs of the daily open and close, and their last values.
    open: Sma,
    close: Sma,
    last: (f64, f64),
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceAhead::new(HigherTimeframePeriod::Day),
        open: Sma::new(2.0).expect("a valid length"),
        close: Sma::new(2.0).expect("a valid length"),
        last: (f64::NAN, f64::NAN),
        from: timestamp(2023, 1, 1, 0, 0),
        to: timestamp(2024, 1, 1, 0, 0),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![self.days.input().to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for day in self.days.next(bar, strategy) {
            self.last = (self.open.next_value(day.open), self.close.next_value(day.close));
        }
        let (open, close) = self.last;
        let held = strategy.position();
        if bar.timestamp >= self.from && bar.timestamp <= self.to {
            if close > open && held <= 0.0 {
                strategy.entry("Buy", Long).size(share_of_worth(&sizing, 100.0, bar.close));
            }
            if close < open && held > 0.0 {
                strategy.close("Buy");
            }
        }
        if held > 0.0 {
            let cut = strategy.avg_cost() * (1.0 - 2.0 / 100.0);
            strategy.exit("Exit").from("Buy").stop(cut);
        }
    }
}
