// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Custom Screener with Alerts [QuantNomad]" by QuantNomad, https://www.tradingview.com/script/QKgb0Fud/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-07-15, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A placeholder rule that trades only on the day the rule runs: from that UTC day's first bar, long 1 BTC on a bar whose bar
//! before closed flat, and closed on a bar whose bar before closed held. The rule reads the run's clock (`timenow`), the run's
//! last bar (`vike_tactics::Tactics::window`), so the day is that bar's; a run with no window (a paper mount) trades on every
//! bar, each being the day it runs. The RSIs of forty other symbols it screens feed only a label. Version 4 with nothing
//! stated: 1 BTC, no costs, 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const NAME: &str = "dummy_long";

struct S {
    /// The position as the bar before closed.
    held_before: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S { held_before: 0.0 })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let run_day = strategy.window().map(|(_, last)| last.div_euclid(MS_PER_DAY) * MS_PER_DAY);
        if run_day.is_none_or(|day| bar.timestamp >= day) {
            if self.held_before == 0.0 {
                strategy.entry(NAME, Long).size(1.0);
            } else {
                strategy.close(NAME);
            }
        }
        self.held_before = strategy.position();
    }
}
