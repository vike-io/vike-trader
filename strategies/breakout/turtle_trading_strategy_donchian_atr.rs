// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Turtle trading strategy (Donchian/ATR)" by jeanobrega, https://www.tradingview.com/script/gbxblhGl/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-10-16, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when the close reaches the highest high of the 20 bars before it. Closed when the close reaches the lowest low of
//! the 10 bars before it, or falls to the stop line of the bar before: the 3-bar EMA of `close - 2 * ATR`, where ATR is the
//! 20-bar EMA of the true range. No costs; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema, TrueRange};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "buy";

struct S {
    range: Ema,
    stop: Ema,
    enter: Donchian,
    exit: Donchian,
    true_range: TrueRange,
    /// The bar before this one: highest high, lowest low and stop line.
    last: (f64, f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        range: Ema::new(20.0).expect("a valid length"),
        stop: Ema::new(3.0).expect("a valid length").with_skip_nan(true),
        enter: Donchian::new(20.0).expect("a valid length"),
        exit: Donchian::new(10.0).expect("a valid length"),
        true_range: TrueRange::default(),
        last: (f64::NAN, f64::NAN, f64::NAN),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        // The true range, the high less the low on the first bar.
        let atr = self.range.next_value(self.true_range.next(bar));
        let stop = self.stop.next_value(bar.close - 2.0 * atr);
        let upper = self.enter.next(bar)[0];
        let lower = self.exit.next(bar)[2];
        let (last_upper, last_lower, last_stop) = self.last;
        self.last = (upper, lower, stop);
        if bar.close >= last_upper {
            strategy.entry(NAME, Long).size(1.0);
        }
        if bar.close <= last_lower || bar.close <= last_stop {
            strategy.close(NAME);
        }
    }
}
