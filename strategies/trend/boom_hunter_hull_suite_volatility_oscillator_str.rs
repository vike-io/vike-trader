// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Boom Hunter + Hull Suite + Volatility Oscillator Strategy" by kevinmck100, https://www.tradingview.com/script/McuqT8px/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-05-31, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three indicators must agree. Boom Hunter: two Ehlers early-onset trend oscillators (supersmoother periods 6 and 28, the
//! second with K 0.3), the first crossing the second. Hull Suite: a Hull average of length 480 against itself 2 bars ago, the close
//! on the same side of it. Volatility oscillator: the bar's body against its 80-bar standard deviation. Long (short) when the
//! oscillator crosses up (down) with the Hull trend up (down) and the body over (under) one deviation, with no position that
//! side. The size risks 3% of the account's worth on the distance from the close to the stop (the lower of the close less ATR(14)
//! and the 20-bar low), in coin, floored to the step; the exit is stated once with the entry: that stop and a target 3 times
//! the distance. 0.01% commission, no account currency; starts with 1,000.
//!
//! The rule's higher-timeframe call for the Hull average is off by default and never runs here. The date window (from 2022)
//! holds every bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::{max, min};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Atr, Donchian, EarlyOnsetTrend, Hma, Stddev};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const SWING: f64 = 20.0;
const RISK: f64 = 3.0;
const FACTOR: f64 = 3.0;

/// One Ehlers early-onset trend oscillator, the closes before the first read as 0 as the rule reads them.
fn eot(lp: f64, k: f64) -> EarlyOnsetTrend {
    EarlyOnsetTrend::new(lp, k).expect("valid parameters").with_zero_history(true)
}

struct S {
    first: EarlyOnsetTrend,
    second: EarlyOnsetTrend,
    cross: Cross,
    /// The Hull average of 480: `WMA(2 * WMA(close, 240) - WMA(close, 480), 22)`, its last length `round(sqrt(480))`.
    hull: Hma,
    hulls: VecDeque<f64>,
    body: Stddev,
    atr: Atr,
    swing: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        first: eot(6.0, 0.0),
        second: eot(28.0, 0.3),
        cross: Cross::new(),
        hull: Hma::new(480.0).expect("a valid length").with_rounded_root(true),
        hulls: VecDeque::with_capacity(4),
        body: Stddev::new(80.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        swing: Donchian::new(SWING).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let q1 = self.first.next_value(bar.close) * 60.0 + 50.0;
        let q4 = self.second.next_value(bar.close) * 60.0 + 50.0;
        let boom = self.cross.step(q1, q4);

        let hull = self.hull.next_value(bar.close);
        self.hulls.push_front(hull);
        self.hulls.truncate(3);
        let (now, back) = (hull, self.hulls.get(2).copied().unwrap_or(f64::NAN));
        let hull_long = now > back && bar.close > now;
        let hull_short = now < back && bar.close < back;

        let spike = bar.close - bar.open;
        let dev = self.body.next_value(spike);
        let (vol_long, vol_short) = (spike > dev, spike < -dev);

        let [swing_high, _, swing_low] = self.swing.next(bar);
        let atr = self.atr.next(bar);

        let long = matches!(boom, Signal::Long) && hull_long && vol_long;
        let short = matches!(boom, Signal::Short) && hull_short && vol_short;
        let risk = strategy.equity() * RISK / 100.0;
        let close = bar.close;
        if long && strategy.position() <= 0.0 {
            let stop = min(close - atr, swing_low);
            let pct = ((1.0 - stop / close) * 100.0).abs();
            let qty = (risk / pct * 100.0).abs() / close;
            let target = close + close * (pct * FACTOR / 100.0);
            strategy.entry("Long", Long).size(sizing.floor(qty));
            strategy.exit("Long  SL/TP").from("Long").take(target).stop(stop);
        }
        if short && strategy.position() >= 0.0 {
            let stop = max(close + atr, swing_high);
            let pct = ((1.0 - stop / close) * 100.0).abs();
            let qty = (risk / pct * 100.0).abs() / close;
            let target = close - close * (pct * FACTOR / 100.0);
            strategy.entry("Short", Short).size(sizing.floor(qty));
            strategy.exit("Short  SL/TP").from("Short").take(target).stop(stop);
        }
    }
}
