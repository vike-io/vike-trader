// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Precision Confluence Trading Strategy [JOAT]" by officialjackofalltrades, https://www.tradingview.com/script/Eexf5DPB/
// The original's notice: © officialjackofalltrades
// Changed: rewritten by hand in Rust for vike.

//! A confluence score for each side, out of the readings below; one position at a time, 10% of the account's worth at the
//! next open, with a stop and a target stated at the signal bar's close. Long when the bull score is 70 or more with the bear
//! under 40 (strong), or 55 or more and over the bear by 15 (medium), and at least 50; short on the mirror. The stop is the
//! lower of the close less 1.5 ATR(14) (0.8 of that for an ultra signal, 1.2 for a medium one) and the 10-bar low less 0.2 ATR; the
//! target is 2.5 times the stop's distance past the close. Once the close is 1.2 times the distance from the entry bar's close in
//! profit, the stop and target are stated again, at 0.1 and 2.5 times it from that close, the distance read from this bar's stop.
//! 0.1% commission, 2 ticks of slippage; version 6: starts with 10,000, margined at 100%.
//!
//! The readings, each scoring for a side: the daily and weekly CPR from the day and week before (a call with lookahead that
//! reads their `[1]`, so nothing it reads lies ahead; the days are the symbol's daily input, the weeks built from them,
//! their history before the window included: `vike_tactics::higher::HigherTimeframeSourceCalendar`); a ribbon of five Hull averages whose half length is
//! cut to a whole number and whose outer length is the root cut too; EMA(9) over EMA(21); a WaveTrend of the typical price
//! with its crosses, divergences and momentum; RSI(14) divergences at 5-bar pivots; DMI(14) and its ADX over 20; the volume
//! against its SMA(20) with a cumulative delta; fair value gaps, order blocks and their breakers, liquidity sweeps and
//! displacement; and SMA(50) and SMA(200). The multi-timeframe confirmation, the session filter and the trailing stop are off
//! or unread, and the calls for 15, 60 and 240 minutes feed only that confirmation.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{
    Adx, Atr, ConfirmedPivot, Donchian, Ema, Hma, Rsi, Sma, Wavetrend,
};
use vike_marketdata::Bar;
use vike_model::MS_PER_DAY;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const REWARD: f64 = 2.5;
const BREAKEVEN: f64 = 1.2;
const ATR_MULTIPLIER: f64 = 1.5;
const PIVOT: usize = 5;

/// `x[k]` of a history that ends with this bar, NaN before the first.
fn back(history: &[f64], k: usize) -> f64 {
    history.len().checked_sub(1 + k).map_or(f64::NAN, |i| history[i])
}

/// One oscillator's divergences at its pivots: the price and value of the last pivot each side.
struct Divergence {
    pivots: ConfirmedPivot,
    high: (f64, f64),
    low: (f64, f64),
}

impl Divergence {
    fn new() -> Self {
        Divergence {
            pivots: ConfirmedPivot::new(PIVOT as f64, PIVOT as f64).expect("valid spans"),
            high: (f64::NAN, f64::NAN),
            low: (f64::NAN, f64::NAN),
        }
    }

    /// `(bull, bear)` on this bar, `series` being the oscillator's history to this bar.
    fn step(&mut self, series: &[f64], highs: &[f64], lows: &[f64]) -> (bool, bool) {
        let osc = back(series, 0);
        let [pivot_high, pivot_low, ..] = self.pivots.next_hl(osc, osc);
        let (mut bull, mut bear) = (false, false);
        if !pivot_high.is_nan() {
            let (price, value) = (back(highs, PIVOT), back(series, PIVOT));
            if !self.high.0.is_nan() {
                bear = price > self.high.0 && value < self.high.1;
            }
            self.high = (price, value);
        }
        if !pivot_low.is_nan() {
            let (price, value) = (back(lows, PIVOT), back(series, PIVOT));
            if !self.low.0.is_nan() {
                bull = price < self.low.0 && value > self.low.1;
            }
            self.low = (price, value);
        }
        (bull, bear)
    }
}

