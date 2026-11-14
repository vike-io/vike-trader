// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "AlphaTrend" by only_fibonacci, https://www.tradingview.com/script/uvkI3QJC/
// The original's notice: author © KivancOzbilgic
// The original's notice: developer © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! A trend line that follows `low - ATR` up while the 14-bar money flow index of the typical price is at least 50, and
//! `high + ATR` down otherwise, never moving against itself; ATR is the 14-bar SMA of the true range. Long 1 when the line
//! crosses up through its value two bars back, short 1 when it crosses down, each reversing the other, and each only when the
//! last opposite cross came at least two bars after the cross of its own kind before this one. No costs; starts with
//! 1,000,000.
//!
//! The line starts from 0 on its first valued bar, as the rule's `nan_to_zero` of no last value reads. The true range has no value on the
//! first bar (`TrueRange::with_first_bar_missing`). The rule's money flow index counts its first bar's flow on both sides, where vike's `Mfi` counts it on neither
//! and has its first value a bar later; the line has none until the ATR's first value a bar later still, so the two give the
//! same line.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::rolling::RollingSum;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Mfi, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const PERIOD: usize = 14;

struct S {
    range: TrueRange,
    true_range: RollingSum,
    mfi: Mfi,
    /// The line's last three values, newest first.
    line: VecDeque<f64>,
    cross: Cross,
    bar: usize,
    last_buy: Option<usize>,
    last_sell: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: TrueRange::default().with_first_bar_missing(true),
        true_range: RollingSum::new(PERIOD),
        mfi: Mfi::new(PERIOD as f64).expect("a valid length"),
        line: VecDeque::from([f64::NAN; 3]),
        cross: Cross::new(),
        bar: 0,
        last_buy: None,
        last_sell: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let atr = self.true_range.next(self.range.next(bar)) / PERIOD as f64;
        let flowing = self.mfi.next(bar) >= 50.0;

        let last = nan_to_zero(self.line[0]);
        let floor = bar.low - atr;
        let ceiling = bar.high + atr;
        let line = if flowing {
            if floor < last { last } else { floor }
        } else if ceiling > last {
            last
        } else {
            ceiling
        };
        self.line.pop_back();
        self.line.push_front(line);

        let t = self.bar;
        self.bar += 1;
        let signal = self.cross.step(self.line[0], self.line[2]);
        let (buy, sell) = (matches!(signal, Signal::Long), matches!(signal, Signal::Short));
        // Bars since the buy before this bar, counted from the bar after it; bars since the latest sell, this bar included.
        let since_buy = self.last_buy.map(|j| t - j - 1);
        let since_sell = if sell { Some(0) } else { self.last_sell.map(|s| t - s) };
        let since_sell_before = self.last_sell.map(|s| t - s - 1);
        let since_buy_now = if buy { Some(0) } else { self.last_buy.map(|j| t - j) };
        if buy && matches!((since_buy, since_sell), (Some(o), Some(k)) if o > k) {
            strategy.entry("buy", Long).size(1.0);
        }
        if sell && matches!((since_sell_before, since_buy_now), (Some(o), Some(k)) if o > k) {
            strategy.entry("sell", Short).size(1.0);
        }
        if buy {
            self.last_buy = Some(t);
        }
        if sell {
            self.last_sell = Some(t);
        }
    }
}
