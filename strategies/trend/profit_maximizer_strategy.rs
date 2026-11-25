// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Profit Maximizer Strategy" by SoftKill21, https://www.tradingview.com/script/7yn5dteO/
// Changed: rewritten by hand in Rust for vike.

//! PMax on the bar's midpoint: a trailing stop 3 SMA(10)-of-true-range from the variable average (VAR, 12), under it while the
//! trend is up and over it while down, each side moving only with the average. Long 1 when the average crosses up through
//! PMax with the MACD histogram (SMA 12 less SMA 26, less its EMA 9) positive, the rule's parabolic SAR (0.02, 0.03, 0.3)
//! rising and the close at or over its SMA(20) (CCI(20) not negative); short 1 on the mirror. Each trade carries a stop 1% and
//! a target 35% of the latest close from its entry, moved on every bar; an entry's first pair is from its signal bar's close,
//! live from its fill. No costs; starts with 100,000.
//!
//! VAR is `Vidya::with_zero_seed`: `alpha * |CMO| * src + (1 - alpha * |CMO|) * VAR[1]` with `alpha = 2 / 13` and CMO the 9-bar
//! rolling sums of the midpoint's rises and falls, 0 while they have no value. The true range has no value on the first bar.
//! The stop line is vike's `Supertrend` about the average with the average as its price (`Supertrend::next_centred`), judged
//! against the last bar's bands with gated turns and carried on this bar's price (`Supertrend::with_previous_bands`,
//! `Supertrend::with_gated_turns`, `Supertrend::with_carry_on_close`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Psar, Sma, Supertrend, TrueRange, Vidya};
use vike_indicators::strategy_kit::parabolic_sar;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MULTIPLIER: f64 = 3.0;
const LOSS: f64 = 0.01;
const PROFIT: f64 = 0.35;

struct S {
    range: TrueRange,
    var: Vidya,
    atr: Sma,
    stop: Supertrend,
    cross: Cross,
    sar: Psar,
    fast: Sma,
    slow: Sma,
    signal: Ema,
    basis: Sma,
    /// The exit distances of this close, which an entry filling at the next open takes.
    distances: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        range: TrueRange::default().with_first_bar_missing(true),
        var: Vidya::new(12.0, 9.0).expect("valid lengths").with_zero_seed(true),
        atr: sma(10.0),
        stop: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true)
            .with_carry_on_close(true),
        cross: Cross::new(),
        sar: parabolic_sar(0.02, 0.03, 0.3),
        fast: sma(12.0),
        slow: sma(26.0),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        basis: sma(20.0),
        distances: (f64::NAN, f64::NAN),
    })
}

impl S {
    fn protect(&self, strategy: &mut Tactics<'_>, (loss, profit): (f64, f64)) {
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy.exit("x_long").from("long").take(cost + profit).stop(cost - loss);
        } else if held < 0.0 {
            strategy.exit("x_short").from("short").take(cost - profit).stop(cost + loss);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let average = self.var.next_value((bar.high + bar.low) / 2.0);
        let range = self.range.next(bar);
        // The SMA is not fed the first bar's missing range.
        let atr = if range.is_nan() { f64::NAN } else { self.atr.next_value(range) };

        let [pmax, _] = self.stop.next_centred(average, average, atr);
        let cross = self.cross.step(average, pmax);

        self.sar.next(bar);
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let hist = macd - self.signal.next_value(macd);
        let cci_up = bar.close - self.basis.next(bar) >= 0.0;
        let rising = self.sar.uptrend() == Some(true);
        let falling = self.sar.uptrend() == Some(false);
        if matches!(cross, Signal::Long) && hist > 0.0 && rising && cci_up {
            strategy.entry("long", Long).size(1.0);
        }
        if matches!(cross, Signal::Short) && hist < 0.0 && falling && !cci_up {
            strategy.entry("short", Short).size(1.0);
        }
        self.distances = (bar.close * LOSS / tick * tick, bar.close * PROFIT / tick * tick);
        self.protect(strategy, self.distances);
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        self.protect(strategy, self.distances);
    }
}
