// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Customizable HTF MACD Strategy v2.0" by MauricioZuniga, https://www.tradingview.com/script/GOvvlTUT/
// The original's notice: (c) Wunderbit Trading
// The original's notice: © 2013  John F. Ehlers
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-06-08, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only, its signals read off the 5-minute bars. A MACD of the 5-minute close (the EMA(7) less the EMA(23), its signal
//! line the EMA(10) of that, each started at its first value and stepped as `last + 2 / (length + 1) * (value - last)`), and a
//! money flow index over 15 bars of the 5-minute typical price and volume (`100 - 100 / (1 + up / down)`; the first bar's flow
//! counts on both sides, as the rule's change of nothing compares false both ways). An entry signal when the MACD crosses up
//! through its signal line, or the money flow index crosses up through 12 while the SMA(19) of the 5-minute close is under
//! its EMA(19), and the ATR has a value over its floor, 0. The ATR is the rule's own: Wilder's step `last + (range - last) / 40`
//! of the 5-minute true range (that bar's high and low against the close the chart bar before read), started at its first
//! value. Flat, the signal with the SMA(19) under the EMA(19) buys all of the account's worth at the next open. Each signal,
//! taken or not, keeps its chart bar's high as the entry price, and a signal while flat sets the take-profit level 4% over its
//! bar's close, kept while held. Held, once a close is 4% over the average cost: a trailing exit that arms at that level,
//! rounded down to the tick, and follows the best price by 0.5% of it, cut to whole ticks; then the rule's second trailing
//! exit (1.3% of the entry price, armed and following), which finds the first holding all of the position and gets nothing.
//! A close at the next open once the close falls under the entry price less twice the ATR, or 8% under the average cost.
//! Commission 0.08%; version 5: starts with 1,000,000, the account kept in USD. The moving-average filter and the RSI filter
//! are off; the rule's date range (2018 to 9999) holds on every bar here. The plots, labels and alert comments are left out.
//! **Does not reproduce its reference** (180 trades, net -303305.40625 on BTCUSDT 30m, 2025-01-01..2026-09-23): this file
//! gives 180 / -303305.600885, the count and the net 0.195 under: a replay of its fills at the USDT-USD rates gives this net,
//! and a rate 0.00001 off on one of six days would close the gap, so likely the conversion; the owner accepted those numbers
//! on 2026-11-16.
//!
//! The rule states two defaults for two inputs, a number and then a `defval` named: the fixed stop's 8 and 1, the ATR floor's
//! 0 and 1. The first is taken, as the same author's open-open cross rule of 2022-06-24 measures (6 there: 7 trades, as the
//! reference; its named 1 gives 25). Here a stop of 8% and one of 1% give the same trades: the ATR stop always comes first.
//!
//! A lower timeframe is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! each 5-minute value is the last 5-minute bar inside the chart bar, from the rule's first bar on, and every average above
//! steps once a chart bar over those values. The rule's guard against the bar in progress reads the bar before only while
//! running live, which changes nothing a closed bar sees. Its one call with lookahead, the moving-average filter's 3-minute
//! series, decides no order with that filter off, so it is not read; with lookahead on a lower timeframe the last 3-minute bar
//! inside a closed chart bar is the same bar anyway.
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
const ATR_LENGTH: f64 = 40.0;
const ATR_MULTIPLE: f64 = 2.0;
const ATR_FLOOR: f64 = 0.0;
const TAKE_PROFIT: f64 = 4.0 / 100.0;
const TAKE_PROFIT_TRAIL: f64 = 0.5 / 100.0;
const TRAILING: f64 = 1.3 / 100.0;
const LOSS: f64 = 8.0 / 100.0;
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
    typical_before: f64,
    up_flow: RollingSum,
    down_flow: RollingSum,
    flow_cross: Cross,
    close_before: f64,
    atr: f64,
    /// The high of the last bar that signalled an entry.
    entry_price: f64,
    /// The take-profit level a signal while flat set, kept while held.
    take_profit: f64,
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
        typical_before: f64::NAN,
        up_flow: RollingSum::new(FLOW_LENGTH),
        down_flow: RollingSum::new(FLOW_LENGTH),
        flow_cross: Cross::new(),
        close_before: f64::NAN,
        atr: f64::NAN,
        entry_price: f64::NAN,
        take_profit: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.08), ..RuleDefaults::PLAIN }
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

        let entry = (macd_up || (flow_up && up_trend)) && self.atr > ATR_FLOOR;
        if entry {
            self.entry_price = bar.high;
        }
        let held = strategy.position();
        let cost = strategy.avg_cost();
        let exit = bar.close < self.entry_price - ATR_MULTIPLE * self.atr
            || bar.close < cost * (1.0 - LOSS);
        self.take_profit = if !(entry || held > 0.0) {
            f64::NAN
        } else if (entry && held <= 0.0) || self.take_profit.is_nan() {
            bar.close * (1.0 + TAKE_PROFIT)
        } else {
            self.take_profit
        };

        if up_trend && entry && held == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
            strategy.entry(LONG, Long).size(size);
        }
        if held > 0.0 && bar.close > cost * (1.0 + TAKE_PROFIT) {
            // The arming level rounds down to the tick (within 1e-4 of one), the offset truncates.
            let arm = (self.take_profit / tick + 1e-4).floor();
            let offset = (self.take_profit * TAKE_PROFIT_TRAIL / tick).trunc();
            let after = arm - (cost / tick).round();
            let trail = TacticsTrail { after_ticks: after, by_ticks: offset };
            strategy.exit("Long Trailing Profit").from(LONG).trail(trail);
            let distance = self.entry_price * TRAILING / tick;
            let trail = TacticsTrail { after_ticks: distance.floor(), by_ticks: distance.trunc() };
            strategy.exit("ATR Trail stop").from(LONG).trail(trail);
        }
        if exit {
            strategy.close(LONG);
        }
    }
}
