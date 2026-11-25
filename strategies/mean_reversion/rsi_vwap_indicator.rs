// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI-VWAP Indicator %" by UnknownUnicorn2151907, https://www.tradingview.com/script/d0FZtMLp/
// The original's notice: © Xaviz
// Changed: rewritten by hand in Rust for vike.

//! An RSI(16) of the day's VWAP of the close (the sum of close times volume over the sum of volume, restarting each UTC day).
//! Long a quarter of the account's worth in BTC (`equity * 25% / close`, floored to the step, no commission in it) when the RSI
//! crosses up through 18, up to 5 entries; half of the position closed at the next open when it crosses down through 80. 0.04%
//! commission; version 4: starts with 100,000.
//!
//! The rule states a drawdown limit of 15% of equity, which halts the run as the original's broker keeps it: the peak is the
//! larger of the capital and the realized equity so far (open profit never lifts it); the drop is that peak less the equity
//! marked now; it is judged only on a fill that closed a trade; and when it reaches the limit every pending order is
//! cancelled, the position is closed at once at the price of that fill, and nothing is ordered again.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Rsi, Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::floor_share;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CAPITAL: f64 = 100_000.0;
const MAX_DRAWDOWN: f64 = 15.0;

struct S {
    vwap: Vwap,
    rsi: Rsi,
    low: Cross,
    high: Cross,
    /// The realized-equity peak, the closed trades counted so far, and whether the drawdown limit has halted the run.
    peak: f64,
    closed: usize,
    halted: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        vwap: Vwap::default().with_source(VwapSource::Close),
        rsi: Rsi::new(16.0).expect("a valid length"),
        low: Cross::new(),
        high: Cross::new(),
        peak: CAPITAL,
        closed: 0,
        halted: false,
    })
}

impl S {
    /// The drawdown limit, judged on a fill that closed a trade: the peak is the larger of the capital and the realized equity so
    /// far, the drop that peak less the equity marked now, and at the limit every order is pulled and the position closed at once.
    fn risk(&mut self, strategy: &mut Tactics<'_>) {
        if self.halted {
            return;
        }
        self.peak = self.peak.max(CAPITAL + strategy.closed_net().unwrap_or(0.0));
        let closed = strategy.closed_trades().unwrap_or(&[]).len();
        let booked = closed > self.closed;
        self.closed = closed;
        if booked && self.peak - strategy.equity() >= self.peak * MAX_DRAWDOWN / 100.0 {
            strategy.cancel_all();
            strategy.close_all();
            self.halted = true;
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 5, fee: Commission::percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        if self.halted {
            return;
        }
        let rsi = self.rsi.next_value(self.vwap.next(bar));
        if matches!(self.low.step(rsi, 18.0), Signal::Long) {
            let worth = strategy.equity();
            strategy.entry("LONG", Long).size(sizing.floor(0.25 * (worth / bar.close)));
        }
        if matches!(self.high.step(rsi, 80.0), Signal::Short) {
            let held = strategy.position();
            strategy.close("LONG").size(floor_share(held * 0.5, held, sizing.qty_step()));
        }
        self.risk(strategy);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.risk(strategy);
    }
}
