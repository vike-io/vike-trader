// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Keltner Channel ETH/USDT 1H" by WunderTrading, https://www.tradingview.com/script/ezSBm5MS/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2021-08-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only, all of the account's worth, in an up trend (SMA(46) of the close under EMA(46)): enter when the bar opens inside
//! the Keltner channel (SMA(81) of the close, 2.5 times the 81-bar SMA of the true range either side) and closes above it,
//! with +DI(19) above -DI(19) and above 27. While held, two targets on the average price: 15% of the entry at 4.5% up, the
//! rest at 20% up. Closed at the next open when a close is under the channel or a low under 4% below the last entry signal's
//! close. 0.07% commission; starts with 1,000.
//!
//! The DI lines are the rule's `dirmov`: Wilder averages (SMA-seeded) of the moves and of the true range, each held at its
//! last value while it has none, a move against the missing bar before the first counting as 0 (`Adx::with_first_bar_moves`,
//! `Adx::with_held_lines`). A target's share of the entry floors to the quantity step.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema, Sma, TrueRange};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "Long";
const LENGTH: usize = 81;
const MULTIPLIER: f64 = 2.5;
const BENCHMARK: f64 = 27.0;
const FIRST_TARGET: f64 = 0.045;
const FIRST_SHARE: f64 = 0.15;
const SECOND_TARGET: f64 = 0.20;
const STOP: f64 = 0.04;

struct S {
    lead_ema: Ema,
    lead_sma: Sma,
    middle: Sma,
    range: RollingSum,
    dmi: Adx,
    true_range: TrueRange,
    /// The close of the last bar that met the entry signal.
    signal_close: f64,
    /// The size the open position was entered with.
    entered: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        lead_ema: Ema::new(46.0).expect("a valid length"),
        lead_sma: Sma::new(46.0).expect("a valid length"),
        middle: Sma::new(LENGTH as f64).expect("a valid length"),
        range: RollingSum::new(LENGTH),
        dmi: Adx::new(19.0)
            .expect("a valid length")
            .with_first_bar_moves(true)
            .with_held_lines(true),
        true_range: TrueRange::default().with_first_bar_missing(true),
        signal_close: f64::NAN,
        entered: 0.0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.07), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let up_trend = self.lead_sma.next(bar) < self.lead_ema.next(bar);
        // The first bar has no close before it, so no true range.
        let true_range = self.true_range.next(bar);
        let middle = self.middle.next(bar);
        let reach = self.range.next(true_range) / LENGTH as f64 * MULTIPLIER;
        let (upper, lower) = (middle + reach, middle - reach);
        let [_, plus_di, minus_di] = self.dmi.next(bar);

        let entry = bar.open > lower
            && bar.open < upper
            && bar.close > upper
            && plus_di > minus_di
            && plus_di > BENCHMARK;
        if entry {
            self.signal_close = bar.close;
        }
        let exit = bar.close < lower || bar.low < self.signal_close * (1.0 - STOP);

        let held = strategy.position();
        if held == 0.0 {
            self.entered = 0.0;
        } else if self.entered == 0.0 {
            self.entered = held;
        }
        if up_trend && entry {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if held > 0.0 {
            let cost = strategy.avg_cost();
            let first = sizing.floor(self.entered * FIRST_SHARE);
            let take = cost * (1.0 + FIRST_TARGET);
            strategy.exit("TP1").from(NAME).size(first).take(take);
            let take = cost * (1.0 + SECOND_TARGET);
            let rest = self.entered - first;
            strategy.exit("TP2").from(NAME).size(rest).take(take);
        }
        if exit {
            strategy.close(NAME);
        }
    }
}
