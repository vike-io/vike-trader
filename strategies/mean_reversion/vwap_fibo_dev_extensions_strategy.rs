// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "VWAP + Fibo Dev Extensions Strategy" by UnknownUnicorn4889556, https://www.tradingview.com/script/EaBACrDi/
// The original's notice: © Mysteriown
// Changed: rewritten by hand in Rust for vike.

//! The week's VWAP of the typical price, restarted each Monday 00:00 UTC, with bands at 1.618 and 2.618 of the week's
//! volume-weighted deviation from it. Long 1 when the last bar's low crossed down through the lower inner band while staying
//! above the outer one, this bar's low lies between the two and the deviation is over 150; closed when the high crosses up
//! through the VWAP or the low crosses down through the outer band. Short the mirror image. Up to five entries a side, an
//! entry the other way reversing them all. 0.08% commission; starts with 100,000.
//!
//! The deviation is the rule's own running sum, `volume * (typical - last VWAP) * (typical - VWAP)`, restarted with the
//! week, over the week's volume. The first bar fed starts a week.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const INNER: f64 = 1.618;
const OUTER: f64 = 2.618;
const LEAST_DEVIATION: f64 = 150.0;
const DAY_MS: i64 = 86_400_000;
/// 1970-01-05, the first Monday of the epoch.
const FIRST_MONDAY_MS: i64 = 4 * DAY_MS;
const WEEK_MS: i64 = 7 * DAY_MS;

/// The rule's levels on one bar.
#[derive(Clone, Copy)]
struct Levels {
    low: f64,
    high: f64,
    lower_inner: f64,
    lower_outer: f64,
    upper_inner: f64,
    upper_outer: f64,
}

struct S {
    week: Option<i64>,
    money: f64,
    volume: f64,
    spread: f64,
    vwap: f64,
    last: Option<Levels>,
    /// The low through the lower inner band and the high through the upper inner band, one bar back.
    lower_inner_cross: Cross,
    upper_inner_cross: Cross,
    high_vwap: Cross,
    low_vwap: Cross,
    low_outer: Cross,
    high_outer: Cross,
    dipped: bool,
    poked: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        week: None,
        money: 0.0,
        volume: 0.0,
        spread: 0.0,
        vwap: f64::NAN,
        last: None,
        lower_inner_cross: Cross::new(),
        upper_inner_cross: Cross::new(),
        high_vwap: Cross::new(),
        low_vwap: Cross::new(),
        low_outer: Cross::new(),
        high_outer: Cross::new(),
        dipped: false,
        poked: false,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.08), pyramiding: 5, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        let week = (bar.timestamp - FIRST_MONDAY_MS).div_euclid(WEEK_MS);
        let starts = self.week != Some(week);
        self.week = Some(week);
        if starts {
            self.money = typical * bar.volume;
            self.volume = bar.volume;
        } else {
            self.money += typical * bar.volume;
            self.volume += bar.volume;
        }
        let vwap = self.money / self.volume;
        self.spread = if starts {
            0.0
        } else {
            self.spread + bar.volume * (typical - self.vwap) * (typical - vwap)
        };
        self.vwap = vwap;
        let deviation = (self.spread / self.volume).sqrt();
        let now = Levels {
            low: bar.low,
            high: bar.high,
            lower_inner: vwap - INNER * deviation,
            lower_outer: vwap - OUTER * deviation,
            upper_inner: vwap + INNER * deviation,
            upper_outer: vwap + OUTER * deviation,
        };

        let wide = deviation > LEAST_DEVIATION;
        let (bull, bear) = match self.last {
            Some(last) => (
                self.dipped
                    && last.low >= last.lower_outer
                    && now.low > now.lower_outer
                    && now.low < now.lower_inner
                    && wide,
                self.poked
                    && last.high <= last.upper_outer
                    && now.high < now.upper_outer
                    && now.high > now.upper_inner
                    && wide,
            ),
            None => (false, false),
        };
        self.dipped =
            matches!(self.lower_inner_cross.step(now.low, now.lower_inner), Signal::Short);
        self.poked = matches!(self.upper_inner_cross.step(now.high, now.upper_inner), Signal::Long);
        self.last = Some(now);

        let high_over_vwap = matches!(self.high_vwap.step(bar.high, vwap), Signal::Long);
        let low_under_vwap = matches!(self.low_vwap.step(bar.low, vwap), Signal::Short);
        let low_under_outer =
            matches!(self.low_outer.step(bar.low, now.lower_outer), Signal::Short);
        let high_over_outer =
            matches!(self.high_outer.step(bar.high, now.upper_outer), Signal::Long);

        if bull {
            strategy.entry("long", Long).size(1.0);
        }
        if high_over_vwap || low_under_outer {
            strategy.close("long");
        }
        if bear {
            strategy.entry("short", Short).size(1.0);
        }
        if low_under_vwap || high_over_outer {
            strategy.close("short");
        }
    }
}
