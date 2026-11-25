// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Twin Optimized Trend Tracker" by KivancOzbilgic, https://www.tradingview.com/script/eENf7NpJ/
// The original's notice: © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! The twin optimized trend tracker on a 40-bar variable index average of the close. A trailing level 1% under the average
//! (over it when the trend is down) is pushed 0.5% further out, and two lines 0.1% either side of that make the twins. Long 1
//! when the average crosses up through the upper twin of two bars ago, short 1 when it crosses down through the lower one,
//! each reversing the other. No costs; starts with 1,000,000.
//!
//! The average starts at 0 and moves toward the close by `2 / 41` times the absolute 9-bar Chande momentum, read 0 until it
//! exists: `Vidya::with_zero_seed`, as the rule writes it.
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

const PERCENT: f64 = 1.0;
const COEFF: f64 = 0.001;

struct S {
    average: Vidya,
    stop: Supertrend,
    /// `(upper twin, lower twin)`, newest first, 3 of them.
    twins: VecDeque<(f64, f64)>,
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Vidya::new(40.0, 9.0).expect("valid lengths").with_zero_seed(true),
        stop: Supertrend::new(10.0, 1.0)
            .expect("valid parameters")
            .with_previous_bands(true)
            .with_gated_turns(true)
            .with_carry_on_close(true),
        twins: VecDeque::with_capacity(3),
        up: Cross::new(),
        down: Cross::new(),
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
        self.twins.push_front((ott * (1.0 + COEFF), ott * (1.0 - COEFF)));
        self.twins.truncate(3);
        let (upper, lower) = self.twins.get(2).copied().unwrap_or((f64::NAN, f64::NAN));
        let buy = matches!(self.up.step(m, upper), Signal::Long);
        let sell = matches!(self.down.step(m, lower), Signal::Short);
        if buy {
            strategy.entry("long", Long).size(1.0);
        }
        if sell {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
