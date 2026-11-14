// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "[blackcat] L2 Reversal Labels Strategy" by blackcat1402, https://www.tradingview.com/script/FH1VyeNT/
// The original's notice: © blackcat1402
// Changed: rewritten by hand in Rust for vike.

//! A divergence on the MACD(12, 26, 9) line at its crosses. Long 1 BTC when the line crosses up through its signal line with
//! the close below, and the line above, where both stood on the bar of the previous cross up; short 1 BTC when it crosses
//! down with the close above, and the line below, where both stood on the bar of the previous cross down. Each reverses the
//! other. No costs; starts with 1,000,000.
//!
//! The rule finds that earlier bar as the bars since the cross one bar back, plus one, which is the bar of the latest cross
//! before this one.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Macd;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

struct S {
    macd: Macd,
    cross: Cross,
    /// `(close, MACD line)` on the bar of the latest cross up, and of the latest cross down.
    last_up: Option<(f64, f64)>,
    last_down: Option<(f64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        cross: Cross::new(),
        last_up: None,
        last_down: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [line, signal, _] = self.macd.next(bar);
        match self.cross.step(line, signal) {
            Signal::Long => {
                if let Some((close, was)) = self.last_up.replace((bar.close, line))
                    && close > bar.close
                    && line > was
                {
                    strategy.entry("long", Long).size(1.0);
                }
            }
            Signal::Short => {
                if let Some((close, was)) = self.last_down.replace((bar.close, line))
                    && close < bar.close
                    && was > line
                {
                    strategy.entry("short", Short).size(1.0);
                }
            }
            _ => {}
        }
    }
}
