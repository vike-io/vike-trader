// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from the script by Peter_O, https://www.tradingview.com/script/9Iwinz7I/
// The original's notice: © Peter_O
// Changed: rewritten by hand in Rust for vike.

//! Stochastic %K(13) smoothed by SMA(4), %D its SMA(3). Long 30 times the account's worth when %K crosses up through %D under
//! 80, short it when %K crosses down through %D over 20, up to 100 entries a side, each reversing the other side. Each entry
//! leaves at a target 100 ticks past its own fill. No costs; starts with 100,000.
//!
//! The rule's margin of 1/30 * 50 percent is not modelled here: the reference closes part of a position once the account
//! falls under it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Stochastic;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const SHARE: f64 = 3000.0;
const TARGET: TacticsTrail = TacticsTrail { after_ticks: 100.0, by_ticks: 0.0 };

struct S {
    stochastic: Stochastic,
    cross: Cross,
    /// Entries placed so far, which name each entry and its target apart.
    entries: usize,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stochastic: Stochastic::new(13.0, 4.0, 3.0).expect("valid lengths"),
        cross: Cross::new(),
        entries: 0,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 100, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [k, d] = self.stochastic.next(bar);
        let side = match self.cross.step(k, d) {
            Signal::Long if k < 80.0 => Long,
            Signal::Short if k > 20.0 => Short,
            _ => return,
        };
        self.entries += 1;
        let name = format!("{} {}", if side == Long { "Long" } else { "Short" }, self.entries);
        let size = share_of_worth(&sizing, SHARE, bar.close);
        strategy.entry(&name, side).size(size);
        // Sized to the entry's own part: a reversal's order also carries the position it closes.
        strategy.exit(&format!("tp {name}")).from(&name).size(size).trail(TARGET);
    }
}
