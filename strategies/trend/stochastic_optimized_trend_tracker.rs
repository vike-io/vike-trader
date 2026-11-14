// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Stochastic Optimized Trend Tracker" by lucidreamer15, https://www.tradingview.com/script/cJhpIJ2w/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! The optimized trend tracker on a stochastic. A variable-index average (an EMA whose weight is scaled by the 9-bar Chande
//! momentum of its input) of the 250-bar stochastic, length 50, plus 1000, is the line; the same average of that line, length
//! 3, is the support, and the tracker trails the support by 0.618% as a supertrend does, then moves 0.309% further off it.
//! Long 1 when the line crosses up through the tracker as it stood two bars before, short 1 when it crosses down, each
//! reversing the other. No costs; starts with 100,000.
//!
//! As the rule's `nz` reads it, an undefined input adds nothing to an average, which starts from 0, and a momentum with no
//! movement in its 9 bars is 0.
//!
//! The trailing level is vike's `Supertrend` about the average with the average as its price and the percent as its band
//! (`Supertrend::next_centred`), judged against the last bar's bands with gated turns and carried on this bar's price
//! (`Supertrend::with_previous_bands`, `Supertrend::with_gated_turns`, `Supertrend::with_carry_on_close`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Stochastic, Supertrend, Vidya};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const PERCENT: f64 = 0.618;

struct S {
    /// The raw %K (a smoothing of 1 passes it through).
    stoch: Stochastic,
    k: Vidya,
    support: Vidya,
    stop: Supertrend,
    /// The tracker, newest first, three bars.
    ott: VecDeque<f64>,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let var = |n: f64| Vidya::new(n, 9.0).expect("valid lengths").with_zero_seed(true);
    Box::new(S {
        stoch: Stochastic::new(250.0, 1.0, 1.0).expect("valid lengths").with_fresh_sum(true),
        k: var(50.0),
        support: var(3.0),
        stop: Supertrend::new(10.0, 1.0)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true)
            .with_carry_on_close(true),
        ott: VecDeque::with_capacity(3),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let stochastic = self.stoch.next(bar)[0];
        let line = self.k.next_value(stochastic) + 1000.0;
        let avg = self.support.next_value(line);
        let off = avg * PERCENT * 0.01;

        let [stop, _] = self.stop.next_centred(avg, avg, off);
        let ott = if avg > stop {
            stop * (200.0 + PERCENT) / 200.0
        } else {
            stop * (200.0 - PERCENT) / 200.0
        };
        self.ott.push_front(ott);
        self.ott.truncate(3);
        let back = self.ott.get(2).copied().unwrap_or(f64::NAN);
        match self.cross.step(line, back) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
