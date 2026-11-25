// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "RSI Divergence Indicator" by eemani123, https://www.tradingview.com/script/qSLcZSyw/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-04-27, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Divergences of RSI(9) of the close at its pivots, one bar to the left and three to the right, confirmed three bars after the
//! pivot. On a confirmed pivot low, against the pivot low before it, 5 to 60 bars between the bar after that one and this bar:
//! a lower low with a higher RSI, or a higher low with a lower RSI, buys 2 BTC, up to two entries held. Both entries are closed
//! when RSI crosses up through 80, or on a confirmed pivot high with a higher high and a lower RSI than the pivot high before
//! it, in the same range. No costs; starts with 10,000 in a USD account.
//!
//! A pivot low may equal the RSI to its left and must be strictly under the three to its right; a pivot high the mirror: vike's
//! `ConfirmedPivot` of the RSI. A window holding a missing RSI confirms nothing, which with one bar to the left is vike's rule
//! too.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::helpers::known;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{ConfirmedPivot, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "long";
const SIZE: f64 = 2.0;
const LEFT: usize = 1;
const RIGHT: usize = 3;
const NEAREST: u64 = 5;
const FARTHEST: u64 = 60;
const TAKE_PROFIT: f64 = 80.0;

/// A confirmed pivot: the bar that confirmed it, the RSI at the pivot, and the bar's low or high there.
#[derive(Clone, Copy)]
struct Pivot {
    bar: u64,
    rsi: f64,
    price: f64,
}

struct S {
    rsi: Rsi,
    pivots: ConfirmedPivot,
    /// `(low, high)` of the last `RIGHT + 1` bars, newest first.
    window: VecDeque<(f64, f64)>,
    bars: u64,
    last_low: Option<Pivot>,
    last_high: Option<Pivot>,
    take: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(9.0).expect("a valid length"),
        pivots: ConfirmedPivot::new(LEFT as f64, RIGHT as f64).expect("valid spans"),
        window: VecDeque::with_capacity(RIGHT + 2),
        bars: 0,
        last_low: None,
        last_high: None,
        take: Cross::new(),
    })
}

/// Whether the bars between the pivot before and this bar, counted from the bar after it, are in the rule's range.
fn in_range(before: Pivot, now: u64) -> bool {
    (NEAREST..=FARTHEST).contains(&(now - before.bar - 1))
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let rsi = self.rsi.next(bar);
        let [pivot_high, pivot_low, _, _] = self.pivots.next_hl(rsi, rsi);
        self.window.push_front((bar.low, bar.high));
        self.window.truncate(RIGHT + 1);
        let now = self.bars;
        self.bars += 1;
        let (low, high) = self.window.get(RIGHT).copied().unwrap_or((f64::NAN, f64::NAN));

        let mut buy = false;
        if let Some(osc) = known(pivot_low) {
            let pivot = Pivot { bar: now, rsi: osc, price: low };
            if let Some(before) = self.last_low
                && in_range(before, now)
            {
                let regular = low < before.price && osc > before.rsi;
                let hidden = low > before.price && osc < before.rsi;
                buy = regular || hidden;
            }
            self.last_low = Some(pivot);
        }
        let mut bear = false;
        if let Some(osc) = known(pivot_high) {
            let pivot = Pivot { bar: now, rsi: osc, price: high };
            if let Some(before) = self.last_high
                && in_range(before, now)
            {
                bear = high > before.price && osc < before.rsi;
            }
            self.last_high = Some(pivot);
        }
        let take = matches!(self.take.step(rsi, TAKE_PROFIT), Signal::Long);

        if buy {
            strategy.entry(NAME, Long).size(SIZE);
        }
        if strategy.position().abs() >= 1.0 && (take || bear) {
            strategy.close(NAME);
        }
    }
}
