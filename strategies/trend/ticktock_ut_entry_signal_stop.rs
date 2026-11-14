// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TickTock (UT entry + Signal stop)" by MrPencilBox, https://www.tradingview.com/script/hNPuoyrz/
// The original's notice: © MrPencilBox
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when flat and the close crosses up through a trailing line 2 ATR(1) from it (the line
//! ratchets up under a rising close and down over a falling one, and jumps to the other side when the close crosses it).
//! Closed when the close crosses down through the line, or by a stop at the signal line taken when the long was signalled:
//! the 10-bar SMA of the 11-bar regression line of the close, placed once the long is held. No costs; version 6 with no
//! size or capital: 100,000 at 100% of equity, margined at 100% (`main.rs`'s rule 4).
//!
//! The rule's Heikin-Ashi read is off by its inputs, so its line follows the chart's own close. The line is vike's
//! `AtrTrailingStop` started down (`with_down_start`): the rule reads a missing line as the close, so its first is
//! `close + loss`.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{AtrTrailingStop, Linearreg, Sma};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const KEY: f64 = 2.0;

struct S {
    regression: Linearreg,
    signal: Sma,
    trail: AtrTrailingStop,
    cross: Cross,
    /// The signal line when the long was signalled.
    stop: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        regression: Linearreg::new(11.0).expect("a valid length"),
        signal: Sma::new(10.0).expect("a valid length"),
        trail: AtrTrailingStop::new(1.0, KEY).expect("valid parameters").with_down_start(true),
        cross: Cross::new(),
        stop: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let signal = self.signal.next_value(self.regression.next_value(bar.close));
        let src = bar.close;
        let trail = self.trail.next(bar);
        let cross = self.cross.step(src, trail);
        let buy = src > trail && matches!(cross, Signal::Long);
        let sell = src < trail && matches!(cross, Signal::Short);
        let held = strategy.position();
        if buy && held <= 0.0 {
            self.stop = signal;
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if sell && held > 0.0 {
            strategy.close("Long");
        }
        if held > 0.0 && !self.stop.is_nan() {
            strategy.exit("Long-StopLoss").from("Long").stop(self.stop);
        }
    }
}
