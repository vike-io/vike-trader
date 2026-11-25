// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Take profit Multi timeframe" by TrendCrypto2022, https://www.tradingview.com/script/9lzBKqhc/
// The original's notice: © TrendCrypto2022
// Changed: rewritten by hand in Rust for vike.

//! Long 1 (no more while held) when the wave trend's fast line crosses up through its 4-bar average under -20 (the wave trend:
//! the 21-bar EMA of `(hlc3 - EMA10) / (0.015 * EMA10 of |hlc3 - EMA10|)`). Held, four exits each take 5% of the entry at 3%,
//! 5%, 8% and 10% over the average entry, each with its own stop 5% under it (the offsets in whole ticks of the instrument). The rest
//! closes at the next open when the fast line crosses down through the average over 0. No costs; starts with 10,000.
//!
//! The rule's multi-timeframe take-profit (4-hour and daily averages) is off by default, so its higher-timeframe calls feed
//! nothing here. Its wave trend is the same law as `kahlman_hullma_wt_cross_strategy`'s, written again here.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Wavetrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::sizing::floor_share;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// `(id, percent of the entry, take profit percent)`.
const EXITS: [(&str, f64, f64); 4] =
    [("TP1%", 5.0, 3.0), ("TP2%", 5.0, 5.0), ("TP3%", 5.0, 8.0), ("TP4%", 5.0, 10.0)];
const STOP: f64 = 5.0;
const SIZE: f64 = 1.0;

struct S {
    wave: Wavetrend,
    /// `(wt1, wt2)` of the last bar.
    last: (f64, f64),
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        wave: Wavetrend::new(10.0, 21.0).expect("valid lengths"),
        last: (f64::NAN, f64::NAN),
    })
}

/// The rule's `percent`: a percentage of the average entry in whole ticks of `tick`.
fn ticks(percent: f64, avg: f64, tick: f64) -> f64 {
    (percent / 100.0 * avg / tick).round() * tick
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let [wt1, wt2] = self.wave.next(bar);
        let (last1, last2) = std::mem::replace(&mut self.last, (wt1, wt2));
        let buy = wt1 > wt2 && last1 <= last2 && wt1 < -20.0;
        let close_buy = wt1 < wt2 && last1 >= last2 && wt1 > 0.0;

        if buy {
            strategy.entry("Long", Long).size(SIZE);
        }
        if strategy.position() != 0.0 {
            let avg = strategy.avg_cost();
            let cut = avg - ticks(STOP, avg, tick);
            for (id, percent, take) in EXITS {
                let size = floor_share(SIZE * percent / 100.0, SIZE, sizing.qty_step());
                let take = avg + ticks(take, avg, tick);
                strategy.exit(id).from("Long").size(size).take(take).stop(cut);
            }
        }
        if close_buy {
            strategy.close_all();
        }
    }
}
