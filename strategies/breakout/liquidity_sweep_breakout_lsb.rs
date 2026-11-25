// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Liquidity Sweep Breakout - LSB" by humayunmha, https://www.tradingview.com/script/MN4tJCT5/
// The original's notice: © humayunmha - Humayun MHA - Fintech, android apppp developer - Trader and trading software developer.
// Changed: rewritten by hand in Rust for vike.

//! A breakout of one bar's range in a late UTC evening. The "session" runs 20:00 to 05:00 UTC, its high and low kept from
//! its first bar on; the bar opening at 20:54 marks the breakout range, its high and low. On the bars opening at each
//! quarter hour from 21:30 to 23:30, a day without a trade yet goes long when the bar closes above the range's high with
//! its low above it too, the stop at the session's low and the target 1.5 times that distance above the close; short is
//! the mirror (closes under the range's low with its high under it, the stop at the session's high). The 23:30 bar,
//! still inside the range, rules the day out. One trade a day: the day's flags clear at 15:00 and at 00:00 UTC, and the
//! 15:00 bar closes the whole position when the rule still counts a trade as running. A position stays until its stop or
//! target, and a later day's signal on the same side only moves them (one entry a side); the other side reverses it. No
//! costs; starts with 1,000,000, no margin stated.
//!
//! The size is the original's forex lot from a risk of 100 dollars: 100 over the stop distance in pips (times 10,000)
//! times 0.693 a pip, times 10, rounded to 0.01 and held within 0.01 and 10. On this market the stop distance is hundreds
//! of dollars, so the lot rounds to 0 and every trade is the floor, 0.01 of a unit. The rule's own book of a trade (the
//! bar's high or low reaching the target or the stop) clears its running flag only, as in the original, and the end of day
//! close reads that flag. The drawn lines, boxes, labels and alert texts are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const DAY_MS: i64 = 86_400_000;
const MINUTE_MS: i64 = 60_000;
/// The session, in minutes of the UTC day: from 20:00 to 05:00 the next morning.
const SESSION_FROM: i64 = 20 * 60;
const SESSION_TO: i64 = 5 * 60;
/// The bar whose range is the breakout's.
const MARK: i64 = 20 * 60 + 54;
/// The bars a breakout is checked on, the last of them the one that can rule the day out.
const CHECKS: [i64; 9] = [
    21 * 60 + 30,
    21 * 60 + 45,
    22 * 60,
    22 * 60 + 15,
    22 * 60 + 30,
    22 * 60 + 45,
    23 * 60,
    23 * 60 + 15,
    23 * 60 + 30,
];
const LAST_CHECK: i64 = 23 * 60 + 30;
const END_OF_DAY: i64 = 15 * 60;
const REWARD_TO_RISK: f64 = 1.5;
const RISK_DOLLARS: f64 = 100.0;
const PIPS_PER_UNIT: f64 = 10_000.0;
const PROFIT_PER_PIP: f64 = 0.693;
const LOT_STEP: f64 = 0.01;
const MOST_LOTS: f64 = 10.0;

/// The original's lot for a trade from `entry` with its stop at `stop`.
fn lot(entry: f64, stop: f64) -> f64 {
    let pips = (entry - stop).abs() * PIPS_PER_UNIT;
    let lots = (RISK_DOLLARS / (pips * PROFIT_PER_PIP)) * 10.0;
    let rounded = (lots / LOT_STEP).round() * LOT_STEP;
    LOT_STEP.max(rounded.min(MOST_LOTS))
}

#[derive(Default)]
struct S {
    in_session_before: bool,
    session_high: f64,
    session_low: f64,
    /// The breakout range, NaN before the first mark bar.
    range_high: f64,
    range_low: f64,
    /// The rule's own flags: a trade it counts as running, one taken today, a day ruled out.
    running: bool,
    taken: bool,
    ruled_out: bool,
    /// The running trade's side (1 long, -1 short), its stop and its target.
    side: i32,
    stop: f64,
    target: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        session_high: f64::NAN,
        session_low: f64::NAN,
        range_high: f64::NAN,
        range_low: f64::NAN,
        stop: f64::NAN,
        target: f64::NAN,
        ..S::default()
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let minute = bar.timestamp.rem_euclid(DAY_MS) / MINUTE_MS;
        let in_session = !(SESSION_TO..SESSION_FROM).contains(&minute);
        if in_session && !self.in_session_before {
            (self.session_high, self.session_low) = (bar.high, bar.low);
        } else if in_session {
            self.session_high = self.session_high.max(bar.high);
            self.session_low = self.session_low.min(bar.low);
        }
        self.in_session_before = in_session;
        if minute == MARK {
            (self.range_high, self.range_low) = (bar.high, bar.low);
        }

        let check = CHECKS.contains(&minute);
        let marked = !self.range_high.is_nan();
        let inside = bar.high >= self.range_low && bar.low <= self.range_high;
        if check && !self.running && minute == LAST_CHECK && marked && inside && !self.taken {
            self.ruled_out = true;
        }
        let may_trade = check && !self.running && !self.taken && !self.ruled_out;
        let long = marked && bar.close > self.range_high && bar.low > self.range_high;
        if may_trade && long {
            self.open(strategy, bar.close, self.session_low, 1);
        }
        let short = marked && bar.close < self.range_low && bar.high < self.range_low;
        if may_trade && !self.running && short {
            self.open(strategy, bar.close, self.session_high, -1);
        }

        if self.running {
            let (target_hit, stop_hit) = if self.side == 1 {
                (bar.high >= self.target, bar.low <= self.stop)
            } else {
                (bar.low <= self.target, bar.high >= self.stop)
            };
            if target_hit || stop_hit {
                self.running = false;
            }
        }
        if minute == END_OF_DAY {
            if self.running {
                strategy.close_all();
                self.running = false;
            }
            (self.taken, self.side, self.ruled_out) = (false, 0, false);
        }
        if minute == 0 {
            (self.running, self.ruled_out, self.taken, self.side) = (false, false, false, 0);
        }
    }
}

impl S {
    /// A trade from this close: its stop at `stop`, its target 1.5 times that distance the other way.
    fn open(&mut self, strategy: &mut Tactics<'_>, entry: f64, stop: f64, side: i32) {
        let target = entry + (entry - stop) * REWARD_TO_RISK;
        let size = lot(entry, stop);
        (self.stop, self.target, self.side) = (stop, target, side);
        (self.running, self.taken) = (true, true);
        if side == 1 {
            strategy.entry("LSB_Buy", Long).size(size);
            strategy.exit("LSB_BuyExit").from("LSB_Buy").stop(stop).take(target);
        } else {
            strategy.entry("LSB_Sell", Short).size(size);
            strategy.exit("LSB_SellExit").from("LSB_Sell").stop(stop).take(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lot;

    /// A stop of one pip is capped at 10 lots; this market's stop distances give the floor.
    #[test]
    fn the_lot_is_held_within_its_floor_and_cap() {
        assert_eq!(lot(1.1000, 1.0999), 10.0);
        assert_eq!(lot(100_000.0, 99_500.0), 0.01);
    }
}
