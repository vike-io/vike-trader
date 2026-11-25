// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Divergence Strategy" by Pro_Trader_HTBB, https://www.tradingview.com/script/mU02nqSK/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-06-12, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Regular divergence of RSI(14) at its pivots, 5 bars each side. A bullish one is a pivot low of the RSI above the one
//! before it, with the low 5 bars back below the low 5 bars back at that one, and between 5 and 60 bars from the previous
//! bar to the pivot before; a bearish one mirrors it on pivot highs and highs. Long 10% of the account's worth on a bullish
//! divergence with RSI under 35, short 10% on a bearish one with RSI over 76, each reversing the other; a long closes once
//! RSI reaches 80, a short once it falls to 54.1. 0.1% commission; starts with 100,000 in a USD account.
//!
//! The RSI is the rule's own: Wilder averages of the gains and losses, `100 - 100 / (1 + up / down)` (`Rsi::with_rs_form`),
//! 100 while the mean loss is 0 and else 0 while the mean gain is (`Rsi::with_zero_saturation`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Rsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 14;
const BACK: usize = 5;
const RANGE_LOWER: usize = 5;
const RANGE_UPPER: usize = 60;
const LONG_ENTRY: f64 = 35.0;
const SHORT_ENTRY: f64 = 76.0;
const LONG_EXIT: f64 = 80.0;
const SHORT_EXIT: f64 = 54.1;
const SHARE: f64 = 10.0;

/// One side's pivots: the bar of the last two, and the RSI and price 5 bars back at each.
#[derive(Default)]
struct Side {
    /// `(bar, rsi, price)` of the newest pivot and the one before it.
    last: Option<(usize, f64, f64)>,
    before: Option<(usize, f64, f64)>,
}

impl Side {
    /// Bars from the bar before `bar` back to the newest pivot before `bar`.
    fn in_range(&self, bar: usize) -> bool {
        self.last.is_some_and(|(at, _, _)| (RANGE_LOWER..=RANGE_UPPER).contains(&(bar - 1 - at)))
    }

    fn found(&mut self, at: (usize, f64, f64)) {
        self.before = self.last.replace(at);
    }
}

struct S {
    rsi: Rsi,
    pivots: ConfirmedPivot,
    /// The RSI, low and high of the last 6 bars, newest last.
    window: Vec<(f64, f64, f64)>,
    lows: Side,
    highs: Side,
    bar: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(LENGTH as f64)
            .expect("a valid length")
            .with_rs_form(true)
            .with_zero_saturation(true),
        pivots: ConfirmedPivot::new(BACK as f64, BACK as f64).expect("valid spans"),
        window: Vec::new(),
        lows: Side::default(),
        highs: Side::default(),
        bar: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let rsi = self.rsi.next_value(bar.close);
        self.window.push((rsi, bar.low, bar.high));
        if self.window.len() > BACK + 1 {
            self.window.remove(0);
        }
        let [pivot_high, pivot_low, ..] = self.pivots.next_hl(rsi, rsi);
        let (n, (rsi_back, low_back, high_back)) = (self.bar, self.window[0]);
        self.bar += 1;

        let mut bull = false;
        if !pivot_low.is_nan() {
            let in_range = self.lows.in_range(n);
            self.lows.found((n, rsi_back, low_back));
            bull =
                self.lows.before.is_some_and(|(_, r, p)| low_back < p && rsi_back > r && in_range);
        }
        let mut bear = false;
        if !pivot_high.is_nan() {
            let in_range = self.highs.in_range(n);
            self.highs.found((n, rsi_back, high_back));
            bear = self
                .highs
                .before
                .is_some_and(|(_, r, p)| high_back > p && rsi_back < r && in_range);
        }

        if bull && rsi < LONG_ENTRY {
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if bear && rsi > SHORT_ENTRY {
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if rsi >= LONG_EXIT {
            strategy.close("long");
        }
        if rsi <= SHORT_EXIT {
            strategy.close("short");
        }
    }
}
