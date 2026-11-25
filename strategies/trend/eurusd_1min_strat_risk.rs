// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EURUSD 1min strat RISK %%" by SoftKill21, https://www.tradingview.com/script/l7ELtgmb/
// The original's notice: © SoftKill21
// Changed: rewritten by hand in Rust for vike.

//! When no trade is open: long 10,000 when the rule's parabolic SAR is rising, EMA(10) crosses up through SMA(17), the MACD
//! histogram (SMA 12 less SMA 26, less its SMA 9) is positive and RSI(5) is above 72; short 10,000 on the mirror with RSI(5)
//! below 23. A long closes on the short signal and a short on the long one. Every trade closes at the next open once its open
//! loss at a close is more than 1% of the closed balance (starting cash plus closed profit). No costs; starts with 1,000.
//!
//! The rule sizes from a risk of 2% over a stop of 500,000 ATR(14) in ticks, which always falls under its floor of 10,000, so
//! every trade is 10,000. Its stop and target lie 5,000 ATR(14) from the entry, never reached, and are left out. Its
//! two entries share a cancel group, which the open-trade gate already makes moot.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Psar, Rsi, Sma};
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SIZE: f64 = 10_000.0;
const CASH: f64 = 1_000.0;
const PROTECT: f64 = 1.0 / 100.0;

struct S {
    sar: Psar,
    rsi: Rsi,
    fast: Sma,
    slow: Sma,
    signal: Sma,
    ema: Ema,
    basis: Sma,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        sar: parabolic_sar(0.02, 0.02, 0.2),
        rsi: Rsi::new(5.0).expect("a valid length"),
        fast: sma(12.0),
        slow: sma(26.0),
        signal: sma(9.0),
        ema: Ema::new(10.0).expect("a valid length"),
        basis: sma(17.0),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.sar.next(bar);
        let rsi = self.rsi.next(bar);
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let hist = if macd.is_nan() { f64::NAN } else { macd - self.signal.next_value(macd) };
        let ema = self.ema.next(bar);
        let basis = self.basis.next(bar);
        let cross = self.cross.step(ema, basis);
        let up = self.sar.uptrend() == Some(true);
        let down = self.sar.uptrend() == Some(false);
        let short = down && matches!(cross, Signal::Short) && hist < 0.0 && rsi < 23.0;
        let long = up && matches!(cross, Signal::Long) && hist > 0.0 && rsi > 72.0;

        let held = strategy.position();
        let balance = strategy.closed_net().unwrap_or(0.0) + CASH;
        let floating = if held == 0.0 { 0.0 } else { (bar.close - strategy.avg_cost()) * held };
        if floating < 0.0 && (floating / balance).abs() > PROTECT {
            strategy.close_all();
        }
        if held == 0.0 {
            if long {
                strategy.entry("long", Long).size(SIZE);
            }
            if short {
                strategy.entry("short", Short).size(SIZE);
            }
        }
        if short {
            strategy.close("long");
        }
        if long {
            strategy.close("short");
        }
    }
}
