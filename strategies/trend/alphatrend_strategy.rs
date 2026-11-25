// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "AlphaTrend Strategy" by KivancOzbilgic, https://www.tradingview.com/script/3wdQu7P3/
// The original's notice: author © KivancOzbilgic
// The original's notice: developer © KivancOzbilgic
// Changed: rewritten by hand in Rust for vike.

//! The AlphaTrend line: with the 14-bar money flow index of the typical price at 50 or more it is `low - ATR`, held at the
//! line before when that is higher; under 50 it is `high + ATR`, held at the line before when that is lower. Here ATR is the
//! 14-bar SMA of the true range, and the line before is 0 while it is undefined. Long 1 when the line crosses up through
//! itself two bars back, short 1 when it crosses down, each reversing the other. No costs; starts with 1,000,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Mfi, Sma, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: usize = 14;

struct S {
    tr: TrueRange,
    range: Sma,
    mfi: Mfi,
    /// The line on the last two bars, newest first.
    line: [f64; 2],
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        tr: TrueRange::default().with_first_bar_missing(true),
        range: Sma::new(LENGTH as f64).expect("a valid length"),
        mfi: Mfi::new(LENGTH as f64).expect("a valid length"),
        line: [f64::NAN; 2],
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The true range with no close before it is undefined.
        let atr = self.range.next_value(self.tr.next(bar));

        let mfi = self.mfi.next(bar);

        let before = nan_to_zero(self.line[0]);
        let line = if mfi >= 50.0 {
            let floor = bar.low - atr;
            if floor < before { before } else { floor }
        } else {
            let ceiling = bar.high + atr;
            if ceiling > before { before } else { ceiling }
        };
        let two_back = self.line[1];
        self.line = [line, self.line[0]];
        match self.cross.step(line, two_back) {
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
