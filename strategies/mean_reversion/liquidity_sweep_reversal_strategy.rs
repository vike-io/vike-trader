// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Liquidity Sweep Reversal Strategy" by blitz_locked, https://www.tradingview.com/script/GoeqO1ev/
// Changed: rewritten by hand in Rust for vike.

//! Swing highs and lows (7 bars each side, known 7 bars late) are kept as levels, unless one lies within 0.25 ATR(14) of a level
//! of its side already kept; a level is dropped when it is 150 bars old. A bar sweeps a high level when its high is over it and
//! its close back under it, with a volume over 1.3 times its SMA(20) and a wick over the body at least 1.5 times (a bar with no
//! body needs a wick); lows are the mirror. Only while flat, a sweep is held for one bar and the trade is taken at the bar
//! after, if it closes beyond the middle of the sweep's extreme and that bar's close, on a bar opening from 12:00 to 15:30 UTC.
//! The entry is market at the next open; its stop is the sweep's extreme 1.2 ATR beyond, and a trade whose stop is under 0.5
//! ATR from the close is not taken; the target is 1.5 times the stop's distance from the close. Once the position's high (low)
//! reaches half of the way from that close to the target, the stop moves to that close, once. 10% of equity, 0.04% commission, 1
//! tick, an account of 10,000 kept in USD.
//!
//! The held sweep is untouched on bars with a position open, as the rule's `strategy.position_size == 0` block leaves it, and
//! levels are checked and dropped every bar. The rule's wins and losses (`strategy.closedtrades`) only feed its dashboard.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, ConfirmedPivot, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const MAX_AGE: i64 = 150;
const MIN_GAP_ATR: f64 = 0.25;
const SL_ATR: f64 = 1.2;
const MIN_RISK_ATR: f64 = 0.5;
const REWARD_RISK: f64 = 1.5;
const BREAKEVEN_AT: f64 = 0.5;
const VOLUME_MULTIPLIER: f64 = 1.3;
const MIN_WICK_RATIO: f64 = 1.5;
const PIVOT_LENGTH: i64 = 7;

/// A swing price waiting to be swept, and the bar it was made on.
struct Level {
    price: f64,
    bar: i64,
}

struct S {
    atr: Atr,
    volume: Sma,
    pivots: ConfirmedPivot,
    bar_index: i64,
    highs: Vec<Level>,
    lows: Vec<Level>,
    pending_long: bool,
    pending_short: bool,
    pending_wick: f64,
    pending_mid: f64,
    active_tp: f64,
    active_entry: f64,
    breakeven_done: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: Atr::new(14.0).expect("a valid length"),
        volume: Sma::new(20.0).expect("a valid length"),
        pivots: ConfirmedPivot::new(7.0, 7.0).expect("valid lengths"),
        bar_index: -1,
        highs: Vec::new(),
        lows: Vec::new(),
        pending_long: false,
        pending_short: false,
        pending_wick: f64::NAN,
        pending_mid: f64::NAN,
        active_tp: f64::NAN,
        active_entry: f64::NAN,
        breakeven_done: false,
    })
}

fn too_close(levels: &[Level], price: f64, atr: f64) -> bool {
    levels.iter().any(|l| (l.price - price).abs() < atr * MIN_GAP_ATR)
}

