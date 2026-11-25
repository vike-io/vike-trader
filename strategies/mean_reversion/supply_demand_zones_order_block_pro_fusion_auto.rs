// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Supply & Demand Zones + Order Block (Pro Fusion) - Auto Order" by denidiana, https://www.tradingview.com/script/lQ5zIOGa/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-03-29, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Swing levels: from the 22nd bar on, each bar looks 2 to 20 bars back for a low under the two lows on each side of it (a
//! demand level) and a high over the two highs on each side (a supply level); the farthest one back found becomes the
//! latest, which stands until a bar finds another. Long 10% of the account's worth at the next open when the bar's low
//! reaches the latest demand and it closes above it, above EMA(50) of the close, on a volume over 1.5 times its 20-bar
//! average; short on the mirror at the latest supply, under the EMA. Each reverses the other, and each signal (re)states a
//! stop 10.00 past this close for its side's entry. No costs; starts with 1,000,000.
//!
//! The rule's trailing stop never arms (its level is cleared on every signal and set only on a close it cannot reach), and
//! its order blocks and filters only draw.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_tactics::rolling::RollingSum;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LENGTH: usize = 20;
/// The stop's distance from the entry, in ticks of the instrument.
const SL_TICKS: f64 = 1000.0;
const SHARE: f64 = 10.0;

struct S {
    ema: IndicatorLine,
    volume: RollingSum,
    /// `(high, low)` of the last `LENGTH + 3` bars, newest first.
    bars: VecDeque<(f64, f64)>,
    seen: usize,
    demand: f64,
    supply: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ema: IndicatorLine::new("ema", &[50.0]),
        volume: RollingSum::new(20),
        bars: VecDeque::with_capacity(LENGTH + 4),
        seen: 0,
        demand: f64::NAN,
        supply: f64::NAN,
    })
}

impl S {
    fn low(&self, i: usize) -> f64 {
        self.bars.get(i).map_or(f64::NAN, |b| b.1)
    }

    fn high(&self, i: usize) -> f64 {
        self.bars.get(i).map_or(f64::NAN, |b| b.0)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sl_points = SL_TICKS * tick;
        let sizing = strategy.sizing();
        let ema = self.ema.next(bar)[0];
        let mean_volume = self.volume.next(bar.volume) / 20.0;
        self.bars.push_front((bar.high, bar.low));
        self.bars.truncate(LENGTH + 3);
        // `bar_index > length`: the first bar is index 0.
        if self.seen > LENGTH {
            for i in 2..=LENGTH {
                let l = self.low(i);
                if l < self.low(i - 1)
                    && l < self.low(i + 1)
                    && l < self.low(i - 2)
                    && l < self.low(i + 2)
                {
                    self.demand = l;
                }
                let h = self.high(i);
                if h > self.high(i - 1)
                    && h > self.high(i + 1)
                    && h > self.high(i - 2)
                    && h > self.high(i + 2)
                {
                    self.supply = h;
                }
            }
        }
        self.seen += 1;
        let spike = bar.volume > mean_volume * 1.5;
        let c = bar.close;
        let bullish = bar.low <= self.demand && c > self.demand && c > ema && spike;
        let bearish = bar.high >= self.supply && c < self.supply && c < ema && spike;
        if bullish {
            strategy.entry("Buy", Long).size(share_of_worth(&sizing, SHARE, c));
            strategy.exit("TP/SL Buy").from("Buy").stop(c - sl_points);
        }
        if bearish {
            strategy.entry("Sell", Short).size(share_of_worth(&sizing, SHARE, c));
            strategy.exit("TP/SL Sell").from("Sell").stop(c + sl_points);
        }
    }
}
