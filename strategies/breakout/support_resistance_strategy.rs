// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Support Resistance Strategy" by ceyhun, https://www.tradingview.com/script/6GeSAYfE/
// The original's notice: © ceyhun
// Changed: rewritten by hand in Rust for vike.

//! Each time the close and its SMA(10) cross either way, the 10-bar highest high is a resistance and the 10-bar lowest low a
//! support; the levels in force are the ones taken at the cross before the latest. Long 1 when the close crosses up through
//! the resistance, short 1 when it crosses down through the support, each reversing the other. No costs; starts with 100,000.
//!
//! The either-way cross is `vike_tactics::rolling::CrossEither`: the reference's differs from a cross up or down at an equality.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Donchian;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::CrossEither;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 10.0;

struct S {
    sma: IndicatorLine,
    range: Donchian,
    turn: CrossEither,
    /// `(resistance, support)` at the latest cross, and at the one before it.
    latest: Option<(f64, f64)>,
    levels: Option<(f64, f64)>,
    up: Cross,
    down: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        sma: IndicatorLine::new("sma", &[LENGTH]),
        range: Donchian::new(LENGTH).expect("a valid length"),
        turn: CrossEither::default(),
        latest: None,
        levels: None,
        up: Cross::new(),
        down: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sma = self.sma.next(bar)[0];
        let [highest, _, lowest] = self.range.next(bar);
        if self.turn.step(sma, bar.close) {
            self.levels = self.latest.replace((highest, lowest));
        }
        let (resistance, support) = self.levels.unwrap_or((f64::NAN, f64::NAN));
        if matches!(self.up.step(bar.close, resistance), Signal::Long) {
            strategy.entry("long", Long).size(1.0);
        }
        if matches!(self.down.step(bar.close, support), Signal::Short) {
            strategy.entry("short", Short).size(1.0);
        }
    }
}
