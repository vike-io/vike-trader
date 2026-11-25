// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "[Pt] Premarket Breakout Strategy" by PtGambler, https://www.tradingview.com/script/0LJUxO7T/
// The original's notice: © PtGambler
// Changed: rewritten by hand in Rust for vike.

//! The pre-market range is the high and low of the bars opening 09:00 to 09:29 UTC. After it, the first close above its high
//! sets the day's direction long and the first close below its low short, until the next pre-market. Once a day, on a bar
//! opening 09:45 to 15:44 UTC, enter that way when the stochastic RSI crosses: long when %K crosses up through %D with %D
//! under 18 on the bar before, short when it crosses down with %D over 82. The size is 11 times the starting 4,000 over the
//! signal close, in whole coins. While held, on every bar, a stop and a target from the entry's fill price: the stop at the
//! range's far side less 1.6% of the daily ATR(14) when the fill is past the range (a long over its high), else the range's
//! width plus that 1.6% from the fill; the target twice as far the other way. A losing position (its fill at or past the
//! close) is closed at the next open when %D is past 80 (a long; under 20 a short) against an RSI(14) that has turned
//! (under 50, under its 3-bar SMA, and that SMA under the RSI's 100-bar SMA, for a long). On the bars opening 07:55 and
//! 15:55 UTC, Monday to Friday, everything is closed, and the range and the day's trade are cleared; so over a weekend the
//! range takes in both pre-markets and one trade covers both days. No commission (the header names a cost per contract and no amount), 3 ticks of slippage; starts
//! with 4,000, language version 5 (no margin call). **Does not reproduce its reference** (61 trades, net -2196.727295 on
//! DOGEUSDT 5m, 2026-07-20..2026-09-29): this file gives 61 / -1551.982900, the net 644.74 over (about 6 USDT a tick on a
//! trade), no cause found (a replay outside the engine gives this file's net to the last decimal, and no stop and limit grid
//! reading at the 0.00001 tick reaches the reference's); the owner accepted those numbers on 2026-11-15.
//!
//! The daily ATR is the rule's call for the daily series without lookahead: the last day closed by this bar's close, over
//! the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`).
//! The stochastic RSI is SMA(3) of the 14-bar stochastic of RSI(14), and %D SMA(3) of that: vike's `Stochrsi` %K with the
//! original's RSI (`Stochrsi::with_rs_form`), smoothed by two `Sma`s that fold their window afresh. A session the original
//! gives with no days holds every day (the reference's 61 trades in 52 weekdays need the weekends); its date range (2011 to
//! 2050), its reinvesting (off) and its plots trade nothing.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Rsi, Sma, Stochrsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::clock;
use vike_tactics::helpers::weekday;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

/// The starting capital times the rule's leverage, the money one entry is worth.
const STAKE: f64 = 4_000.0 * 11.0;
const ATR_SHARE: f64 = 1.6 * 0.01;
const REWARD_TO_RISK: f64 = 2.0;
const STOCH_LOW: f64 = 18.0;
const STOCH_HIGH: f64 = 82.0;
/// The pre-market session, the entry session and the two closing bars, in minutes of the UTC day.
const PRE_MARKET: std::ops::Range<i64> = 540..570;
const ENTRY: std::ops::Range<i64> = 585..945;
const CLOSING: [i64; 2] = [475, 955];
/// Friday, counted from Monday 0.
const FRIDAY: i64 = 4;
/// The pre-market range as the rule clears it.
const CLEARED: (f64, f64) = (0.0, 9999.9);

