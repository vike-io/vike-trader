// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Larry Connors Strategy [JoseMetal]" by JoseMetal, https://www.tradingview.com/script/H4ciolWP/
// The original's notice: © JoseMetal
// Changed: rewritten by hand in Rust for vike.

//! Larry Connors' RSI(2) pullback, 10% of the account's worth, entered only when flat. Long when the close is above SMA(200)
//! and not above SMA(10) and RSI(2) has just left the zone under 5 (under it a bar ago, not now); short when the close is not
//! above SMA(200) but above SMA(10) and RSI(2) has just left the zone over 95. A long closes when a close is above SMA(10), a
//! short when one is not. The rule's stop, target and break-even are off. No costs; starts with 10,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "Abrir Long";
const SHORT_NAME: &str = "Abrir Short";

struct S {
    exit_line: IndicatorLine,
    trend_line: IndicatorLine,
    rsi: IndicatorLine,
    last_rsi: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        exit_line: IndicatorLine::new("sma", &[10.0]),
        trend_line: IndicatorLine::new("sma", &[200.0]),
        rsi: IndicatorLine::new("rsi", &[2.0]),
        last_rsi: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let exit_line = self.exit_line.next(bar)[0];
        let trend_line = self.trend_line.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let last_rsi = std::mem::replace(&mut self.last_rsi, rsi);
        let above_exit = bar.close > exit_line;
        let uptrend = bar.close > trend_line;
        let held = strategy.position();
        if held > 0.0 && above_exit {
            strategy.close(LONG_NAME);
        }
        if held < 0.0 && !above_exit {
            strategy.close(SHORT_NAME);
        }
        if held != 0.0 {
            return;
        }
        // An RSI with a value a bar ago has one now, so "not under 5" is "at least 5".
        let left_low = last_rsi < 5.0 && rsi >= 5.0;
        let left_high = last_rsi > 95.0 && rsi <= 95.0;
        if uptrend && !above_exit && left_low {
            strategy.entry(LONG_NAME, Long).size(share_of_worth(&sizing, 10.0, bar.close));
        } else if !uptrend && above_exit && left_high {
            strategy.entry(SHORT_NAME, Short).size(share_of_worth(&sizing, 10.0, bar.close));
        }
    }
}
