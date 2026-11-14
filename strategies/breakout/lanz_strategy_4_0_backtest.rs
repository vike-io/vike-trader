// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "LANZ Strategy 4.0 [Backtest]" by rau_u_lanz, https://www.tradingview.com/script/8Q2g9IW1/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-05-29, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Swing levels are the pivot highs and lows of 180 bars either side. When flat, the first close above the latest pivot high
//! since it formed buys, and the first close below the latest pivot low since it formed sells, each once per pivot. The stop
//! is this bar's low less 5 (50 tenths of a dollar) for a buy, its high plus 5 for a sell, the target as far on the other side
//! of this close, and the size risks 1% of the account's worth at the stop: `worth * 1% / (ticks to the stop / 10 * 10)`,
//! floored to the step. Both levels rest with the entry. No costs; starts with 100,000.
//!
//! A new pivot high clears the buy's once-per-pivot mark, a new pivot low the sell's.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::ConfirmedPivot;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsSizing};

const BUY: &str = "buy";
const SELL: &str = "sell";
const SWING: usize = 180;
/// A pip in ticks of the instrument, and the stop's buffer in pips.
const PIP_TICKS: f64 = 10.0;
const BUFFER_PIPS: f64 = 50.0;
const RISK: f64 = 1.0 / 100.0;
const PIP_VALUE: f64 = 10.0;

struct S {
    /// The pivots of `SWING` bars either side, as the reference finds them: of equal extremes the newest counts.
    pivots: ConfirmedPivot,
    last_top: f64,
    last_bottom: f64,
    top_crossed: bool,
    bottom_crossed: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        pivots: ConfirmedPivot::new(SWING as f64, SWING as f64).expect("valid spans"),
        last_top: f64::NAN,
        last_bottom: f64::NAN,
        top_crossed: false,
        bottom_crossed: false,
    })
}

/// The size that risks `RISK` of `worth` between `entry` and `stop`, a pip being `pip`.
fn size(sizing: &TacticsSizing, worth: f64, entry: f64, stop: f64, pip: f64) -> f64 {
    let pips = (entry - stop).abs() / pip;
    sizing.floor(worth * RISK / (pips * PIP_VALUE))
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let pip = tick * PIP_TICKS;
        let buffer = BUFFER_PIPS * pip;
        let sizing = strategy.sizing();
        let [high, low, _, _] = self.pivots.next(bar);
        if !high.is_nan() {
            self.last_top = high;
            self.top_crossed = false;
        }
        if !low.is_nan() {
            self.last_bottom = low;
            self.bottom_crossed = false;
        }
        if strategy.position() != 0.0 {
            return;
        }
        let buy = !self.top_crossed && bar.close > self.last_top;
        let sell = !self.bottom_crossed && bar.close < self.last_bottom;
        if buy {
            let stop = bar.low - buffer;
            let take = bar.close + (bar.close - stop);
            let worth = strategy.equity();
            strategy.entry(BUY, Long).size(size(&sizing, worth, bar.close, stop, pip));
            strategy.exit("buy exit").from(BUY).take(take).stop(stop);
            self.top_crossed = true;
        }
        if sell {
            let stop = bar.high + buffer;
            let take = bar.close - (stop - bar.close);
            let worth = strategy.equity();
            strategy.entry(SELL, Short).size(size(&sizing, worth, bar.close, stop, pip));
            strategy.exit("sell exit").from(SELL).take(take).stop(stop);
            self.bottom_crossed = true;
        }
    }
}
