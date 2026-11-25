// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "VWAP Trendfollow Strategy [wbburgin]" by wbburgin, https://www.tradingview.com/script/XoR8rQaW/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2023-05-25, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A VWAP of the typical price over the UTC day with its volume-weighted standard deviation, and bands 0.88 and 1.76 of that
//! deviation either side. Long 3% of the account's worth at the next open when the close crosses up through the inner upper
//! band while the VWAP has risen at least once since it last fell; short when the close crosses down through the inner lower
//! band while it has not; each reverses the other. A long is closed when the close crosses down through the outer upper
//! band, a short when it crosses up through the outer lower one. 0.025% commission; starts with 1,000.
//!
//! The rise count is the imported library's `trendUp` (`wbburgin_utils` 2 by wbburgin, MPL-2.0), ported here. The rule's
//! earnings, dividend and split requests look ahead, but this symbol has none and the session anchor reads none of them.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MULTIPLIER: f64 = 0.88;
const SHARE: f64 = 3.0;

struct S {
    /// The VWAP and its deviation (`Vwap::deviation`).
    line: Vwap,
    vwap: f64,
    rises: f64,
    inner_up: Cross,
    inner_down: Cross,
    outer_up: Cross,
    outer_down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        line: Vwap::default().with_source(VwapSource::TypicalDivided),
        vwap: f64::NAN,
        rises: 0.0,
        inner_up: Cross::new(),
        inner_down: Cross::new(),
        outer_up: Cross::new(),
        outer_down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.025), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let vwap = self.line.next(bar);
        let deviation = self.line.deviation();
        // The rule reads the deviation back off the call's upper band, `(vwap + deviation) - vwap`.
        let spread = vwap + deviation - vwap;
        let (inner, outer) = (spread * MULTIPLIER, 2.0 * spread * MULTIPLIER);
        self.rises = if vwap > self.vwap {
            self.rises + 1.0
        } else if vwap < self.vwap {
            0.0
        } else {
            self.rises
        };
        self.vwap = vwap;
        let rising = self.rises != 0.0;
        let buy = matches!(self.inner_up.step(bar.close, vwap + inner), Signal::Long) && rising;
        let sell =
            matches!(self.inner_down.step(bar.close, vwap - inner), Signal::Short) && !rising;
        let take_long = matches!(self.outer_down.step(bar.close, vwap + outer), Signal::Short);
        let take_short = matches!(self.outer_up.step(bar.close, vwap - outer), Signal::Long);
        if buy {
            strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if sell {
            strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if take_long {
            strategy.close("Long");
        }
        if take_short {
            strategy.close("Short");
        }
    }
}
