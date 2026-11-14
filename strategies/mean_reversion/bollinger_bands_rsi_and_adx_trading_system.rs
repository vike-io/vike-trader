// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Bollinger Bands, RSI and ADX Trading System" by exlux, https://www.tradingview.com/script/sR1hePBi/
// The original's notice: © exlux99
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when RSI(20) crosses up through 35 with the close under the upper Bollinger band (SMA(60) of the close and 2
//! deviations) and ADX(14, 14) under 32; short 1 when RSI(20) crosses down through 65 with the close ABOVE the upper band and
//! the same ADX filter, each reversing the other. Each entry carries a target and a stop 90 ticks from its fill, live from that
//! fill; a long also closes at the next open when the close crosses down through the SMA(60), a short when it crosses up. No
//! costs; starts with 100,000.
//!
//! The ADX is the rule's own: Wilder averages of the directional moves and of the true range, both missing on the first bar,
//! each line held at its last number when it has none (`Adx::with_held_lines`), and `100 *` the Wilder average of
//! `|+DI - -DI| / (+DI + -DI)`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Bollinger, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const LENGTH: f64 = 14.0;
/// The target's and the stop's distance from the entry, in ticks of the instrument.
const WIDTH_TICKS: f64 = 90.0;

struct S {
    rsi: Rsi,
    bands: Bollinger,
    adx: Adx,
    oversold: Cross,
    overbought: Cross,
    basis: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(20.0).expect("a valid length"),
        bands: Bollinger::new(60.0, 2.0).expect("valid parameters"),
        adx: Adx::new(LENGTH).expect("a valid length").with_held_lines(true),
        oversold: Cross::new(),
        overbought: Cross::new(),
        basis: Cross::new(),
    })
}

/// The target and stop of what is held, `WIDTH_TICKS` ticks of `tick` from its entry.
fn protect(strategy: &mut Tactics<'_>, tick: f64) {
    let width = WIDTH_TICKS * tick;
    let held = strategy.position();
    let cost = strategy.avg_cost();
    if held > 0.0 {
        strategy.exit("X_long").from(UP).take(cost + width).stop(cost - width);
    } else if held < 0.0 {
        strategy.exit("x_short").from(DOWN).take(cost - width).stop(cost + width);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let rsi = self.rsi.next(bar);
        let [upper, basis, _] = self.bands.next(bar);
        let [adx, _, _] = self.adx.next(bar);
        let up = matches!(self.oversold.step(rsi, 35.0), Signal::Long);
        let down = matches!(self.overbought.step(rsi, 65.0), Signal::Short);
        let through = self.basis.step(bar.close, basis);
        let calm = adx < 32.0;

        if bar.close < upper && up && calm {
            strategy.entry(UP, Long).size(1.0);
        }
        protect(strategy, tick);
        if matches!(through, Signal::Short) {
            strategy.close(UP);
        }
        if bar.close > upper && down && calm {
            strategy.entry(DOWN, Short).size(1.0);
        }
        if matches!(through, Signal::Long) {
            strategy.close(DOWN);
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        protect(strategy, tick);
    }
}
