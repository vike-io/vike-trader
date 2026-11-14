// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "DEMA ATR Strategy [PrimeAutomation]" by ChartPrime, https://www.tradingview.com/script/rqRd3f62/
// The original's notice: © PrimeAutomation
// Changed: rewritten by hand in Rust for vike.

//! Long 10% of the account's worth, at the bar's own close, when a trailing line crosses up through itself two bars back and
//! the position is not long; short 10% on the mirror, from a long only. Each entry first closes the other side. The line
//! starts at DEMA(75) and from then on stays where it was unless the DEMA's band, 1.2 ATR(20) either side, has moved past it,
//! which drags it to the band's edge. Once an entry is made three targets stand, 1, 2 and 3 times ATR(200) times 3 from its
//! close; the first bar after it whose high (a short's low) is past one closes 30%, 30% and 40% of the position then held,
//! at that bar's close. A target is spent once taken. 0.06% commission, the whole of an entry's on its first closed row
//! (`[engine] whole_entry_fee`); starts with 10,000, margin none.
//!
//! The rule's other calls are no part of it: the library import and the 5-minute market-close call only feed an exit that is
//! off, and the closed-trade reads only build alert text. Every order fills at the close of the bar that gave it, and the
//! rule reads the position as it stood before that fill, so a target is first tested on the bar after the entry.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use std::collections::VecDeque;
use vike_indicators::indicators::{Atr, Dema};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SHARES: [f64; 3] = [0.30, 0.30, 0.40];

struct S {
    dema: Dema,
    band: Atr,
    wide: Atr,
    /// The line, newest first: this bar's, then the three before it.
    line: VecDeque<f64>,
    cross_up: Cross,
    cross_down: Cross,
    /// The three targets still standing, for a long and for a short.
    long: [f64; 3],
    short: [f64; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        dema: Dema::new(75.0).expect("a valid length"),
        band: Atr::new(20.0).expect("a valid length"),
        wide: Atr::new(200.0).expect("a valid length"),
        line: VecDeque::with_capacity(4),
        cross_up: Cross::new(),
        cross_down: Cross::new(),
        long: [f64::NAN; 3],
        short: [f64::NAN; 3],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let close = bar.close;
        let dema = self.dema.next_value(close);
        let range = self.band.next(bar) * 1.2;
        let target = self.wide.next(bar) * 3.0;
        let (upper, lower) = (dema + range, dema - range);
        // `nz(line[1], dema)`, then dragged to the band's edge if it lies past it.
        let mut line = self.line.front().copied().filter(|x| !x.is_nan()).unwrap_or(dema);
        if lower > line {
            line = lower;
        }
        if upper < line {
            line = upper;
        }
        self.line.push_front(line);
        self.line.truncate(4);
        let two_back = self.line.get(2).copied().unwrap_or(f64::NAN);
        let up = matches!(self.cross_up.step(line, two_back), Signal::Long);
        let down = matches!(self.cross_down.step(line, two_back), Signal::Short);

        let held = strategy.position();
        if up && held <= 0.0 {
            strategy.close("Short");
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, close));
            self.long = [1.0, 2.0, 3.0].map(|k| close + target * k);
            self.short = [f64::NAN; 3];
        }
        if down && held > 0.0 {
            strategy.close("Long");
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, close));
            self.short = [1.0, 2.0, 3.0].map(|k| close - target * k);
            self.long = [f64::NAN; 3];
        }

        // The position as it stood before this bar's orders.
        let size = held.abs();
        let name = if held > 0.0 { "Long" } else { "Short" };
        for (i, share) in SHARES.iter().enumerate() {
            let hit = if held > 0.0 { bar.high > self.long[i] } else { bar.low < self.short[i] };
            if held == 0.0 || !hit {
                continue;
            }
            strategy.close(name).size(floor_share(size * share, size, sizing.qty_step()));
            if held > 0.0 {
                self.long[i] = f64::NAN;
            } else {
                self.short[i] = f64::NAN;
            }
        }
    }
}
