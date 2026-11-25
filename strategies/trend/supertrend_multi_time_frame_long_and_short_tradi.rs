// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend Multi Time Frame Long and Short Trading Strategy with Take Profit, Stop Loss and in build alerts V01" by AIScripts, https://www.tradingview.com/script/nXIb4hOt/
// The original's notice: © ranga_trading
// Changed: rewritten by hand in Rust for vike.

//! A chandelier exit (22 bars of the close, 3 ATR) on the chart sets the trend; a turn up is taken long when the daily and the
//! weekly chandelier of the bar before were both up, a turn down short when both were down, 60% of the account's worth, one
//! side at a time and each reversing the other, filling at the signal close. A turn the other way also closes the position.
//! Each entry carries four exits from the bar after its fill, all under one stop 5% from the average entry: three with targets at
//! 5%, 10% and 15% above (below) it and a fourth with none, of 10%, 15%, 20% and the rest of the account's worth in BTC as it stood
//! at the last bar that held nothing (the rule's capital and the run's closed trades, `vike_tactics::Tactics::closed_net`),
//! whatever the entry's own size. No costs; starts with 2,000.
//!
//! The rule's daily and weekly values are read with lookahead on, of `[1]`, so every 30-minute bar of a day (a week) sees the
//! chandelier of the day (week) before it as that bar closed, the last bar of the period included, where a read without
//! lookahead would see the period's own bar there: nothing it reads lies ahead. Each chandelier is computed on the higher bars:
//! the symbol's daily input and the weeks built from it, their history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//! The exit sizes are the rule's own, fixed at each bar it holds nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Chandelier;
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics, TacticsInput};

const LENGTH: f64 = 22.0;
const MULTIPLIER: f64 = 3.0;
const CAPITAL: f64 = 2_000.0;
const STOP: f64 = 0.05;
/// The three targets, then the fourth exit with none.
const TARGETS: [f64; 3] = [0.05, 0.10, 0.15];
const SHARES: [f64; 3] = [0.10, 0.15, 0.20];

/// The rule's chandelier exit: 22 bars of the close, 3 ATR, the stops ratcheting.
fn chandelier() -> Chandelier {
    let chandelier = Chandelier::new(LENGTH, MULTIPLIER).expect("valid factors");
    chandelier.with_close_extremes(true).with_ratchet(true)
}

/// The chandelier's trend after `bar`, `1` up or `-1` down.
fn trend(chandelier: &mut Chandelier, bar: &Bar) -> i32 {
    chandelier.next(bar);
    if chandelier.uptrend() { 1 } else { -1 }
}

/// A higher timeframe's chandelier, read the way a call with lookahead on of `[1]` reads it.
struct Htf {
    bars: HigherTimeframeSourceCalendar,
    chandelier: Chandelier,
    /// `(start of the higher bar, its trend)`, each closed bar in turn.
    closed: Vec<(i64, i32)>,
}

impl Htf {
    fn new(every: HigherTimeframePeriod) -> Self {
        Htf {
            bars: HigherTimeframeSourceCalendar::new(every),
            chandelier: chandelier(),
            closed: Vec::new(),
        }
    }

    fn feed(&mut self, bar: &Bar, day: TacticsInput<'_>) {
        for b in self.bars.next(bar, day) {
            let dir = trend(&mut self.chandelier, &b);
            self.closed.push((b.timestamp, dir));
        }
    }

    /// The trend of the last higher bar that started before `period`.
    fn before(&self, period: i64) -> i32 {
        self.closed.iter().rev().find(|(start, _)| *start < period).map_or(0, |&(_, dir)| dir)
    }
}

struct S {
    chart: Chandelier,
    previous_dir: i32,
    days: Htf,
    weeks: Htf,
    /// The rule's position flag of the bar before, NaN before any signal.
    pos: f64,
    /// The sizes of the four exits, refreshed on each bar that holds nothing.
    sizes: [f64; 4],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        chart: chandelier(),
        previous_dir: 1,
        days: Htf::new(HigherTimeframePeriod::Day),
        weeks: Htf::new(HigherTimeframePeriod::Week),
        pos: f64::NAN,
        sizes: [0.0; 4],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let dir = trend(&mut self.chart, bar);
        let buy_signal = dir == 1 && self.previous_dir == -1;
        let sell_signal = dir == -1 && self.previous_dir == 1;
        self.previous_dir = dir;
        self.days.feed(bar, strategy.input(higher::DAY));
        self.weeks.feed(bar, strategy.input(higher::DAY));
        let day = self.days.before(bar.timestamp.div_euclid(MS_PER_DAY) * MS_PER_DAY);
        let week = self.weeks.before(HigherTimeframePeriod::Week.start(bar.timestamp));
        let long = buy_signal && day > 0 && week > 0;
        let short = sell_signal && day < 0 && week < 0;

        let before = self.pos;
        let mut pos = if long {
            1.0
        } else if short {
            -1.0
        } else {
            before
        };
        let long_cond = long && before != 1.0;
        let short_cond = short && before != -1.0;
        let avg = strategy.avg_cost();
        let long_stop = avg * (1.0 - STOP);
        let short_stop = avg * (1.0 + STOP);
        if (bar.low < long_stop && before == 1.0) || (bar.high > short_stop && before == -1.0) {
            pos = 0.0;
        }
        let long_exit = sell_signal && before == 1.0;
        let short_exit = buy_signal && before == -1.0;
        if long_exit || short_exit {
            pos = 0.0;
        }
        self.pos = pos;

        let held = strategy.position();
        if held == 0.0 {
            let equity = (CAPITAL + strategy.closed_net().unwrap_or(0.0)) / bar.close;
            let parts = SHARES.map(|share| sizing.floor(equity * share));
            self.sizes = [
                parts[0],
                parts[1],
                parts[2],
                sizing.floor(equity - parts[0] - parts[1] - parts[2]),
            ];
        }
        if long_cond {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 60.0, bar.close));
        }
        if short_cond {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 60.0, bar.close));
        }
        if long_exit {
            strategy.close("long");
        }
        if short_exit {
            strategy.close("short");
        }
        if held != 0.0 {
            let (name, side) = if held > 0.0 { ("long", 1.0) } else { ("short", -1.0) };
            let stop = if held > 0.0 { long_stop } else { short_stop };
            for (i, size) in self.sizes.into_iter().enumerate() {
                let exit =
                    strategy.exit(&format!("Exit{}{name}", i + 1)).from(name).size(size).stop(stop);
                if let Some(t) = TARGETS.get(i) {
                    exit.take(avg * (1.0 + side * t));
                }
            }
        }
    }
}
