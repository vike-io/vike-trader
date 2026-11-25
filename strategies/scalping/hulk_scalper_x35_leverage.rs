// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Hulk Scalper x35 Leverage" by npietronuto1, https://www.tradingview.com/script/oGUYuz4N/
// The original's notice: © npietronuto1
// Changed: rewritten by hand in Rust for vike.

//! A pullback scalp in the direction of the EMA(50)/EMA(200) trend, one trade at a time, all of the account's worth. Flat, a
//! long when the close is above EMA(200), EMA(50) is above EMA(200), ADX(14, 14) is above 33, RSI(20) is within 2 of 50 (both
//! bounds strict), and the 1-minute MACD(12, 26, 11) histogram is under -5.45; a short is the mirror (close and EMA(50) under
//! EMA(200), the histogram over 5.45). Each entry carries one exit from its fill, issued with it and never moved: a target
//! 1.35% and a stop 1.1% from the signal bar's close. At most 6 entries between two crosses of the averages: the count the bar
//! before left is under 6, and a cross either way sets it back to 0. No costs; starts with 100, no margin stated.
//!
//! The ADX is the rule's own: Wilder averages of the directional moves and of the true range, both missing on the first bar,
//! each line held at its last number when it has none (`Adx::with_held_lines`), and `100 *` the Wilder average of
//! `|+DI - -DI| / (+DI + -DI)`.
//!
//! The 1-minute frame is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! the MACD over its bars from the rule's first bar on, as it stands after the last of them inside the chart bar. The rule asks
//! for it with gaps on, which changes one thing only: a chart bar with no 1-minute bar inside it reads no histogram (and so
//! takes no entry), where gaps off would read the last one again. The EMA fills, the alerts and the plots are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema, Macd, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const ONE_MINUTE: &str = "1m";
const RSI_BAND: f64 = 2.0;
const ADX_OVER: f64 = 33.0;
const HISTOGRAM_BAND: f64 = 5.45;
const MAX_TRADES: u32 = 6;
const TARGET: f64 = 0.0135;
const STOP: f64 = 0.011;

struct S {
    /// The rule's first bar's open: no 1-minute bar before it is fed.
    from: Option<i64>,
    unseen: HigherTimeframeSourceUnseen,
    macd: Macd,
    rsi: Rsi,
    adx: Adx,
    fast: Ema,
    slow: Ema,
    fast_before: f64,
    slow_before: f64,
    /// The entries since the last cross of the averages as the bar before left them, `None` before the first bar.
    trades_before: Option<u32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        unseen: HigherTimeframeSourceUnseen::new(),
        macd: Macd::new(12.0, 26.0, 11.0).expect("valid lengths"),
        rsi: Rsi::new(20.0).expect("a valid length"),
        adx: Adx::new(14.0).expect("a valid length").with_held_lines(true),
        fast: Ema::new(50.0).expect("a valid length"),
        slow: Ema::new(200.0).expect("a valid length"),
        fast_before: f64::NAN,
        slow_before: f64::NAN,
        trades_before: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![ONE_MINUTE.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let minutes = self.unseen.next(strategy.input(ONE_MINUTE));
        let mut histogram = f64::NAN;
        for minute in &minutes[minutes.partition_point(|b| b.timestamp < from)..] {
            [_, _, histogram] = self.macd.next(minute);
        }
        let rsi = self.rsi.next(bar) - 50.0;
        let [adx, _, _] = self.adx.next(bar);
        let fast = self.fast.next_value(bar.close);
        let slow = self.slow.next_value(bar.close);

        let crossed = (fast > slow && self.fast_before <= self.slow_before)
            || (slow > fast && self.slow_before <= self.fast_before);
        let mut trades = if crossed { 0 } else { self.trades_before.unwrap_or(0) };
        let below_cap = self.trades_before.is_some_and(|n| n < MAX_TRADES);
        let calm = rsi < RSI_BAND && rsi > -RSI_BAND;
        let open = below_cap && calm && adx > ADX_OVER && strategy.position() == 0.0;
        let close = bar.close;
        let size = share_of_worth(&strategy.sizing(), 100.0, close);

        if open && close > slow && fast > slow && histogram < -HISTOGRAM_BAND {
            strategy.entry("long", Long).size(size);
            strategy
                .exit("exit")
                .from("long")
                .take(close * (1.0 + TARGET))
                .stop(close * (1.0 - STOP));
            trades += 1;
        }
        if open && close < slow && fast < slow && histogram > HISTOGRAM_BAND {
            strategy.entry("short", Short).size(size);
            strategy
                .exit("exit")
                .from("short")
                .take(close * (1.0 - TARGET))
                .stop(close * (1.0 + STOP));
            trades += 1;
        }
        (self.fast_before, self.slow_before) = (fast, slow);
        self.trades_before = Some(trades);
    }
}
