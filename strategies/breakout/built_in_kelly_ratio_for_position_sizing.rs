// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Built-in Kelly ratio for position sizing" by CryptoRox, https://www.tradingview.com/script/bFXf4IXh/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-02-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Bands one ATR(10) either side of EMA(20) of the close. When the close crosses up through the upper band, a buy stop rests a
//! tick over this bar's high; when it crosses down through the lower band, a sell stop a tick under its low. Once there has been
//! such a cross, the buy stop is pulled while the close is under the EMA or the high has reached its level, the sell stop on the
//! mirror; the opposite entry reverses. Each is sized at the Kelly fraction of the account's worth when the closed trades make it
//! positive, and at 10,000 worth otherwise, floored to the quantity step; one that floors to nothing only closes an opposite
//! position. 0.1% commission; starts with 10,000.
//!
//! The rule's targets and stops are off by default, and so is its date range.
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
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CASH: f64 = 10_000.0;
/// In percent.
const COMMISSION: f64 = 0.1;

struct S {
    ema: IndicatorLine,
    atr: IndicatorLine,
    up: Cross,
    down: Cross,
    buy_level: f64,
    sell_level: f64,
    crossed_up: bool,
    crossed_down: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: IndicatorLine::new("ema", &[20.0]),
        atr: IndicatorLine::new("atr", &[10.0]),
        up: Cross::new(),
        down: Cross::new(),
        buy_level: 0.0,
        sell_level: 0.0,
        crossed_up: false,
        crossed_down: false,
    })
}

/// The Kelly fraction of the closed trades, NaN when a part of it divides by zero. The gross loss also carries the open lots'
/// entry commission, as the rule's original counts it.
fn kelly(strategy: &Tactics<'_>) -> f64 {
    let rows = strategy.closed_trades().unwrap_or(&[]);
    let wins: Vec<f64> = rows.iter().map(|r| r.net).filter(|n| *n > 0.0).collect();
    let losses: Vec<f64> = rows.iter().map(|r| r.net).filter(|n| *n < 0.0).collect();
    let ratio = |a: f64, b: f64| if b == 0.0 { f64::NAN } else { a / b };
    let p = ratio(wins.len() as f64, rows.len() as f64);
    let win = ratio(wins.iter().sum(), wins.len() as f64);
    let open_fee: f64 = strategy.lots().iter().map(|l| l.qty * l.price * COMMISSION / 100.0).sum();
    let loss = ratio(losses.iter().fold(0.0, |g, l| g - l) + open_fee, losses.len() as f64);
    p - (1.0 - p) / (win / loss)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(COMMISSION), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let ema = self.ema.next(bar)[0];
        let atr = self.atr.next(bar)[0];
        let kr = kelly(strategy);
        let size = sizing.floor(if kr > 0.0 {
            strategy.equity() * kr / bar.close
        } else {
            CASH / bar.close
        });
        let cross_up = matches!(self.up.step(bar.close, ema + atr), Signal::Long);
        let cross_down = matches!(self.down.step(bar.close, ema - atr), Signal::Short);
        if cross_up {
            self.buy_level = bar.high + tick;
            self.crossed_up = true;
        }
        if cross_down {
            self.sell_level = bar.low - tick;
            self.crossed_down = true;
        }
        if self.crossed_up && (bar.close < ema || bar.high >= self.buy_level) {
            strategy.cancel("long");
        }
        if cross_up {
            strategy.entry("long", Long).size(size).stop(self.buy_level);
        }
        if self.crossed_down && (bar.close > ema || bar.low <= self.sell_level) {
            strategy.cancel("short");
        }
        if cross_down {
            strategy.entry("short", Short).size(size).stop(self.sell_level);
        }
    }
}
