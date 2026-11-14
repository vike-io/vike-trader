// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BTC bot" by RafaelZioni, https://www.tradingview.com/script/DPBeZbMP/
// The original's notice: © RafaelZioni
// The original's notice: © theCrypster 2020
// Changed: rewritten by hand in Rust for vike.

//! Long all of the account's worth when the smoothed RSI oscillator crosses up through 0. The oscillator is
//! `(rsi + 5 * (rsi - ema(rsi)) + ema(rsi)) / 2` of RSI(21) and its EMA(21). Longs only: a short signal (the last close crossing
//! up through the weekly pivot's third resistance, `high + 2 * (pivot - low)`) can open nothing, so it closes what is held.
//! Four exits stand for each entry, one stop 15% under the entry shared by all and four targets: a quarter at 3%, a quarter at
//! 5%, a quarter at 7% and the rest at 10% over it, each level a whole number of the symbol's ticks (nothing is traded where no
//! tick is known). 0.075% commission; starts with 10,000.
//!
//! The pivot reads `high[1]`, `low[1]` and `close[1]` of the weekly bar with lookahead on, which on every bar of a week (its own
//! last included) is the week before it, already closed: the last week that ended by this bar's open, never the week in
//! progress. The weeks are built from the symbol's daily input, its history before the window included
//! (`vike_tactics::higher::HigherTimeframeSourceCalendar`), and the levels are vike's `PivotPoints` of them.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, PivotPoints, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "buy";
/// Percent of the entry each of the first three exits takes, and the targets and stop in percent of the entry price.
const SHARE: f64 = 0.25;
const TARGETS: [f64; 4] = [3.0, 5.0, 7.0, 10.0];
const STOP: f64 = 15.0;

struct S {
    weeks: HigherTimeframeSourceCalendar,
    pivots: PivotPoints,
    /// `(week's start, its pivot's third resistance)` of the last week closed, and of the week before it.
    last: (i64, f64),
    before: (i64, f64),
    rsi: Rsi,
    mean: Ema,
    up: Cross,
    sell: Cross,
    previous_close: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        pivots: PivotPoints::default(),
        last: (i64::MIN, f64::NAN),
        before: (i64::MIN, f64::NAN),
        rsi: Rsi::new(21.0).expect("a valid length").with_rs_form(true),
        mean: Ema::new(21.0).expect("a valid length").with_skip_nan(true),
        up: Cross::new(),
        sell: Cross::new(),
        previous_close: f64::NAN,
    })
}

/// A level in whole ticks of the entry price: `round(percent / 100 * price / tick)`, ties up.
fn ticks(percent: f64, price: f64, tick: f64) -> f64 {
    (percent / 100.0 * price / tick + 0.5).floor() * tick
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        for week in self.weeks.next(bar, strategy.input(higher::DAY)) {
            self.pivots.next_hlc(week.high, week.low, week.close);
            // `PivotPoints` hands out the levels of the bar fed before: those of this week come with the next one fed.
            let r3 = self.pivots.clone().next_hlc(f64::NAN, f64::NAN, f64::NAN)[3];
            self.before = std::mem::replace(&mut self.last, (week.timestamp, r3));
        }
        // On a week's last bar that week has just closed: the week before is the one before it.
        let (_, r3) = if HigherTimeframePeriod::Week.end(self.last.0) <= bar.timestamp {
            self.last
        } else {
            self.before
        };

        let rsi = self.rsi.next_value(bar.close);
        let mean = self.mean.next_value(rsi);
        let oscillator = (rsi + (rsi - mean) * 5.0 + mean) / 2.0;
        let buy = matches!(self.up.step(oscillator, 0.0), Signal::Long);
        let sell = matches!(self.sell.step(self.previous_close, r3), Signal::Long);
        self.previous_close = bar.close;

        if buy {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if sell && strategy.position() > 0.0 {
            strategy.close(NAME);
        }
        if let Some(entry) = strategy.bindings().iter().find(|b| b.name == NAME).cloned() {
            let cost = strategy.avg_cost();
            let cut = cost - ticks(STOP, cost, tick);
            for (i, target) in TARGETS.iter().enumerate() {
                let exit = strategy
                    .exit(&format!("x{}", i + 1))
                    .from(NAME)
                    .take(cost + ticks(*target, cost, tick))
                    .stop(cut);
                if i < 3 {
                    exit.size(floor_share(SHARE * entry.size, entry.size, sizing.qty_step()));
                }
            }
        }
    }
}
