// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Multi Timeframe RSI Buy Sell Strategy [TradeDots]" by tradedots, https://www.tradingview.com/script/kfj6EdjQ/
// The original's notice: © tradedots
// Changed: rewritten by hand in Rust for vike.

//! The RSI(14) of the close on 5-minute bars, on 15-minute bars and on the chart's own bars. When all three are under 30, a
//! long of 60% of the account's worth, closing a short first; when all three are over 70, a short of 60%, closing a long
//! first. Each side cools down: once it placed its entry, its next one waits until its condition has held on five more bars
//! (counted on those bars alone) and is placed on the sixth, while the other side's signal ends that wait. Commission
//! 0.01%; starts with 10,000, no margin stated. The plotted average of the three is left out: no order reads it.
//!
//! A lower timeframe is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! its RSI over its bars from the rule's first bar on, as it stands after the last of them inside the chart bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsInput};

const LENGTH: f64 = 14.0;
const BUY_UNDER: f64 = 30.0;
const SELL_OVER: f64 = 70.0;
const COOL_DOWN: u32 = 5;
const SHARE: f64 = 60.0;
const FIVE_MINUTES: &str = "5m";
const FIFTEEN_MINUTES: &str = "15m";

/// The RSI of one lower timeframe, fed each bar of its input once, from the rule's first bar on.
struct Lower {
    unseen: HigherTimeframeSourceUnseen,
    rsi: Rsi,
    value: f64,
}

impl Lower {
    fn new() -> Self {
        Lower {
            unseen: HigherTimeframeSourceUnseen::new(),
            rsi: Rsi::new(LENGTH).expect("a valid length"),
            value: f64::NAN,
        }
    }

    /// The RSI after the last bar of `input` closed by now, its bars before `from` left out.
    fn next(&mut self, input: TacticsInput<'_>, from: i64) -> f64 {
        let bars = self.unseen.next(input);
        for bar in &bars[bars.partition_point(|b| b.timestamp < from)..] {
            self.value = self.rsi.next(bar);
        }
        self.value
    }
}

struct S {
    /// The rule's first bar's open: no lower bar before it is fed.
    from: Option<i64>,
    five: Lower,
    fifteen: Lower,
    own: Rsi,
    buy_wait: u32,
    sell_wait: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        five: Lower::new(),
        fifteen: Lower::new(),
        own: Rsi::new(LENGTH).expect("a valid length"),
        buy_wait: COOL_DOWN,
        sell_wait: COOL_DOWN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIVE_MINUTES.to_string(), FIFTEEN_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let rsis = [
            self.five.next(strategy.input(FIVE_MINUTES), from),
            self.fifteen.next(strategy.input(FIFTEEN_MINUTES), from),
            self.own.next(bar),
        ];
        let sizing = strategy.sizing();
        if rsis.iter().all(|&rsi| rsi < BUY_UNDER) {
            if self.buy_wait >= COOL_DOWN {
                strategy.close("Short");
                strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
                self.sell_wait = COOL_DOWN;
                self.buy_wait = 0;
            } else {
                self.buy_wait += 1;
            }
        }
        if rsis.iter().all(|&rsi| rsi > SELL_OVER) {
            if self.sell_wait >= COOL_DOWN {
                strategy.close("Long");
                strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
                self.sell_wait = 0;
                self.buy_wait = COOL_DOWN;
            } else {
                self.sell_wait += 1;
            }
        }
    }
}
