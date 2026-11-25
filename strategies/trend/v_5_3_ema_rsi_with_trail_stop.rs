// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "v.5 - 3 EMA + RSI with Trail Stop" by Free990, https://www.tradingview.com/script/mgD7xBuw/
// Changed: rewritten by hand in Rust for vike.

//! Long 20% of the account's worth at the next open when EMA(10) of the close crosses up through EMA(20) above EMA(100) on
//! an up bar. Held, it is closed at the next open when RSI(14) is over 70, or once 24 bars have closed in the trade on a
//! close over the entry bar's open. From the bar after the fill it also carries one exit: a stop 5% under the average entry
//! and a trailing stop that waits for 50 ticks past the entry and then follows the best price by 10 ticks. 0.05% commission;
//! starts with 1,000.
//!
//! The rule's shorts are switched off; its tables only draw.
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
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const NAME: &str = "Long";
const EXIT_RSI: f64 = 70.0;
const X_BARS: u32 = 24;
const STOP: f64 = 5.0;
const TRAIL: TacticsTrail = TacticsTrail { after_ticks: 50.0, by_ticks: 10.0 };
const SHARE: f64 = 20.0;

struct S {
    a: IndicatorLine,
    b: IndicatorLine,
    c: IndicatorLine,
    rsi: IndicatorLine,
    cross: Cross,
    last_held: f64,
    entry_open: f64,
    bars_in: u32,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        a: IndicatorLine::new("ema", &[10.0]),
        b: IndicatorLine::new("ema", &[20.0]),
        c: IndicatorLine::new("ema", &[100.0]),
        rsi: IndicatorLine::new("rsi", &[14.0]),
        cross: Cross::new(),
        last_held: 0.0,
        entry_open: f64::NAN,
        bars_in: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let a = self.a.next(bar)[0];
        let b = self.b.next(bar)[0];
        let c = self.c.next(bar)[0];
        let rsi = self.rsi.next(bar)[0];
        let crossed = matches!(self.cross.step(a, b), Signal::Long);
        let entry = crossed && a > c && bar.close > bar.open;
        let mut exit = rsi > EXIT_RSI;

        let held = strategy.position();
        if strategy.lots().len() == 1 && held > self.last_held {
            self.entry_open = bar.open;
        }
        self.last_held = held;
        if held > 0.0 {
            self.bars_in += 1;
        }
        if held == 0.0 {
            self.bars_in = 0;
        }
        if self.bars_in >= X_BARS && bar.close > self.entry_open {
            exit = true;
        }

        if entry {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if held > 0.0 && exit {
            strategy.close(NAME);
        }
        if held > 0.0 {
            let stop = strategy.avg_cost() * (1.0 - STOP / 100.0);
            strategy.exit("Trail Stop Long").from(NAME).stop(stop).trail(TRAIL);
        }
    }
}
