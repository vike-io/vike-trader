// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI and Smoothed RSI Bull Div Strategy [BigBitsIO]" by BigBitsIO, https://www.tradingview.com/script/NC603cui/
// The original's notice: © BigBitsIO
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth on a bullish RSI divergence: RSI(14) under 30, its Hull average of length 10 under 35 and
//! falling for 3 bars or more, that average's lowest over the last 25 bars under 25, and the RSI above its own lowest of those
//! 25 bars while the close is under the close of the bar that lowest came on. Each such bar also sets a target 3% and a stop
//! 1.75% of its close away from the entry's fill price, so the first is set when the entry fills and a later one moves them.
//! Closed at the next open when the RSI is over 65. 0.1% commission; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Hma, Rsi};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const LOOKBACK: usize = 25;

/// The lowest number of a window, NaN skipped, NaN when it holds none.
// Not vike's `Donchian`: this answers from the window's first number (Donchian from its `length`-th bar), and the bar of the
// lowest is the newest of a tie (Donchian's offsets take the older).
fn lowest(values: impl Iterator<Item = f64>) -> f64 {
    values.filter(|v| !v.is_nan()).fold(f64::NAN, f64::min)
}

struct S {
    rsi: Rsi,
    /// Hull's average of the RSI, `WMA(2 WMA(n/2) - WMA(n), round(sqrt n))`, fed from its first number.
    hull: Hma,
    previous_hull: f64,
    falling: usize,
    /// `(rsi, close, hull)` of the last 25 bars, newest first.
    window: VecDeque<(f64, f64, f64)>,
    /// The target's and the stop's distance in ticks the last signal set.
    ticks: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(14.0).expect("a valid length"),
        hull: Hma::new(10.0).expect("a valid length").with_rounded_root(true),
        previous_hull: f64::NAN,
        falling: 0,
        window: VecDeque::with_capacity(LOOKBACK + 1),
        ticks: (f64::NAN, f64::NAN),
    })
}

impl S {
    fn protect(&self, strategy: &mut Tactics<'_>, tick: f64) {
        if strategy.position() > 0.0 && !self.ticks.0.is_nan() {
            let cost = strategy.avg_cost();
            strategy
                .exit("bracket")
                .from(NAME)
                .take(cost + self.ticks.0 * tick)
                .stop(cost - self.ticks.1 * tick);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let rsi = self.rsi.next(bar);
        let hull = if rsi.is_nan() { f64::NAN } else { self.hull.next_value(rsi) };
        self.falling = if hull < self.previous_hull { self.falling + 1 } else { 0 };
        self.previous_hull = hull;
        self.window.push_front((rsi, bar.close, hull));
        self.window.truncate(LOOKBACK);
        let low_rsi = lowest(self.window.iter().map(|w| w.0));
        let low_hull = lowest(self.window.iter().map(|w| w.2));
        // The newest bar holding the lowest RSI, as the reference's `lowestbars` finds it.
        let target = self.window.iter().find(|w| w.0 == low_rsi).map_or(f64::NAN, |w| w.1);
        let buy = rsi < 30.0
            && hull < 35.0
            && low_hull < 25.0
            && bar.close < target
            && self.falling >= 3
            && rsi > low_rsi;
        if buy {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
            self.ticks = (bar.close * (3.0 / 100.0) / tick, bar.close * (1.75 / 100.0) / tick);
            self.protect(strategy, tick);
        }
        if rsi > 65.0 {
            strategy.close(NAME);
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        self.protect(strategy, tick);
    }
}
