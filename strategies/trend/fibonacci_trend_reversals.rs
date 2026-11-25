// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Fibonacci Trend Reversals" by nioboi, https://www.tradingview.com/script/cZa2pcyV/
// The original's notice: © nioboi
// The original's notice: TSOT | Trend Strength Over Time © federalTacos5392b
// The original's notice: ATR SL Finder © Veryfid
// The original's notice: Strategy Execution © nioboi (yours truly)
// The original's notice: Entry Visuals © nioboi (yours truly)
// Changed: rewritten by hand in Rust for vike.

//! The midline of the 180-bar channel (half way between its highest high and lowest low) as the trend line. Long when a bar
//! opens under it and closes at or over it while the trend strength is bullish and the trend was not already long; short the
//! mirror, the trend strength bearish. The trend strength scores the 75th percentile of the highs and the 25th of the lows
//! over 13, 21, 34, 55 and 89 bars (nearest rank) against the 75th percentile of the highs and the 25th of the lows over 144:
//! 10 a value above the long high band, -10 one under the long low band, and 5 for each low and -5 for each high between the
//! two; bullish over 0, bearish under.
//!
//! An entry takes all of the account's worth at the signal close and reverses a position the other way. Its stop is 3.5
//! ATR(14) beyond the signal bar's extreme (its low for a long), its risk the distance from the close to it; two targets
//! from the close, half the entry (floored to the step) at once the risk and the rest at twice it. The stop is the rule's
//! own: a bar whose low (a long) or high (a short) reaches it closes the rest at that bar's close. A long whose high
//! reaches the first target moves its stop to 0.05% over the
//! entry, on every such bar, and is closed only on a bar that does not reach it. A short whose low reaches the first target
//! moves its stop to 0.05% under the entry, once, and from then a high at or over it closes the rest. Everything fills at
//! the signal bar's close. 0.055% commission; starts with 1,000, version 6: margined at 100% (`main.rs`'s rule 4). **Does
//! not reproduce its reference** (234 trades, net -94.374191 on Bybit's WIFUSDT perpetual 5m, 2026-07-20..2026-10-02): this
//! file gives 235 / -92.349222, one trade more and the net 2.02 over: not in the margin call (every cut equals the
//! reference's rules), but on the venue's tick of today, 0.0001, where the window's bars had 0.00001 (a target fills 0.00007
//! off), and a float tie of the trend line with the open on 2026-08-14 19:05 (0.1359 against 0.13590000000000002) where the
//! reference does not enter; the owner accepted those numbers on 2026-11-15.
//!
//! The rule's other stop kinds (a fixed percent, the trend switch) are off by its inputs, and its plots draw nothing that
//! trades. The second target takes what the first leaves of an odd quantity: with both at half, floored, the unit left over
//! makes a row of its own later (240 trades against 235).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::{Atr, Donchian};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const CHANNEL: f64 = 180.0;
const LENGTHS: [usize; 5] = [13, 21, 34, 55, 89];
const LONG_LENGTH: usize = 144;
const ATR_MULTIPLIER: f64 = 3.5;
const REWARD_TO_RISK: f64 = 2.0;
const BREAK_EVEN: f64 = 0.0005;

/// One side's levels, set at its entry.
struct Side {
    held: bool,
    first_target_hit: bool,
    stop: f64,
    break_even: f64,
    first_target: f64,
}

impl Side {
    fn new() -> Self {
        Side {
            held: false,
            first_target_hit: false,
            stop: f64::NAN,
            break_even: f64::NAN,
            first_target: f64::NAN,
        }
    }
}

struct S {
    channel: Donchian,
    atr: Atr,
    /// The last `LONG_LENGTH` highs and lows, newest first.
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    long_trend: bool,
    short_trend: bool,
    long: Side,
    short: Side,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        channel: Donchian::new(CHANNEL).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        highs: VecDeque::new(),
        lows: VecDeque::new(),
        long_trend: false,
        short_trend: false,
        long: Side::new(),
        short: Side::new(),
    })
}

/// The nearest-rank percentile of the newest `length` values: the `ceil(percent * length)`-th smallest, missing until
/// `length` are in.
fn nearest_rank(values: &VecDeque<f64>, length: usize, percent: f64) -> f64 {
    if values.len() < length {
        return f64::NAN;
    }
    let mut window: Vec<f64> = values.iter().take(length).copied().collect();
    window.sort_by(f64::total_cmp);
    let rank = (percent / 100.0 * length as f64).ceil() as usize;
    window[rank.clamp(1, length) - 1]
}