/// One closed period: its open time and the three prices a CPR reads.
#[derive(Clone, Copy)]
struct Period {
    timestamp: i64,
    high: f64,
    low: f64,
    close: f64,
}

const NONE: Period = Period { timestamp: i64::MIN, high: f64::NAN, low: f64::NAN, close: f64::NAN };

/// The central pivot range of a period: `(pivot, bottom, top)`.
fn cpr(p: Period) -> (f64, f64, f64) {
    let pivot = (p.high + p.low + p.close) / 3.0;
    let bottom = (p.high + p.low) / 2.0;
    (pivot, bottom, (pivot - bottom) + pivot)
}

struct S {
    days: HigherTimeframeSourceCalendar,
    weeks: HigherTimeframeSourceCalendar,
    day: (Period, Period),
    week: (Period, Period),
    opens: Vec<f64>,
    highs: Vec<f64>,
    lows: Vec<f64>,
    closes: Vec<f64>,
    /// The rule's Hull ribbon: vike's `Hma`, whose half and root lengths are cut to whole numbers as the rule's are.
    hulls: [Hma; 5],
    hma21_last: f64,
    ema_fast: Ema,
    ema_slow: Ema,
    sma50: Sma,
    sma200: Sma,
    wave: Wavetrend,
    wt1s: Vec<f64>,
    cross: Cross,
    momentum_last: f64,
    wave_div: Divergence,
    rsi: Rsi,
    rsis: Vec<f64>,
    rsi_div: Divergence,
    adx: Adx,
    volume_average: Sma,
    delta_average: Sma,
    cum_buy: f64,
    cum_sell: f64,
    last_bull_ob_high: f64,
    last_bear_ob_low: f64,
    /// The 20-bar extremes and the 10-bar ones, and the 20-bar ones as the bar before left them.
    recent: Donchian,
    swing: Donchian,
    recent_high: f64,
    recent_low: f64,
    atr: Atr,
    entry_price: f64,
    moved: bool,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        weeks: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Week),
        day: (NONE, NONE),
        week: (NONE, NONE),
        opens: Vec::new(),
        highs: Vec::new(),
        lows: Vec::new(),
        closes: Vec::new(),
        hulls: [8.0, 13.0, 21.0, 34.0, 55.0].map(|n| Hma::new(n).expect("a valid length")),
        hma21_last: f64::NAN,
        ema_fast: ema(9.0),
        ema_slow: ema(21.0),
        sma50: Sma::new(50.0).expect("a valid length"),
        sma200: Sma::new(200.0).expect("a valid length"),
        wave: Wavetrend::new(10.0, 21.0).expect("valid lengths"),
        wt1s: Vec::new(),
        cross: Cross::new(),
        momentum_last: f64::NAN,
        wave_div: Divergence::new(),
        rsi: Rsi::new(14.0).expect("a valid length").with_rs_form(true),
        rsis: Vec::new(),
        rsi_div: Divergence::new(),
        adx: Adx::new(14.0).expect("a valid length").with_held_lines(true),
        volume_average: Sma::new(20.0).expect("a valid length"),
        delta_average: Sma::new(20.0).expect("a valid length"),
        cum_buy: 0.0,
        cum_sell: 0.0,
        last_bull_ob_high: f64::NAN,
        last_bear_ob_low: f64::NAN,
        recent: Donchian::new(20.0).expect("a valid length"),
        swing: Donchian::new(10.0).expect("a valid length"),
        recent_high: f64::NAN,
        recent_low: f64::NAN,
        atr: Atr::new(14.0).expect("a valid length"),
        entry_price: f64::NAN,
        moved: false,
    })
}

