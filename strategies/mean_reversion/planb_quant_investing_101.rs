// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "PlanB Quant Investing 101" by fillippone, https://www.tradingview.com/script/Tz02ikOo/
// The original's notice: © fillippone
// Changed: rewritten by hand in Rust for vike.

//! RSI(14) of the close against its own last six values (the six bars before this one). Long when the lowest of them is
//! under 50 and this bar's RSI is more than 2 over it; short when the highest of them is over 90 and this bar's RSI is under
//! 65, the other side closed by the entry. Each entry is 1,000 of the account's money at this close, one a side. No costs;
//! starts with 1,000 in an account kept in USD.
//!
//! Over the reference's window the long opened on its 22nd bar never meets a short signal, so no trade closes, as in the
//! reference.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::strategy_kit::{IndicatorLine, max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The RSI values before this bar the signals look back over.
const LOOKBACK: usize = 6;
const CASH: f64 = 1_000.0;

struct S {
    rsi: IndicatorLine,
    /// The RSI of the last `LOOKBACK` bars, the newest last.
    past: VecDeque<f64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        past: VecDeque::with_capacity(LOOKBACK + 1),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar)[0];
        let (highest, lowest) = if self.past.len() == LOOKBACK {
            let fold = |pick: fn(f64, f64) -> f64| self.past.iter().copied().reduce(pick);
            (fold(max).unwrap_or(f64::NAN), fold(min).unwrap_or(f64::NAN))
        } else {
            (f64::NAN, f64::NAN)
        };
        self.past.push_back(rsi);
        if self.past.len() > LOOKBACK {
            self.past.pop_front();
        }
        let buy = lowest < 50.0 && rsi > lowest + 2.0;
        let sell = highest > 90.0 && rsi < 65.0;
        let sizing = strategy.sizing();
        if buy {
            strategy.entry("Buy Signal", Long).size(cash_worth(&sizing, CASH, bar.close));
        }
        if sell {
            strategy.entry("Sell Signal", Short).size(cash_worth(&sizing, CASH, bar.close));
        }
    }
}
