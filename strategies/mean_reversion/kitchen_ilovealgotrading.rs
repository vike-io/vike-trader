// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Kitchen [ilovealgotrading]" by projeadam, https://www.tradingview.com/script/T2juFbHt/
// The original's notice: © projeadam
// Changed: rewritten by hand in Rust for vike.

//! Two supertrends (factor 3, ATR 10), one on 1-hour bars and one on 4-hour bars, and a stochastic RSI on the 4-hour closes (RSI
//! 14, stochastic 14, its 3-bar SMA, `k`). A long of 1000 dollars' worth opens when the flat account sees both supertrends up
//! and `k` having crossed up through 15 within the last 20 chart bars (the bar before this one counts, this one does not); a
//! short, the mirror, `k` crossing down through 85 with both supertrends down. Only one trade at a time. While long, every bar
//! that closes under the 1-hour supertrend line rests a sell stop at that close, a short a buy stop over it; a stop once rested
//! stays until it fills or the next one replaces it. No costs; starts with 1,000.
//!
//! Each higher timeframe is the last bar closed by this bar's close, built from the run's own symbol's 30-minute bars from the
//! rule's first bar on (`vike_tactics::higher::HigherTimeframeSourceIntraday`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::{Stochrsi, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BACK: usize = 20;
const LOW: f64 = 15.0;
const HIGH: f64 = 85.0;
const DOLLARS: f64 = 1_000.0;

struct S {
    hours: HigherTimeframeSourceIntraday,
    fours: HigherTimeframeSourceIntraday,
    hour_trend: Supertrend,
    four_trend: Supertrend,
    stoch_rsi: Stochrsi,
    /// The 1-hour supertrend line and direction, and the 4-hour direction, as last closed.
    hour: (f64, f64),
    four_dir: f64,
    k: f64,
    over: Cross,
    under: Cross,
    /// Whether `k` crossed up through the low limit / down through the high limit, newest first.
    ups: VecDeque<bool>,
    downs: VecDeque<bool>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        hours: HigherTimeframeSourceIntraday::new(60),
        fours: HigherTimeframeSourceIntraday::new(240),
        hour_trend: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        four_trend: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        stoch_rsi: Stochrsi::new(14.0, 14.0, 3.0).expect("valid lengths").with_rs_form(true),
        hour: (f64::NAN, f64::NAN),
        four_dir: f64::NAN,
        k: f64::NAN,
        over: Cross::new(),
        under: Cross::new(),
        ups: VecDeque::new(),
        downs: VecDeque::new(),
    })
}

impl S {
    fn four_closed(&mut self, four: &Bar) {
        // The rule's direction: 1 down, -1 up.
        self.four_dir = -self.four_trend.next(four)[1];
        // The rule's stochastic reads the RSI numbers its 14-bar window holds as soon as the RSI has one (vike's once it holds
        // 14 of them), and its SMA is a fresh sum where vike's runs; the row's trades and net are the same.
        self.k = self.stoch_rsi.next_value(four.close)[1];
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        for hour in self.hours.next(bar, strategy.input(higher::HALF_HOUR)) {
            let [line, up] = self.hour_trend.next(&hour);
            self.hour = (line, -up);
        }
        for four in self.fours.next(bar, strategy.input(higher::HALF_HOUR)) {
            self.four_closed(&four);
        }
        let up = matches!(self.over.step(self.k, LOW), Signal::Long);
        let down = matches!(self.under.step(self.k, HIGH), Signal::Short);
        // `ups[0]` is this bar's: the rule looks at the bars before it.
        self.ups.push_front(up);
        self.downs.push_front(down);
        self.ups.truncate(BACK + 1);
        self.downs.truncate(BACK + 1);
        let back_long = self.ups.iter().skip(1).any(|&x| x);
        let back_short = self.downs.iter().skip(1).any(|&x| x);

        let (line, dir) = self.hour;
        let buy = self.four_dir < 0.0 && dir < 0.0 && back_long;
        let sell = self.four_dir > 0.0 && dir > 0.0 && back_short;
        let held = strategy.position();
        if held == 0.0 {
            if buy {
                strategy.entry("Long_Open", Long).size(cash_worth(&sizing, DOLLARS, bar.close));
            } else if sell {
                strategy.entry("Short_Open", Short).size(cash_worth(&sizing, DOLLARS, bar.close));
            }
        }
        if bar.close < line && held > 0.0 {
            strategy.exit("Long_Close").from("Long_Open").stop(bar.close);
        }
        if bar.close > line && held < 0.0 {
            strategy.exit("Short_Close").from("Short_Open").stop(bar.close);
        }
    }
}
