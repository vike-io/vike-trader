// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTREX strategy" by RafaelZioni, https://www.tradingview.com/script/K3U9opwQ/
// The original's notice: © RafaelZioni
// Changed: rewritten by hand in Rust for vike.

//! A SuperTrend whose centre is the close of the last RSI(14) buy cross, long 25% of the account's worth and shorts disallowed.
//! The RSI is taken of the weekly sample (the close of each week's first 30-minute bar, held between), so it moves on the
//! first bar of a week alone; it crossing up through 35 sets the centre, crossing down through 70 sets the other. The bands are
//! the centre plus and minus 4 ATR(10); the line is the lower one while the trend is up and the upper one while it is down.
//! The close crossing up through the line buys, crossing down sells. A sell with the position held closes all of it (the rule
//! lets no short in), a sell flat does nothing, and a buy while held is ignored. From the bar after the fill four exits stand:
//! three of a quarter of the entry each at 10%, 20% and 50% above the average entry and a fourth for what is left at 100%
//! above, all four under one stop 25% below, the levels in whole ticks of the symbol (nothing is traded where no tick is known).
//! 0.075% commission; starts with 10,000.
//!
//! The RSI is saturated: vike's `Rsi` in the RS form (`Rsi::with_rs_form`, the sample's `NaN` before the first week skipped)
//! with a mean move of 1e-10 or less read as none, the losses' first (`Rsi::with_saturation`), so the sample held flat for a
//! week reads 100 where its decayed means would read anything.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Rsi};
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "buy";
const SHARE: f64 = 25.0;
const FACTOR: f64 = 4.0;
/// A quarter of the entry for each of the first three exits.
const SLICE: f64 = 0.25;
const STOP: f64 = 25.0;
/// The exits' targets, in percent above the average entry, and whether they take a slice or what is left.
const TARGETS: [(&str, f64, bool); 4] =
    [("x1", 10.0, true), ("x2", 20.0, true), ("x3", 50.0, true), ("x4", 100.0, false)];

/// The original's `round(percent / 100 * price / mintick)`: a level in whole ticks.
fn ticks(percent: f64, price: f64, tick: f64) -> f64 {
    (percent / 100.0 * price / tick).round()
}

struct S {
    week: Option<i64>,
    sample: f64,
    rsi: Rsi,
    atr: Atr,
    up_cross: Cross,
    down_cross: Cross,
    last_long: f64,
    last_short: f64,
    centre_before: f64,
    short_before: f64,
    up_trend: f64,
    down_trend: f64,
    trend: i32,
    line_cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        week: None,
        sample: f64::NAN,
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true).with_saturation(true),
        atr: Atr::new(10.0).expect("a valid length"),
        up_cross: Cross::new(),
        down_cross: Cross::new(),
        last_long: f64::NAN,
        last_short: f64::NAN,
        centre_before: f64::NAN,
        short_before: f64::NAN,
        up_trend: f64::NAN,
        down_trend: f64::NAN,
        trend: 1,
        line_cross: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let week = HigherTimeframePeriod::Week.start(bar.timestamp);
        // `change(time("W")) != 0`: the first bar of a week, never the first bar on the chart.
        if self.week.is_some_and(|w| w != week) {
            self.sample = bar.close;
        }
        self.week = Some(week);
        let rsi = self.rsi.next_value(self.sample);
        let long = matches!(self.up_cross.step(rsi, 35.0), Signal::Long);
        let short = matches!(self.down_cross.step(rsi, 70.0), Signal::Short);
        let last_long = if long { bar.close } else { nan_to_zero(self.last_long) };
        let last_short = if short { bar.close } else { nan_to_zero(self.last_short) };
        let centre = last_long;
        let atr = self.atr.next(bar);
        // Left: no `Supertrend` form is this one: its bands carry on the last centre and the last short cross's close.
        let up_level = centre - FACTOR * atr;
        let down_level = centre + FACTOR * atr;
        let (previous_up, previous_down) = (self.up_trend, self.down_trend);
        self.up_trend =
            if self.centre_before > previous_up { max(up_level, previous_up) } else { up_level };
        self.down_trend = if self.short_before < previous_down {
            min(down_level, previous_down)
        } else {
            down_level
        };
        self.trend = if bar.close > previous_down {
            1
        } else if bar.close < previous_up {
            -1
        } else {
            self.trend
        };
        self.centre_before = centre;
        self.short_before = last_short;
        self.last_long = last_long;
        self.last_short = last_short;
        let line = if self.trend == 1 { self.up_trend } else { self.down_trend };
        let (buy, sell) = match self.line_cross.step(bar.close, line) {
            Signal::Long => (true, false),
            Signal::Short => (false, true),
            _ => (false, false),
        };

        if buy {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if sell && strategy.position() > 0.0 {
            strategy.close_all();
        }
        if strategy.position() > 0.0 {
            let avg = strategy.avg_cost();
            let opened = strategy.bindings().first().map_or(strategy.position(), |b| b.size);
            let slice = floor_share(SLICE * opened, opened, sizing.qty_step());
            let stop = avg - ticks(STOP, avg, tick) * tick;
            for (name, percent, sliced) in TARGETS {
                let target = avg + ticks(percent, avg, tick) * tick;
                let size = if sliced { slice } else { opened - 3.0 * slice };
                strategy.exit(name).from(NAME).size(size).take(target).stop(stop);
            }
        }
    }
}
