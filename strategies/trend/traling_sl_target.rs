// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Traling.SL.Target" by Sharad_Gaikwad, https://www.tradingview.com/script/4jxYYaGU/
// The original's notice: © Sharad_Gaikwad
// Changed: rewritten by hand in Rust for vike.

//! Long 1 when EMA(20) crosses up through EMA(50), short 1 when it crosses down, each only while flat, all filled at the
//! signal bar's close. A long's target starts 1% above its entry and its stop 1% below; each bar that closes 0.5% or more
//! above the reference price (the entry at first) raises the target and the stop by 0.3% of themselves and the reference by
//! 0.5%. The long is closed at the close of the bar that closes at or below the stop, or at or above the target. A short is
//! the mirror. No costs; starts with 1,000,000.
//!
//! The rule keeps trailing the last side's levels after its trade has closed; they are reset by the next entry, so that changes
//! nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const PROFIT: f64 = 0.01;
const LOSS: f64 = 0.01;
const TRAIL_AFTER: f64 = 0.005;
const TRAIL_TARGET: f64 = 0.003;
const TRAIL_STOP: f64 = 0.003;

/// One side's levels: the reference price trailing starts from, the target and the stop.
#[derive(Clone, Copy)]
struct Levels {
    entry: f64,
    target: f64,
    stop: f64,
}

impl Levels {
    const NONE: Levels = Levels { entry: f64::NAN, target: f64::NAN, stop: f64::NAN };
}

struct S {
    fast: IndicatorLine,
    slow: IndicatorLine,
    cross: Cross,
    /// `Some(LONG)` or `Some(SHORT)`: the side last entered.
    side: Option<OrderSide>,
    long: Levels,
    short: Levels,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: IndicatorLine::new("ema", &[20.0]),
        slow: IndicatorLine::new("ema", &[50.0]),
        cross: Cross::new(),
        side: None,
        long: Levels::NONE,
        short: Levels::NONE,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let fast = self.fast.next(bar)[0];
        let slow = self.slow.next(bar)[0];
        let cross = self.cross.step(fast, slow);
        let flat = strategy.position() == 0.0;
        let c = bar.close;

        let l = &mut self.long;
        if self.side == Some(Long) && c >= l.entry + l.entry * TRAIL_AFTER {
            l.target += l.target * TRAIL_TARGET;
            l.stop += l.stop * TRAIL_STOP;
            l.entry += l.entry * TRAIL_AFTER;
        }
        let s = &mut self.short;
        if self.side == Some(Short) && c <= s.entry - s.entry * TRAIL_AFTER {
            s.target -= s.target * TRAIL_TARGET;
            s.stop -= s.stop * TRAIL_STOP;
            s.entry -= s.entry * TRAIL_AFTER;
        }

        if matches!(cross, Signal::Long) && flat {
            self.side = Some(Long);
            self.long = Levels { entry: c, target: c + c * PROFIT, stop: c - c * LOSS };
            strategy.entry("long", Long).size(1.0);
        }
        if matches!(cross, Signal::Short) && flat {
            self.side = Some(Short);
            self.short = Levels { entry: c, target: c - c * PROFIT, stop: c + c * LOSS };
            strategy.entry("short", Short).size(1.0);
        }

        if c <= self.long.stop || c >= self.long.target {
            strategy.close("long");
        }
        if c >= self.short.stop || c <= self.short.target {
            strategy.close("short");
        }
    }
}
