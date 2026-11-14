// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Optimized Trend Tracker" by KivancOzbilgic, https://www.tradingview.com/script/HPbRZvsV/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! The optimized trend tracker on a 2-bar variable index average of the close: a trailing level 1.4% under the average (over
//! it when the trend is down), pushed 0.7% further out. Long 1 when the average crosses up through that line of two bars ago,
//! short 1 when it crosses down through it, each reversing the other. No costs; starts with 100,000.
//!
//! The rule also reads the same line on twenty other symbols, but only to write a label: no order reads them.
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

use vike_indicators::indicators::{Supertrend, Vidya};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: f64 = 2.0;
const PERCENT: f64 = 1.4;

struct S {
    average: Vidya,
    stop: Supertrend,
    /// The line, newest first, 3 of them.
    ott: VecDeque<f64>,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Vidya::new(LENGTH, 9.0).expect("valid lengths").with_zero_seed(true),
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
        let m = self.average.next_value(bar.close);
        let fark = m * PERCENT * 0.01;
        let [mt, _] = self.stop.next_centred(m, m, fark);
        let ott =
            if m > mt { mt * (200.0 + PERCENT) / 200.0 } else { mt * (200.0 - PERCENT) / 200.0 };
        self.ott.push_front(ott);
        self.ott.truncate(3);
        let back = self.ott.get(2).copied().unwrap_or(f64::NAN);
        match self.cross.step(m, back) {
            Signal::Long => {
                strategy.entry("Long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("Short", Short).size(1.0);
            }
            _ => {}
        }
    }
}