/// The points a condition gives.
fn pts(cond: bool, n: f64) -> f64 {
    if cond { n } else { 0.0 }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let (open, high, low, close, volume) = (bar.open, bar.high, bar.low, bar.close, bar.volume);
        for d in self.days.next(bar, strategy.input(higher::DAY)) {
            self.day = (
                self.day.1,
                Period { timestamp: d.timestamp, high: d.high, low: d.low, close: d.close },
            );
        }
        for w in self.weeks.next(bar, strategy.input(higher::DAY)) {
            self.week = (
                self.week.1,
                Period { timestamp: w.timestamp, high: w.high, low: w.low, close: w.close },
            );
        }
        // On the last bar of a period the period itself has just closed: the one before is the period before it.
        let today = bar.timestamp - bar.timestamp.rem_euclid(MS_PER_DAY);
        let day = if self.day.1.timestamp < today { self.day.1 } else { self.day.0 };
        let this_week = HigherTimeframePeriod::Week.start(bar.timestamp);
        let week = if self.week.1.timestamp < this_week { self.week.1 } else { self.week.0 };

        self.opens.push(open);
        self.highs.push(high);
        self.lows.push(low);
        self.closes.push(close);
        let (c1, o1, h1, l1) = (
            back(&self.closes, 1),
            back(&self.opens, 1),
            back(&self.highs, 1),
            back(&self.lows, 1),
        );
        let (c2, o2, h2, l2) = (
            back(&self.closes, 2),
            back(&self.opens, 2),
            back(&self.highs, 2),
            back(&self.lows, 2),
        );

        // CPR.
        let (d_pivot, d_bottom, d_top) = cpr(day);
        let (w_pivot, ..) = cpr(week);
        let d_width = ((d_top - d_bottom) / d_pivot) * 100.0;
        let cpr_bull = close > d_pivot && close > w_pivot;
        let cpr_bear = close < d_pivot && close < w_pivot;
        let narrow = d_width < 0.5;
        let cpr_bull_score = pts(cpr_bull, 10.0) + pts(close > d_top, 3.0) + pts(narrow, 2.0);
        let cpr_bear_score = pts(cpr_bear, 10.0) + pts(close < d_bottom, 3.0) + pts(narrow, 2.0);

        // The Hull ribbon.
        let [h8, h13, h21, h34, h55] = {
            let mut out = [0.0; 5];
            for (slot, hull) in out.iter_mut().zip(self.hulls.iter_mut()) {
                *slot = hull.next_value(close);
            }
            out
        };
        let hma21_last = std::mem::replace(&mut self.hma21_last, h21);
        let hma_bull = close > h21 && h21 > hma21_last;
        let hma_bear = close < h21 && h21 < hma21_last;
        let full_bull = h8 > h13 && h13 > h21 && h21 > h34 && h34 > h55;
        let full_bear = h8 < h13 && h13 < h21 && h21 < h34 && h34 < h55;
        let fast = self.ema_fast.next_value(close);
        let slow = self.ema_slow.next_value(close);
        let hma_bull_score = pts(hma_bull, 5.0) + pts(full_bull, 7.0) + pts(fast > slow, 3.0);
        let hma_bear_score = pts(hma_bear, 5.0) + pts(full_bear, 7.0) + pts(fast < slow, 3.0);
        let sma50 = self.sma50.next_value(close);
        let sma200 = self.sma200.next_value(close);

        // WaveTrend of the typical price.
        let [wt1, wt2] = self.wave.next_hlc(high, low, close);
        self.wt1s.push(wt1);
        let cross = self.cross.step(wt1, wt2);
        let cross_up = matches!(cross, Signal::Long);
        let cross_down = matches!(cross, Signal::Short);
        let (wt_bull_div, wt_bear_div) = self.wave_div.step(&self.wt1s, &self.highs, &self.lows);
        let momentum = wt1 - wt2;
        let momentum_last = std::mem::replace(&mut self.momentum_last, momentum);
        let momentum_bull = momentum > 0.0 && momentum > momentum_last;
        let momentum_bear = momentum < 0.0 && momentum < momentum_last;
        let wt_bull_score = (if cross_up && wt1 < -60.0 { 8.0 } else { pts(cross_up, 5.0) })
            + pts(wt_bull_div, 5.0)
            + pts(momentum_bull, 2.0);
        let wt_bear_score = (if cross_down && wt1 > 60.0 { 8.0 } else { pts(cross_down, 5.0) })
            + pts(wt_bear_div, 5.0)
            + pts(momentum_bear, 2.0);

        // RSI divergence.
        self.rsis.push(self.rsi.next_value(close));
        let (rsi_bull_div, rsi_bear_div) = self.rsi_div.step(&self.rsis, &self.highs, &self.lows);
        let div_bull_score = pts(rsi_bull_div, 5.0) + pts(rsi_bull_div && wt_bull_div, 5.0);
        let div_bear_score = pts(rsi_bear_div, 5.0) + pts(rsi_bear_div && wt_bear_div, 5.0);

        // ADX.
        let [adx, plus, minus] = self.adx.next(bar);
        let strong = adx > 20.0;
        let adx_bull_score = if strong && plus > minus { 10.0 } else { pts(plus > minus, 5.0) };
        let adx_bear_score = if strong && minus > plus { 10.0 } else { pts(minus > plus, 5.0) };

        // Volume.
        let vol_ma = self.volume_average.next_value(volume);
        let high_volume = volume > vol_ma * 1.5;
        let climax = volume > vol_ma * 3.0;
        let bull_volume = close > open && high_volume;
        let bear_volume = close < open && high_volume;
        self.cum_buy += if close > open { volume } else { 0.0 };
        self.cum_sell += if close < open { volume } else { 0.0 };
        let delta = self.cum_buy - self.cum_sell;
        let delta_ma = self.delta_average.next_value(delta);
        let bull_confirmed = bull_volume && delta > delta_ma;
        let bear_confirmed = bear_volume && delta < delta_ma;
        let vol_bull_score = (if bull_confirmed { 7.0 } else { pts(bull_volume, 5.0) })
            + pts(climax && close > open, 3.0);
        let vol_bear_score = (if bear_confirmed { 7.0 } else { pts(bear_volume, 5.0) })
            + pts(climax && close < open, 3.0);

        // Smart money concepts.
        let bull_fvg = low > h2 && c1 > h2;
        let bull_fvg_size = if bull_fvg { ((low - h2) / h2) * 100.0 } else { 0.0 };
        let bear_fvg = high < l2 && c1 < l2;
        let bear_fvg_size = if bear_fvg { ((l2 - high) / high) * 100.0 } else { 0.0 };
        let sig_bull_fvg = bull_fvg && bull_fvg_size > 0.3;
        let sig_bear_fvg = bear_fvg && bear_fvg_size > 0.3;
        let bull_ob = c2 < o2 && c1 < o1 && close > open && high_volume;
        let bear_ob = c2 > o2 && c1 > o1 && close < open && high_volume;
        if bull_ob {
            self.last_bull_ob_high = h1;
        }
        if bear_ob {
            self.last_bear_ob_low = l1;
        }
        let breaker_bull = !self.last_bear_ob_low.is_nan()
            && close > self.last_bear_ob_low
            && c1 <= self.last_bear_ob_low;
        let breaker_bear = !self.last_bull_ob_high.is_nan()
            && close < self.last_bull_ob_high
            && c1 >= self.last_bull_ob_high;
        let (recent_high, recent_low) = (self.recent_high, self.recent_low);
        let [recent_highest, _, recent_lowest] = self.recent.next(bar);
        (self.recent_high, self.recent_low) = (recent_highest, recent_lowest);
        let sweep_high = high > recent_high && close < recent_high;
        let sweep_low = low < recent_low && close > recent_low;
        let atr = self.atr.next(bar);
        let displacement = (close - open).abs() > atr * 2.0 && climax;
        let smc_bull_score = pts(sig_bull_fvg, 2.0)
            + pts(bull_ob, 2.0)
            + pts(breaker_bull, 1.0)
            + pts(sweep_low && high_volume, 2.0)
            + pts(displacement && close > open, 3.0);
        let smc_bear_score = pts(sig_bear_fvg, 2.0)
            + pts(bear_ob, 2.0)
            + pts(breaker_bear, 1.0)
            + pts(sweep_high && high_volume, 2.0)
            + pts(displacement && close < open, 3.0);

        // Key averages.
        let ma_bull_score =
            pts(close > sma50, 3.0) + pts(close > sma200, 4.0) + pts(sma50 > sma200, 3.0);
        let ma_bear_score =
            pts(close < sma50, 3.0) + pts(close < sma200, 4.0) + pts(sma50 < sma200, 3.0);

        let bull = cpr_bull_score
            + hma_bull_score
            + wt_bull_score
            + div_bull_score
            + adx_bull_score
            + vol_bull_score
            + smc_bull_score
            + ma_bull_score;
        let bear = cpr_bear_score
            + hma_bear_score
            + wt_bear_score
            + div_bear_score
            + adx_bear_score
            + vol_bear_score
            + smc_bear_score
            + ma_bear_score;
        let strong_long = bull >= 70.0 && bear < 40.0;
        let strong_short = bear >= 70.0 && bull < 40.0;
        let medium_long = bull >= 55.0 && bull > bear + 15.0 && !strong_long;
        let medium_short = bear >= 55.0 && bear > bull + 15.0 && !strong_short;
        let long_signal = (strong_long || medium_long) && bull >= 50.0;
        let short_signal = (strong_short || medium_short) && bear >= 50.0;
        let ultra_long = bull >= 85.0
            && bear < 25.0
            && (cross_up || rsi_bull_div || (displacement && close > open));
        let ultra_short = bear >= 85.0
            && bull < 25.0
            && (cross_down || rsi_bear_div || (displacement && close < open));
        let multiplier = if ultra_long || ultra_short {
            ATR_MULTIPLIER * 0.8
        } else if strong_long || strong_short {
            ATR_MULTIPLIER
        } else {
            ATR_MULTIPLIER * 1.2
        };
        let [swing_high, _, swing_low] = self.swing.next(bar);
        let stop_bull = (close - atr * multiplier).min(swing_low - atr * 0.2);
        let stop_bear = (close + atr * multiplier).max(swing_high + atr * 0.2);

        let held = strategy.position();
        if long_signal && held == 0.0 {
            let distance = close - stop_bull;
            strategy.entry("Long", Long).size(share_of_worth(&sizing, 10.0, close));
            strategy.exit("Long Exit").from("Long").take(close + distance * REWARD).stop(stop_bull);
            self.entry_price = close;
            self.moved = false;
        }
        if short_signal && held == 0.0 {
            let distance = stop_bear - close;
            strategy.entry("Short", Short).size(share_of_worth(&sizing, 10.0, close));
            strategy
                .exit("Short Exit")
                .from("Short")
                .take(close - distance * REWARD)
                .stop(stop_bear);
            self.entry_price = close;
            self.moved = false;
        }
        let entry = self.entry_price;
        if held > 0.0 && !self.moved {
            let distance = entry - stop_bull;
            if close >= entry + distance * BREAKEVEN {
                strategy
                    .exit("Long Exit")
                    .from("Long")
                    .take(entry + distance * REWARD)
                    .stop(entry + distance * 0.1);
                self.moved = true;
            }
        }
        if held < 0.0 && !self.moved {
            let distance = stop_bear - entry;
            if close <= entry - distance * BREAKEVEN {
                strategy
                    .exit("Short Exit")
                    .from("Short")
                    .take(entry - distance * REWARD)
                    .stop(entry - distance * 0.1);
                self.moved = true;
            }
        }
    }
}
