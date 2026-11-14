// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TSI CCI Hull" by SeaSide420, https://www.tradingview.com/script/MuAKk43D/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-27, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long all of the account's worth, or short it, reversing, when three readings of the open agree. The true strength index
//! of the open (EMA(50) of EMA(50) of its change, over the same of its size, times 5) smoothed by EMA(125) is above its value
//! two bars back; a Hull-style average of the open (`n1`: WMA(5) of 2 WMA(13) less WMA(25)) rises, a commodity channel index
//! of the open around it (`(open - n1) / (0.015 * mean deviation(25))`) is above 0 and rising, and WMA(5) of 2 WMA(12) less
//! WMA(25) of `n1` is under the open. Short on the mirror. No costs; version 4: starts with 100,000.
//!
//! The rule evaluates on every tick; on a closed bar that is its close. Its half length of 25 is read two ways, and only this
//! pair matches (350 / 5172.478524 against 344 / -4279.68 with both 12 and 346 / -9304.79 with 12 and 13): `round(25 / 2)`
//! is 13, and a bare `25 / 2` as an average's length is cut to 12.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Hma, Mad};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const PERIOD: f64 = 25.0;

struct S {
    change: [Ema; 2],
    size: [Ema; 2],
    signal: Ema,
    last_open: f64,
    /// The mean absolute deviation of the last 25 opens from their mean.
    deviation: Mad,
    /// The Hull-style average of the open: `wma(2 * wma(open, 13) - wma(open, 25), 5)`, called `n1`: vike's `Hma` of 25 with
    /// the half rounded up.
    first: Hma,
    /// The same of `n1`, with the cut half length: `wma(2 * wma(n1, 12) - wma(n1, 25), 5)`, vike's `Hma` of 25.
    hull: Hma,
    /// The signal line, newest last, three of them.
    lines: Vec<f64>,
    last_n1: f64,
    last_cci: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    let hma = || Hma::new(PERIOD).expect("a valid length");
    Box::new(S {
        change: [ema(50.0), ema(50.0)],
        size: [ema(50.0), ema(50.0)],
        signal: ema(125.0),
        last_open: f64::NAN,
        deviation: Mad::new(PERIOD).expect("a valid length"),
        first: hma().with_rounded_half(true),
        hull: hma(),
        lines: Vec::new(),
        last_n1: f64::NAN,
        last_cci: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let p = bar.open;
        let change = p - std::mem::replace(&mut self.last_open, p);
        let [c1, c2] = &mut self.change;
        let [s1, s2] = &mut self.size;
        let tsi = 100.0
            * (c2.next_value(c1.next_value(change)) / s2.next_value(s1.next_value(change.abs())));
        let line = self.signal.next_value(tsi * 5.0);
        self.lines.push(line);
        let two_back =
            if self.lines.len() > 2 { self.lines[self.lines.len() - 3] } else { f64::NAN };

        let n1 = self.first.next_value(p);
        let last_n1 = std::mem::replace(&mut self.last_n1, n1);
        let cci = (p - n1) / (0.015 * self.deviation.next_value(p));
        let last_cci = std::mem::replace(&mut self.last_cci, cci);
        let hull = if n1.is_nan() { f64::NAN } else { self.hull.next_value(n1) };

        let long = line > two_back && hull < p && cci > 0.0 && cci > last_cci && n1 > last_n1;
        let short = line < two_back && hull > p && cci < 0.0 && cci < last_cci && n1 < last_n1;
        if long {
            strategy.entry("Buy Here", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if short {
            strategy.entry("Sell Here", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