impl S {
    /// Checks each kept level of one side against this bar: the sweep's extreme if one swept, and the levels left.
    fn sweep(&mut self, bar: &Bar, volume_sma: f64, high_side: bool) -> Option<f64> {
        let body = (bar.close - bar.open).abs();
        let wick = if high_side {
            bar.high - bar.close.max(bar.open)
        } else {
            bar.close.min(bar.open) - bar.low
        };
        let wick_ok = (body > 0.0 && wick / body >= MIN_WICK_RATIO) || (body == 0.0 && wick > 0.0);
        let volume_ok = bar.volume > volume_sma * VOLUME_MULTIPLIER;
        let index = self.bar_index;
        let levels = if high_side { &mut self.highs } else { &mut self.lows };
        let mut extreme = None;
        let mut i = 0;
        while i < levels.len() {
            let level = &levels[i];
            let wicked = if high_side {
                bar.high > level.price && bar.close < level.price
            } else {
                bar.low < level.price && bar.close > level.price
            };
            if wicked && volume_ok && wick_ok {
                let now = if high_side { bar.high } else { bar.low };
                extreme = Some(match extreme {
                    None => now,
                    Some(e) if high_side => f64::max(e, now),
                    Some(e) => f64::min(e, now),
                });
                levels.remove(i);
            } else if index - level.bar > MAX_AGE {
                levels.remove(i);
            } else {
                i += 1;
            }
        }
        extreme
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        self.bar_index += 1;
        let atr = self.atr.next(bar);
        let volume_sma = self.volume.next_value(bar.volume);
        let [pivot_high, pivot_low, ..] = self.pivots.next(bar);
        let from = self.bar_index - PIVOT_LENGTH;
        if !pivot_high.is_nan() && !too_close(&self.highs, pivot_high, atr) {
            self.highs.push(Level { price: pivot_high, bar: from });
        }
        if !pivot_low.is_nan() && !too_close(&self.lows, pivot_low, atr) {
            self.lows.push(Level { price: pivot_low, bar: from });
        }
        let sweep_high = self.sweep(bar, volume_sma, true);
        let sweep_low = self.sweep(bar, volume_sma, false);

        let (hour, _) = clock(bar);
        let session = (12..16).contains(&hour);
        if strategy.position() == 0.0 {
            let (mut do_long, mut do_short, mut wick) = (false, false, f64::NAN);
            if self.pending_long && bar.close > self.pending_mid && session {
                do_long = true;
                wick = self.pending_wick;
            }
            if self.pending_short && bar.close < self.pending_mid && session {
                do_short = true;
                wick = self.pending_wick;
            }
            self.pending_long = false;
            self.pending_short = false;
            if let Some(low) = sweep_low {
                self.pending_long = true;
                self.pending_wick = low;
                self.pending_mid = (low + bar.close) / 2.0;
            }
            if let Some(high) = sweep_high {
                self.pending_short = true;
                self.pending_wick = high;
                self.pending_mid = (high + bar.close) / 2.0;
            }
            if do_long || do_short {
                let long = do_long;
                let (sl, risk) = if long {
                    let sl = wick - atr * SL_ATR;
                    (sl, bar.close - sl)
                } else {
                    let sl = wick + atr * SL_ATR;
                    (sl, sl - bar.close)
                };
                if risk >= atr * MIN_RISK_ATR {
                    let tp = if long {
                        bar.close + risk * REWARD_RISK
                    } else {
                        bar.close - risk * REWARD_RISK
                    };
                    let (name, side) = if long { ("Long", Long) } else { ("Short", Short) };
                    strategy.entry(name, side).size(share_of_worth(&sizing, 10.0, bar.close));
                    strategy.exit(&format!("{name} Exit")).from(name).take(tp).stop(sl);
                    self.active_tp = tp;
                    self.active_entry = bar.close;
                    self.breakeven_done = false;
                }
            }
        }

        let held = strategy.position();
        if held != 0.0 && !self.breakeven_done && !self.active_entry.is_nan() {
            let long = held > 0.0;
            let reach = (self.active_tp - self.active_entry) * BREAKEVEN_AT;
            let trigger = self.active_entry + reach;
            let hit = if long { bar.high >= trigger } else { bar.low <= trigger };
            if hit {
                let (name, _) = if long { ("Long", Long) } else { ("Short", Short) };
                strategy
                    .exit(&format!("{name} Exit"))
                    .from(name)
                    .take(self.active_tp)
                    .stop(self.active_entry);
                self.breakeven_done = true;
            }
        }
    }
}
