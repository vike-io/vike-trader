// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "X48 - Strategy | Channel BreakOut & Consecutive (17in1) + TL/TP/SL | V.2.13" by X4815162342, https://www.tradingview.com/script/nt3wgHEc/
// The original's notice: © X4815162342
// Changed: rewritten by hand in Rust for vike.

//! A channel breakout that is always in the market: on every bar a buy stop a tick over the highest high of the last 5 bars
//! (this one included) and a sell stop a tick under their lowest low, each 16.333 worth and reversing the other. The buy stop
//! waits until 5 bars lie before this one, the sell stop until 5 bars have come. 0.1% commission; starts with 50, and the cash
//! size keeps trading with the account below nothing. **Does not reproduce its reference** (3677 trades, net -105.361972 on
//! BTCUSDT 30m, 2025-01-01..2026-09-15): this file gives 3678 / -105.383959, one trade more: the reference has no trade before
//! 2025-01-01 11:30 UTC, and this file's first trade (a short from 04:30, filled by its stop) is the whole difference, every
//! later trade the same to 1e-6; nothing in the original delays its orders; the owner accepted those numbers on 2026-11-16.
//!
//! A stop's size is 16.333 at its level on the grid, or at this close when the close is already through it, floored to the
//! quantity step. The rule runs again after each of its fills inside a bar, reading that bar's final values as the original
//! does on history: both stops are placed again from the channel this bar closes, which puts the other side's stop past the
//! bar's own extreme, so it cannot fill in the bar that reversed.
//!
//! Measured: while the channel has fewer than 5 bars its sell stop has no level, and read as a market order it opens one
//! short at the second bar's open that the reference does not have: 3678 trades / -105.355538 against 3677 / -105.361972,
//! that trade's net being the whole difference (0.006433). The stop with no level places nothing.
//!
//! Of the original's 17 modes only the breakout is on by default; the consecutive bars, the range break, the moving-average and
//! MACD crosses, every confirmation, the swing, ATR, callback, percent and profit exits, the spot mode and the entry cover are
//! off, so they and the 1-minute and same-timeframe reads they alone use are left out. Its time filters (1945 to 2074, 00:00
//! to 23:59 of each day) take every bar. The imported library (X4815162342's `X48_LibaryStrategyStatus`, MPL-2.0) only draws
//! a status table, so none of it is ported; its alerts, plots and lines only draw too.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::push;
use vike_tactics::sizing::cash_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 5;
const CASH: f64 = 16.333;

struct S {
    /// The last `LENGTH` highs and lows, newest first.
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    /// Bars fed so far, this one included.
    bars: usize,
    /// The bar the channel has already taken: a fill inside a bar feeds it ahead of its close.
    fed: Option<i64>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { highs: VecDeque::new(), lows: VecDeque::new(), bars: 0, fed: None })
}

/// The price a stop entry at `level` is sized from.
fn sized_at(strategy: &Tactics<'_>, side: OrderSide, level: f64, close: f64) -> f64 {
    let level = strategy.snap_stop(side, level);
    if (side == Long && level <= close) || (side == Short && level >= close) {
        close
    } else {
        level
    }
}

impl S {
    fn rule(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.fed != Some(bar.timestamp) {
            self.fed = Some(bar.timestamp);
            push(&mut self.highs, bar.high, LENGTH);
            push(&mut self.lows, bar.low, LENGTH);
            self.bars += 1;
        }
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let full = self.highs.len() == LENGTH;
        let top = self.highs.iter().copied().fold(f64::MIN, f64::max);
        let bottom = self.lows.iter().copied().fold(f64::MAX, f64::min);

        // The close `LENGTH` bars back exists.
        if self.bars > LENGTH {
            let level = top + tick;
            let size = cash_worth(&sizing, CASH, sized_at(strategy, Long, level, bar.close));
            strategy.entry("Long", Long).size(size).stop(level);
        }
        // A stop with no level places nothing.
        if full {
            let level = bottom - tick;
            let size = cash_worth(&sizing, CASH, sized_at(strategy, Short, level, bar.close));
            strategy.entry("Short", Short).size(size).stop(level);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule(strategy, bar);
    }
}
