// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Gaussian Channel Strategy" by RezzoRedPriest, https://www.tradingview.com/script/erJi1tWf/
// The original's notice: © RezzoRedPriest
// The original's notice: This strategy based on © DonovanWall indicator but upgraded from v4 to v6 PineScript and strategi functions added.
// Changed: rewritten by hand in Rust for vike.

//! The Gaussian channel's middle line: a 4-pole Gaussian filter of the typical price `(high + low + close) / 3`, sampling
//! period 144. Long all of the account's worth when the typical price crosses up through it, or is above it within 3 bars
//! of such a cross; short all of it when the typical price crosses down, or is below it within 3 bars of that. A signal
//! against the position closes it and enters the other side; one with the position does nothing. No costs.
//!
//! The header states no capital and no margin, so version 6's 100,000 and 100% apply. An entry sized at the signal's close
//! overshoots the account when the next open is higher, and the engine's margin call (`[engine.margin_call]`) cuts it at
//! that open, one whole unit when the shortfall is under one step, each cut a trade of its own.
//!
//! The filter is vike's `GaussianFilter` (Ehlers' 4-pole Gaussian filter, from 0): only the 4-pole one is read here, as the
//! rule's bands and its fast and reduced-lag modes are off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::GaussianFilter;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const POLES: f64 = 4.0;
const PERIOD: f64 = 144.0;
const LOOKBACK: usize = 3;

struct S {
    filter: GaussianFilter,
    cross: Cross,
    /// Bars since the typical price last crossed up and down through the filter, `None` before the first.
    since_up: Option<usize>,
    since_down: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        filter: GaussianFilter::new(PERIOD, POLES).expect("valid parameters"),
        cross: Cross::new(),
        since_up: None,
        since_down: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = (bar.high + bar.low + bar.close) / 3.0;
        let filt = self.filter.next_value(src);
        let cross = self.cross.step(src, filt);
        let tick =
            |since: Option<usize>, now: bool| if now { Some(0) } else { since.map(|n| n + 1) };
        self.since_up = tick(self.since_up, matches!(cross, Signal::Long));
        self.since_down = tick(self.since_down, matches!(cross, Signal::Short));
        let recent = |since: Option<usize>| since.is_some_and(|n| n <= LOOKBACK);
        let long = matches!(cross, Signal::Long) || (src > filt && recent(self.since_up));
        let short = matches!(cross, Signal::Short) || (src < filt && recent(self.since_down));
        let held = strategy.position();
        if long && held <= 0.0 {
            if held < 0.0 {
                strategy.close("short");
            }
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if short && held >= 0.0 {
            if held > 0.0 {
                strategy.close("long");
            }
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
