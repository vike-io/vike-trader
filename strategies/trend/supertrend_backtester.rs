// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend Backtester" by DevLucem, https://www.tradingview.com/script/GMZLB374/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2019-10-30, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A supertrend of factor 3 on ATR(7) around the bar's midpoint. Long 1 on the bar it turns up, short 1 on the bar it turns
//! down, each reversing the other, and each closed by a target and a stop 100 ticks either side of its fill. No costs; starts
//! with 100,000.
//!
//! The bands: `up = hl2 - 3 * ATR` held at the larger of it and its last value while the last close stayed above that last
//! value, `down` the mirror. The trend turns up when the close passes the last `down`, down when it passes under the last `up`,
//! and starts up: vike's `Supertrend` judged against the last bar's bands (`Supertrend::with_previous_bands`). The target
//! and the stop need the fill's price, so they are issued from the fill and work from the next point
//! of the entry bar's walk.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FACTOR: f64 = 3.0;
/// The target's and the stop's distance from the fill: 100 ticks.
const AWAY: f64 = 1.0;
/// The trend before the first bar, which the rule compares the first bar's trend with; `None` reads as no turn.
const TREND_BEFORE: Option<i32> = None;

struct S {
    supertrend: Supertrend,
    trend: Option<i32>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        supertrend: Supertrend::new(7.0, FACTOR)
            .expect("valid parameters")
            .with_previous_bands(true),
        trend: TREND_BEFORE,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let trend = if self.supertrend.next(bar)[1] > 0.0 { 1 } else { -1 };
        let was = self.trend.replace(trend);
        if was.is_some_and(|w| w != trend) {
            if trend == 1 {
                strategy.entry("long", Long).size(1.0);
            } else {
                strategy.entry("short", Short).size(1.0);
            }
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let held = strategy.position();
        if held == 0.0 {
            return;
        }
        let entry = strategy.avg_cost();
        let (lots, toward) = if held > 0.0 { ("long", 1.0) } else { ("short", -1.0) };
        strategy
            .exit(&format!("exit {lots}"))
            .from(lots)
            .take(entry + toward * AWAY)
            .stop(entry - toward * AWAY);
    }
}
