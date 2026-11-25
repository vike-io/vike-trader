// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "LANZ Strategy 2.0 [Backtest]" by rau_u_lanz, https://www.tradingview.com/script/YFZE9yFC/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2025-05-22, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Swings: a bar two bars back is a swing high when its high and the one before rise (`high[2] > high[3] > high[4]`), a swing
//! low the mirror; the newest three of each are kept. The trend is up when the newest swing high and low are both over the ones
//! before, down when both are under, else as it was. A close over the newest swing high by half a pip (a pip is 10 ticks) breaks
//! the structure up, one under the newest swing low by half a pip down; the last break and the last trend are remembered.
//!
//! On the bar opening at 02:00 New York (the US daylight rule), with no trade open: a buy limit at the close when the last
//! break is up (`BUY` when the trend is up too, `BUY_FB` when it is known and not up), a sell limit the mirror (`SELL`,
//! `SELL_FB`). The stop is 12 pips past the farthest of the newest three swing lows (highs for a sell), 5 pips past the close
//! while fewer than three are known, and at least 10 pips from the close; the target 5.5 times that distance the other way.
//! The size risks 1,000,000 (1% of the original's 100,000,000 account input) at 10 a pip: `1,000,000 / (pips to the stop *
//! 10)`, floored to the step. Each entry's exit rests with it. No costs; starts with 1,000,000. **Does not reproduce
//! its reference** (38 trades, net 140.753326 on CAPITALCOM:EURUSD 30m, 2025-01-01..2026-10-02): checked on Dukascopy's
//! EURUSD, another feed's prices, this file gives 41 / 175.579190 on bars of the bid, which it is run on (the nearer
//! count), and 45 / 135.736705 on bars of the mid price; the held ticks have none from 2026-09-23 00:00 to 06:32 UTC,
//! inside the window, so that day's 02:00 New York bar is missing. The owner accepted those numbers on 2026-11-16.
//!
//! The original's close of every trade at 11:45 New York never fires on 30-minute bars, which open on the hour and the half
//! hour, so it is left out; so is the plot.
//!
//! **Data:** the reference ran on CAPITALCOM:EURUSD, a CFD feed; this file is checked on Dukascopy's EURUSD, a forex feed.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_marketdata::Bar;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// A pip in ticks, and the distances in pips.
const PIP_TICKS: f64 = 10.0;
const BREAK_PIPS: f64 = 0.5;
const COVER_PIPS: f64 = 12.0;
const FALLBACK_PIPS: f64 = 5.0;
const LEAST_PIPS: f64 = 10.0;
const REWARD: f64 = 5.5;
const RISK: f64 = 100_000_000.0 * (1.0 / 100.0);
/// The bar that decides: 02:00 New York, in minutes of its day.
const ANALYSIS_MINUTE: i64 = 2 * 60;

/// The newest three swings of one side, newest first.
#[derive(Default)]
struct Swings([Option<f64>; 3]);

impl Swings {
    fn push(&mut self, level: f64) {
        self.0 = [Some(level), self.0[0], self.0[1]];
    }
}

struct S {
    /// The last five highs and lows, newest first.
    bars: VecDeque<(f64, f64)>,
    highs: Swings,
    lows: Swings,
    trend: Option<i8>,
    last_break: Option<i8>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        bars: VecDeque::new(),
        highs: Swings::default(),
        lows: Swings::default(),
        trend: None,
        last_break: None,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let pip = tick * PIP_TICKS;
        self.bars.push_front((bar.high, bar.low));
        self.bars.truncate(5);
        if self.bars.len() == 5 {
            let (high, low) = (|i: usize| self.bars[i].0, |i: usize| self.bars[i].1);
            if high(2) > high(3) && high(3) > high(4) {
                self.highs.push(high(2));
            }
            if low(2) < low(3) && low(3) < low(4) {
                self.lows.push(low(2));
            }
        }
        let [last_high, previous_high, _] = self.highs.0;
        let [last_low, previous_low, _] = self.lows.0;
        if let (Some(lh), Some(ph), Some(ll), Some(pl)) =
            (last_high, previous_high, last_low, previous_low)
        {
            if lh > ph && ll > pl {
                self.trend = Some(1);
            } else if lh < ph && ll < pl {
                self.trend = Some(-1);
            }
        }
        let break_distance = BREAK_PIPS * pip;
        if last_high.is_some_and(|h| bar.close > h + break_distance) {
            self.last_break = Some(1);
        } else if last_low.is_some_and(|l| bar.close < l - break_distance) {
            self.last_break = Some(-1);
        }

        if us_minute(bar.timestamp, -5) != ANALYSIS_MINUTE || !strategy.lots().is_empty() {
            return;
        }
        let entry = bar.close;
        let sizing = strategy.sizing();
        let mut place = |name: &str, side: OrderSide, swings: &Swings| {
            let sign = if side == Long { 1.0 } else { -1.0 };
            let base = match swings.0 {
                [Some(a), Some(b), Some(c)] if side == Long => a.min(b).min(c) - COVER_PIPS * pip,
                [Some(a), Some(b), Some(c)] => a.max(b).max(c) + COVER_PIPS * pip,
                _ => entry - sign * FALLBACK_PIPS * pip,
            };
            let stop = if sign * (entry - base) < LEAST_PIPS * pip {
                entry - sign * LEAST_PIPS * pip
            } else {
                base
            };
            let take = entry + REWARD * (entry - stop);
            let pips = (entry - stop).abs() / pip;
            let size = sizing.floor(RISK / (pips * 10.0));
            strategy.entry(name, side).size(size).limit(entry);
            // Sized to the entry's own part: a reversal's order also carries the position it closes.
            strategy.exit(&format!("TP/SL {name}")).from(name).size(size).stop(stop).take(take);
        };
        match (self.last_break, self.trend) {
            (Some(1), Some(1)) => place("BUY", Long, &self.lows),
            (Some(-1), Some(-1)) => place("SELL", Short, &self.highs),
            (Some(1), Some(_)) => place("BUY_FB", Long, &self.lows),
            (Some(-1), Some(_)) => place("SELL_FB", Short, &self.highs),
            _ => {}
        }
    }
}
