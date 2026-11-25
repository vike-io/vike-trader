// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Hulk Grid Algorithm V2 [The Quant Science]" by thequantscience, https://www.tradingview.com/script/JDeL8FAl/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2024-01-07, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A grid of 10 buy levels around 100, 0.6% apart (97, 97.6, ... 102.4), each asked for once: a long worth 100, less 20% for
//! each level up (the bottom level 100, the top 13.42), over the level's price and floored to the quantity step, resting as a
//! limit when the close is above the level, a stop when it is below, a market order when it is on it. Once a position is held
//! and then flat again, every level is asked for again; so it is when the close reaches 105% of the top level, which closes
//! everything. At most 10 longs. 0.7% commission, 5 ticks of slippage; starts with 10,000. **Does not reproduce its
//! reference** (95 trades, net 301.911896 on LTCUSDT 30m, 2025-01-01..2026-10-02): this file gives 95 / 304.683660, the net
//! 2.77 over at the venue's quantity step (0.001), no cause found; the owner accepted those numbers on 2026-11-15.
//!
//! The optional grid stop is off by default and left out, as are the plots.
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
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CENTER: f64 = 100.0;
const STEP: f64 = 0.6 / 100.0;
const BASE_ORDER: f64 = 100.0;
const DECAY: f64 = 20.0 / 100.0;
const LEVELS: usize = 10;

struct S {
    levels: [f64; LEVELS],
    asked: [bool; LEVELS],
    /// The position as of the last bar.
    last_held: f64,
    target: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    // Level k (from 0) lies `k - 5` steps from the centre.
    let levels = std::array::from_fn(|k| match k {
        0..=4 => CENTER * (1.0 - STEP * (5 - k) as f64),
        5 => CENTER,
        _ => CENTER * (1.0 + STEP * (k - 5) as f64),
    });
    Box::new(S {
        levels,
        asked: [false; LEVELS],
        last_held: 0.0,
        target: levels[LEVELS - 1] * 1.05,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: LEVELS, fee: Commission::percent(0.7), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let held = strategy.position();
        if held == 0.0 && self.last_held != 0.0 {
            self.asked = [false; LEVELS];
        }
        self.last_held = held;
        let sizing = strategy.sizing();
        for (k, &level) in self.levels.iter().enumerate() {
            if std::mem::replace(&mut self.asked[k], true) {
                continue;
            }
            let worth = BASE_ORDER * libm::pow(1.0 - DECAY, k as f64);
            let entry = strategy.entry(&format!("GRID#{}", k + 1), Long);
            entry.size(sizing.floor(worth / level));
            if bar.close > level {
                entry.limit(level);
            } else if bar.close < level {
                entry.stop(level);
            }
        }
        if bar.close >= self.target {
            strategy.close_all();
            self.asked = [false; LEVELS];
        }
    }
}