impl S {
    /// The trend strength's sign: bullish over 0, bearish under.
    fn strength(&self) -> f64 {
        let high_band = nearest_rank(&self.highs, LONG_LENGTH, 75.0);
        let low_band = nearest_rank(&self.lows, LONG_LENGTH, 25.0);
        let highs = LENGTHS.map(|n| nearest_rank(&self.highs, n, 75.0));
        let lows = LENGTHS.map(|n| nearest_rank(&self.lows, n, 25.0));
        let bull = highs.iter().chain(&lows).filter(|v| **v > high_band).count() as f64;
        let bear = highs.iter().chain(&lows).filter(|v| **v < low_band).count() as f64;
        let between = |v: &&f64| **v < high_band && **v > low_band;
        let weak_bull = lows.iter().filter(between).count() as f64;
        let weak_bear = highs.iter().filter(between).count() as f64;
        let bull_strength = 10.0 * (bull + 0.5 * weak_bull - 0.5 * weak_bear - bear);
        let bear_strength = 10.0 * (bear + 0.5 * weak_bear - 0.5 * weak_bull - bull);
        bull_strength - bear_strength
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.055), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let [highest, _, lowest] = self.channel.next(bar);
        let trend_line = highest - (highest - lowest) * 0.5;
        for (ring, x) in [(&mut self.highs, bar.high), (&mut self.lows, bar.low)] {
            ring.push_front(x);
            ring.truncate(LONG_LENGTH);
        }
        let strength = self.strength();
        let atr = self.atr.next(bar);
        let short_stop = atr * ATR_MULTIPLIER + bar.high;
        let long_stop = bar.low - atr * ATR_MULTIPLIER;

        let can_long =
            bar.close >= trend_line && bar.open < trend_line && !self.long_trend && strength > 0.0;
        let can_short =
            bar.close <= trend_line && bar.open > trend_line && !self.short_trend && strength < 0.0;
        if can_long {
            (self.long_trend, self.short_trend) = (true, false);
        } else if can_short {
            (self.short_trend, self.long_trend) = (true, false);
        }

        if self.long.held && can_short {
            strategy.close("Long");
            self.long.held = false;
        } else if self.short.held && can_long {
            strategy.close("Short");
            self.short.held = false;
        }

        let sizing = strategy.sizing();
        let close = bar.close;
        if can_long {
            let risk = close - long_stop;
            let size = share_of_worth(&sizing, 100.0, close);
            let half = floor_share(size * 0.5, size, sizing.qty_step());
            self.long = Side {
                held: true,
                first_target_hit: false,
                stop: long_stop,
                break_even: close + close * BREAK_EVEN,
                first_target: close + (REWARD_TO_RISK * risk) / 2.0,
            };
            strategy.entry("Long", Long).size(size);
            strategy.exit("TP 1 Long").from("Long").size(half).take(self.long.first_target);
            strategy
                .exit("TP 2 Long")
                .from("Long")
                .size(size - half)
                .take(close + REWARD_TO_RISK * risk);
        }
        if can_short {
            let risk = short_stop - close;
            let size = share_of_worth(&sizing, 100.0, close);
            let half = floor_share(size * 0.5, size, sizing.qty_step());
            self.short = Side {
                held: true,
                first_target_hit: false,
                stop: short_stop,
                break_even: close - close * BREAK_EVEN,
                first_target: close - (REWARD_TO_RISK * risk) / 2.0,
            };
            strategy.entry("Short", Short).size(size);
            strategy.exit("TP 1 Short").from("Short").size(half).take(self.short.first_target);
            strategy
                .exit("TP 2 Short")
                .from("Short")
                .size(size - half)
                .take(close - REWARD_TO_RISK * risk);
        }

        // The original never marks a long's first target as hit, so its stop moves on every bar that reaches the target.
        let long = &mut self.long;
        if long.held {
            if bar.high >= long.first_target {
                long.stop = long.break_even;
            } else if bar.low <= long.stop {
                strategy.close("Long");
                long.held = false;
            }
        }
        let short = &mut self.short;
        if short.held {
            if bar.low <= short.first_target && !short.first_target_hit {
                short.first_target_hit = true;
                short.stop = short.break_even;
            } else if bar.high >= short.stop {
                // the stop is the break-even once the first target was hit, so one test serves both of the original's
                strategy.close("Short");
                short.held = false;
            }
        }
    }
}
