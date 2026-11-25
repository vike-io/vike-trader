// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Customizable HTF MACD Strategy v1.2" by MauricioZuniga, https://www.tradingview.com/script/X4Ix7Tbw/
// The original's notice: (c) Wunderbit Trading
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-06-06, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only, everything read off the 5-minute bars. A MACD of the 5-minute close (the EMA(7) less the EMA(23), its signal
//! line the EMA(10) of that, each started at its first value and stepped as `last + 2 / (length + 1) * (value - last)`), and a
//! money flow index over 15 bars of the 5-minute typical price and volume (`100 - 100 / (1 + up / down)`; the first bar's flow
//! counts on both sides, as the rule's change of nothing compares false both ways). An entry signal when the MACD crosses up
//! through its signal line, or the money flow index crosses up through 12 while the SMA(19) of the 5-minute close is under
//! its EMA(19), and the RSI(34) of the 5-minute close has a value (its filter, under 100, is off). Flat, the signal with the
//! SMA(19) under the EMA(19) buys all of the account's worth at the next open. Each signal, taken or not, keeps its bar's close
//! as the entry price. Held, a trailing stop that waits for the price to run 1.3% of that entry price past the fill and then
//! follows the best price by the same distance, both cut to whole ticks, and a close at the next open once the close falls
//! under the entry price less twice the ATR: the rule's own, Wilder's step `last + (range - last) / 40` of the 5-minute true
//! range (that bar's high and low against the close the chart bar before read), started at its first value. Commission
//! 0.07%; version 4: starts with 100,000, the account kept in USD. The take profit is off, as are the trend and RSI filters;
//! the rule's date range (2018 to 9999) holds on every bar here. The plots and the alert comments are left out.
//!
//! A lower timeframe is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! each 5-minute value is the last 5-minute bar inside the chart bar, from the rule's first bar on, and every average above
//! steps once a chart bar over those values. The rule's guard against the bar in progress reads the bar before only while
//! running live, which changes nothing a closed bar sees.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Rsi, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const FIVE_MINUTES: &str = "5m";
const RIBBON: f64 = 19.0;
const FAST: f64 = 7.0;
const SLOW: f64 = 23.0;
const SIGNAL: f64 = 10.0;
const FLOW_LENGTH: usize = 15;
const FLOW_FLOOR: f64 = 12.0;
const RSI_LENGTH: f64 = 34.0;
const RSI_CEILING: f64 = 100.0;
const ATR_LENGTH: f64 = 40.0;
const ATR_MULTIPLE: f64 = 2.0;
const TRAILING: f64 = 1.3 / 100.0;
const LONG: &str = "long";

/// The rule's own average: its first value as is, then `last + 2 / (length + 1) * (value - last)`.
struct Smoothed {
    factor: f64,
    last: f64,
}

impl Smoothed {
    fn new(length: f64) -> Self {
        Smoothed { factor: 2.0 / (length + 1.0), last: f64::NAN }
    }

    fn next(&mut self, value: f64) -> f64 {
        let stepped = self.last + self.factor * (value - self.last);
        self.last = if stepped.is_nan() { value } else { stepped };
        self.last
    }
}

struct S {
    /// The rule's first bar's open: no 5-minute bar before it is read.
    from: Option<i64>,
    fast: Smoothed,
    slow: Smoothed,
    signal: Smoothed,
    macd_cross: Cross,
    ribbon_average: Ema,
    ribbon_mean: Sma,
    rsi: Rsi,
    typical_before: f64,
    up_flow: RollingSum,
    down_flow: RollingSum,
    flow_cross: Cross,
    close_before: f64,
    atr: f64,
    /// The close of the last bar that signalled an entry.
    entry_price: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        fast: Smoothed::new(FAST),
        slow: Smoothed::new(SLOW),
        signal: Smoothed::new(SIGNAL),
        macd_cross: Cross::new(),
        ribbon_average: Ema::new(RIBBON).expect("a valid length"),
        ribbon_mean: Sma::new(RIBBON).expect("a valid length"),
        rsi: Rsi::new(RSI_LENGTH).expect("a valid length"),
        typical_before: f64::NAN,
        up_flow: RollingSum::new(FLOW_LENGTH),
        down_flow: RollingSum::new(FLOW_LENGTH),
        flow_cross: Cross::new(),
        close_before: f64::NAN,
        atr: f64::NAN,
        entry_price: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.07), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIVE_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let Some(tick) = strategy.tick() else { return };
        let input = strategy.input(FIVE_MINUTES);
        let Some(lower) = input.bar().filter(|b| b.timestamp >= from).cloned() else { return };

        let macd = self.fast.next(lower.close) - self.slow.next(lower.close);
        let signal = self.signal.next(macd);
        let macd_up = matches!(self.macd_cross.step(macd, signal), Signal::Long);

        let up_trend = self.ribbon_mean.next(&lower) < self.ribbon_average.next(&lower);
        let rsi = self.rsi.next(&lower);

        let typical = (lower.high + lower.low + lower.close) / 3.0;
        let change = typical - self.typical_before;
        self.typical_before = typical;
        let up = self.up_flow.next(lower.volume * if change <= 0.0 { 0.0 } else { typical });
        let down = self.down_flow.next(lower.volume * if change >= 0.0 { 0.0 } else { typical });
        let flow = 100.0 - 100.0 / (1.0 + up / down);
        let flow_up = matches!(self.flow_cross.step(flow, FLOW_FLOOR), Signal::Long);

        let close_before = self.close_before;
        self.close_before = lower.close;
        let range = (lower.high - lower.low)
            .max((lower.high - close_before).abs())
            .max((lower.low - close_before).abs());
        // The rule's `max` of a missing value is missing: no range on the first bar.
        let range = if close_before.is_nan() { f64::NAN } else { range };
        let stepped = self.atr + (range - self.atr) / ATR_LENGTH;
        self.atr = if stepped.is_nan() { range } else { stepped };

        let entry = (macd_up || (flow_up && up_trend)) && rsi < RSI_CEILING;
        if entry {
            self.entry_price = bar.close;
        }
        let exit = bar.close < self.entry_price - ATR_MULTIPLE * self.atr;

        if up_trend && entry && strategy.position() == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
            strategy.entry(LONG, Long).size(size);
        }
        let distance = self.entry_price * TRAILING / tick;
        if distance.is_finite() {
            let trail = TacticsTrail { after_ticks: distance.floor(), by_ticks: distance.trunc() };
            strategy.exit("Trail stop").from(LONG).trail(trail);
        }
        if exit {
            strategy.close(LONG);
        }
    }
}