struct S {
    days: HigherTimeframeSourceCalendar,
    day_atr: Atr,
    /// The daily ATR of the last day closed.
    daily: f64,
    /// The pre-market `(high, low)`.
    range: (f64, f64),
    stoch: Stochrsi,
    k: Sma,
    d: Sma,
    /// `(k, d)` of the bar before.
    kd: (f64, f64),
    rsi: Rsi,
    rsi_smooth: Sma,
    rsi_mean: Sma,
    /// The day's direction: 1 long, -1 short, 0 none yet.
    trend: i32,
    traded: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        day_atr: Atr::new(14.0).expect("a valid length"),
        daily: f64::NAN,
        range: CLEARED,
        stoch: Stochrsi::new(14.0, 14.0, 3.0).expect("valid lengths").with_rs_form(true),
        k: Sma::new(3.0).expect("a valid length").with_fresh_sum(true),
        d: Sma::new(3.0).expect("a valid length").with_fresh_sum(true),
        kd: (f64::NAN, f64::NAN),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        rsi_smooth: Sma::new(3.0).expect("a valid length").with_fresh_sum(true),
        rsi_mean: Sma::new(100.0).expect("a valid length").with_fresh_sum(true),
        trend: 0,
        traded: false,
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
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.daily = self.day_atr.next(&day);
        }
        let margin = self.daily * ATR_SHARE;
        let (hour, minute) = clock(bar);
        let minute = hour * 60 + minute;
        let pre_market = PRE_MARKET.contains(&minute);
        if pre_market {
            let (high, low) = &mut self.range;
            *high = if bar.high > *high { bar.high } else { *high };
            *low = if bar.low < *low { bar.low } else { *low };
        }
        let (range_high, range_low) = self.range;

        let [stoch, _] = self.stoch.next_value(bar.close);
        let k = self.k.next_value(stoch);
        let d = self.d.next_value(k);
        let (k1, d1) = self.kd;
        self.kd = (k, d);
        let rsi = self.rsi.next_value(bar.close);
        let rsi_smooth = self.rsi_smooth.next_value(rsi);
        let rsi_mean = self.rsi_mean.next_value(rsi);
        let rsi_bull = rsi_smooth > rsi_mean && rsi > 50.0 && rsi > rsi_smooth;
        let rsi_bear = rsi_smooth < rsi_mean && rsi < 50.0 && rsi < rsi_smooth;

        self.trend = if pre_market {
            0
        } else if self.trend == 0 {
            if bar.close > range_high {
                1
            } else if bar.close < range_low {
                -1
            } else {
                0
            }
        } else {
            self.trend
        };

        let entry_session = ENTRY.contains(&minute);
        let long = self.trend == 1 && !self.traded && d1 < STOCH_LOW && k > d && k1 <= d1;
        let short = self.trend == -1 && !self.traded && d1 > STOCH_HIGH && k < d && k1 >= d1;
        let size = (STAKE / bar.close).floor();
        if long && entry_session {
            strategy.entry("Long", Long).size(size);
            self.traded = true;
        }
        if short && entry_session {
            strategy.entry("Short", Short).size(size);
            self.traded = true;
        }

        let held = strategy.position();
        if let Some(lot) = strategy.lots().first().filter(|_| held != 0.0) {
            let entry = lot.price;
            let width = range_high - range_low;
            let losing = (bar.close - entry) * held <= 0.0;
            if held > 0.0 {
                let stop =
                    if entry > range_high { range_low - margin } else { entry - width - margin };
                let target = entry + (entry - stop) * REWARD_TO_RISK;
                strategy.exit("Exit").from("Long").take(target).stop(stop);
                if d > 80.0 && losing && rsi_bear {
                    strategy.close("Long");
                }
            } else {
                let stop =
                    if entry < range_low { range_high + margin } else { entry + width + margin };
                let target = entry - (stop - entry) * REWARD_TO_RISK;
                strategy.exit("Exit").from("Short").take(target).stop(stop);
                if d < 20.0 && losing && rsi_bull {
                    strategy.close("Short");
                }
            }
        }

        if CLOSING.contains(&minute) && weekday(bar.timestamp) <= FRIDAY {
            strategy.close_all();
            self.traded = false;
            self.range = CLEARED;
        }
    }
}
