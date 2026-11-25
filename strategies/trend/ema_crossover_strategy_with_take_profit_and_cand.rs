// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "EMA Crossover Strategy with Take Profit and Candle Highlighting" by Quince2010, https://www.tradingview.com/script/B7umqU7W/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-01-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long 1 when EMA(20) crosses up through EMA(50), short 1 when it crosses down, each reversing the other. The four targets are
//! the signal bar's high plus 0.5, 1, 1.5 and 2 times its range for a long, its low less the same for a short, set on the signal
//! bar and resting from the fill, and a stop rests 3% past each close. No costs; starts with 1,000,000.
//!
//! The rule issues the four targets and then the stop, each an exit of the whole entry. The first target holds all of the entry
//! when the others are issued, so they get no order: only the first target ever fills.
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
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BUY: &str = "Buy";
const SELL: &str = "Sell";
/// The targets' distance from the signal bar's far end, in its ranges.
const MULTIPLIERS: [f64; 4] = [0.5, 1.0, 1.5, 2.0];
/// The stop's distance from each close.
const STOP: f64 = 0.03;

struct S {
    slow: Ema,
    fast: Ema,
    cross: Cross,
    targets: [f64; 4],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        slow: Ema::new(50.0).expect("a valid length"),
        fast: Ema::new(20.0).expect("a valid length"),
        cross: Cross::new(),
        targets: [f64::NAN; 4],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let slow = self.slow.next(bar);
        let fast = self.fast.next(bar);
        let range = bar.high - bar.low;
        let signal = self.cross.step(fast, slow);
        match signal {
            Signal::Long => self.targets = MULTIPLIERS.map(|m| bar.high + range * m),
            Signal::Short => self.targets = MULTIPLIERS.map(|m| bar.low - range * m),
            _ => {}
        }
        match signal {
            Signal::Long => {
                strategy.entry(BUY, Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry(SELL, Short).size(1.0);
            }
            _ => {}
        }
        for lots in [BUY, SELL] {
            for (k, target) in self.targets.into_iter().enumerate() {
                strategy.exit(&format!("target {} {lots}", k + 1)).from(lots).take(target);
            }
        }
        for (lots, away) in [(BUY, -STOP), (SELL, STOP)] {
            strategy.exit(&format!("stop {lots}")).from(lots).stop(bar.close * (1.0 + away));
        }
    }
}
