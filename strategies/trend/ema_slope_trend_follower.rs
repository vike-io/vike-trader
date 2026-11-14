// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Slope Trend Follower" by lukescream, https://www.tradingview.com/script/TKg40Ska/
// The original's notice: © lukescream
// Changed: rewritten by hand in Rust for vike.

//! The slope of EMA(130) of the close (its change over its value), smoothed by EMA(9) and EMA(21). Long 30% of the account's
//! worth while the fast is above the slow and the close above EMA(200), short 30% while the fast is below and the close below;
//! a long is closed when the fast crosses down through the slow, a short when it crosses up, before the entries are asked.
//! 0.06% commission and 2 ticks of slippage; starts with 10,000.
//!
//! Both smoothings skip the slope's first NaN bars. The rule's volatility filter is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Ema;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const SHARE: f64 = 30.0;

struct S {
    average: Ema,
    last: f64,
    fast: Ema,
    slow: Ema,
    trend: Ema,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        average: Ema::new(130.0).expect("a valid length"),
        last: f64::NAN,
        fast: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        slow: Ema::new(21.0).expect("a valid length").with_skip_nan(true),
        trend: Ema::new(200.0).expect("a valid length"),
        cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let average = self.average.next_value(bar.close);
        let slope = (average - self.last) / average;
        self.last = average;
        let fast = self.fast.next_value(slope);
        let slow = self.slow.next_value(slope);
        let trend = self.trend.next_value(bar.close);
        let cross = self.cross.step(fast, slow);
        let held = strategy.position();
        if matches!(cross, Signal::Long) && held < 0.0 {
            strategy.close("short");
        }
        if matches!(cross, Signal::Short) && held > 0.0 {
            strategy.close("long");
        }
        if fast > slow && bar.close > trend {
            let size = share_of_worth(&sizing, SHARE, sizing.slipped(Long, bar.close));
            strategy.entry("long", Long).size(size);
        }
        if fast < slow && bar.close < trend {
            let size = share_of_worth(&sizing, SHARE, sizing.slipped(Short, bar.close));
            strategy.entry("short", Short).size(size);
        }
    }
}
