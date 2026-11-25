// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by Peter_O, https://www.tradingview.com/script/BTSkg8IP/
// The original's notice: © Peter_O
// Changed: rewritten by hand in Rust for vike.

//! Stochastic %K(13) smoothed by SMA(4), %D its SMA(3). Long 20,000 when %K crosses up through %D under 80, short 20,000 when
//! it crosses down through %D over 20, each reversing the other. Each entry carries two exits, issued again at every close: half
//! of it leaves at a target 150 ticks past its fill, the other half at a target 400 ticks past it or at a stop. A long's stop is
//! the signal bar's low, or the last confirmed pivot low (one bar either side) when that is lower, set on each long signal and
//! kept until the next; a short's the mirror. A cost of 0.00003 a unit; starts with 1,000, so every position is far larger
//! than the account, which nothing refuses. **Does not reproduce its reference** (4381 trades, net -6000.899902 on
//! CAPITALCOM:EURUSD 4h, 2017-04-30..2026-09-09): checked on Dukascopy's EURUSD, another feed's prices, this file gives
//! 4397 / -5536.800000 on bars of the mid price, which it is run on (the nearer count), and 4400 / -6048.100000 on bars
//! of the bid; the owner accepted those numbers on 2026-11-16.
//!
//! Alerts, their trade counter and the plot are left out; the original's `year > 2009` holds on every bar it runs on.
//!
//! **Data:** the reference ran on CAPITALCOM:EURUSD, a CFD feed; this file is checked on Dukascopy's EURUSD, a forex feed.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{ConfirmedPivot, Stochastic};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const SIZE: f64 = 20_000.0;
const PARTIAL: TacticsTrail = TacticsTrail { after_ticks: 150.0, by_ticks: 0.0 };
const TARGET: TacticsTrail = TacticsTrail { after_ticks: 400.0, by_ticks: 0.0 };

struct S {
    stochastic: Stochastic,
    cross: Cross,
    pivots: ConfirmedPivot,
    /// The stops, each set on its side's signal; the first bar's low and high until then.
    long_stop: f64,
    short_stop: f64,
    started: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stochastic: Stochastic::new(13.0, 4.0, 3.0).expect("valid lengths"),
        cross: Cross::new(),
        pivots: ConfirmedPivot::new(1.0, 1.0).expect("valid spans"),
        long_stop: f64::NAN,
        short_stop: f64::NAN,
        started: false,
    })
}

/// The exits of `name`'s entry: half at the partial target, the other half at the target or at `stop` when it is a price.
/// Each is sized to its part of the entry: a reversal's order also carries the position it closes.
fn exits(strategy: &mut Tactics<'_>, name: &str, stop: f64) {
    strategy.exit(&format!("XPart{name}")).from(name).size(SIZE / 2.0).trail(PARTIAL);
    let exit = strategy.exit(&format!("X{name}"));
    exit.from(name).size(SIZE / 2.0).trail(TARGET);
    if !stop.is_nan() {
        exit.stop(stop);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_contract(0.00003), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if !std::mem::replace(&mut self.started, true) {
            (self.long_stop, self.short_stop) = (bar.low, bar.high);
        }
        let [k, d] = self.stochastic.next(bar);
        let [_, _, last_high, last_low] = self.pivots.next(bar);
        let signal = self.cross.step(k, d);
        let go_long = matches!(signal, Signal::Long) && k < 80.0;
        let go_short = matches!(signal, Signal::Short) && k > 20.0;
        if go_long {
            self.long_stop = if bar.low < last_low { bar.low } else { last_low };
        }
        if go_short {
            self.short_stop = if bar.high > last_high { bar.high } else { last_high };
        }
        if go_long {
            strategy.entry("Long", Long).size(SIZE);
        }
        exits(strategy, "Long", self.long_stop);
        if go_short {
            strategy.entry("Short", Short).size(SIZE);
        }
        exits(strategy, "Short", self.short_stop);
    }
}
