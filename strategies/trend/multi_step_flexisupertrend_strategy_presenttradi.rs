// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Multi-Step FlexiSuperTrend - Strategy [presentTrading]" by PresentTrading, https://www.tradingview.com/script/FxxdXitn/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! Twenty Supertrends of the bar's typical price `(high + low + close) / 3` over ATR(10), with factors 0.618, 1.0, 1.382 and
//! so on by 0.382; the median of the twenty distances from the price to each one's line is the oscillator. A second
//! Supertrend, of factor 15 over a 10-bar Wilder average, runs on a bar made from the oscillator (opening at its previous
//! value, closing at this one). Long a tenth of the account's worth while that Supertrend is up and the oscillator above 0,
//! short while it is down and the oscillator below 0; a short is closed at the next open as the long goes in, and the mirror.
//! While held, three targets from the average cost: 30% of the entry at 2%, 20% at 8% and 15% at 18%. 0.1% commission and
//! one tick of slippage on each fill; starts with 10,000.
//!
//! The oscillator's Supertrends are the reference's own law (`helpers::FlexiMedian`). The signal's is vike's that starts down
//! (`Supertrend::with_down_start`: on the first bar with an ATR the bands the bar before lacked read as 0, as the rule's `nan_to_zero`
//! reads them), fed the made bar from the first that is there (`nan_to_zero` opens it at 0); while it is missing the rule reads down.
//! The direction is `-1` up and `1` down, as the rule writes it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::FlexiMedian;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "Long Position";
const DOWN: &str = "Short Position";
const SHARE: f64 = 10.0;
const SIGNAL_FACTOR: f64 = 15.0;
/// `(level %, share of the entry %)` of each target.
const TARGETS: [(f64, f64); 3] = [(2.0, 30.0), (8.0, 20.0), (18.0, 15.0)];
const LOTS_PER_UNIT: f64 = 100_000.0;

/// A share of an entry's size, floored to the quantity step, a product a hair under a whole lot taken as that lot, and never
/// under one lot.
fn share_of_entry(size: f64, percent: f64) -> f64 {
    let lots = size * (percent * 0.01) * LOTS_PER_UNIT;
    let nearest = lots.round();
    let lots = if (lots - nearest).abs() <= 1e-9 { nearest } else { lots.floor() };
    lots.max(1.0) / LOTS_PER_UNIT
}

struct S {
    median: FlexiMedian,
    median_previous: f64,
    signal: Supertrend,
    /// The trade held: its entry's time and size at the fill.
    trade: Option<(i64, f64)>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        median: FlexiMedian::default(),
        median_previous: f64::NAN,
        signal: Supertrend::new(10.0, SIGNAL_FACTOR)
            .expect("valid parameters")
            .with_down_start(true),
        trade: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let median = self.median.next(bar);
        let open = nan_to_zero(self.median_previous);
        self.median_previous = median;
        let dir = if median.is_nan() {
            1.0
        } else {
            -self.signal.next_hlc(open.max(median), open.min(median), median)[1]
        };
        let held = strategy.position();
        if dir < 0.0 && median > 0.0 && sizing.equity() > 0.0 {
            if held < 0.0 {
                strategy.close(DOWN);
            }
            strategy.entry(UP, Long).size(share_of_worth(
                &sizing,
                SHARE,
                sizing.slipped(Long, bar.close),
            ));
        }
        if dir > 0.0 && median < 0.0 && sizing.equity() > 0.0 {
            if held > 0.0 {
                strategy.close(UP);
            }
            strategy.entry(DOWN, Short).size(share_of_worth(
                &sizing,
                SHARE,
                sizing.slipped(Short, bar.close),
            ));
        }

        let Some(first) = strategy.lots().first() else {
            self.trade = None;
            return;
        };
        if self.trade.is_none_or(|(ms, _)| ms != first.entry_timestamp) {
            self.trade = Some((first.entry_timestamp, first.qty));
        }
        let (_, size) = self.trade.expect("set above");
        let cost = strategy.avg_cost();
        let (lots, toward) = if held > 0.0 { (UP, 1.0) } else { (DOWN, -1.0) };
        for (i, (level, share)) in TARGETS.iter().enumerate() {
            strategy
                .exit(&format!("{lots} target {}", i + 1))
                .from(lots)
                .size(share_of_entry(size, *share))
                .take(cost * (1.0 + toward * level / 100.0));
        }
    }
}
