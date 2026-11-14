// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Easy Sys 420" by SeaSide420, https://www.tradingview.com/script/zBaDqbla/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-10-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A points system on the open. Fourteen readings each vote a signed amount, and the trade follows a Supertrend of a
//! volume-and-spread line: long (all of the account's worth, at the next open, reversing a short) when the trend is up and the
//! points exceed 15 or the zigzag reads up, short on the mirror with -15. The readings: a parabolic SAR (3) and the trend (3)
//! and the zigzag (1); the open's 20-bar rate of change (1); a directional index of 5 bars, plus over minus (2); a CCI(21)
//! (1); a Weis wave's side (2); the open against its SMA(20) (1); the awesome oscillator (1); the 14-bar momentum (1); the
//! MACD(12, 26, 9) histogram of the open rising (3); a bull-bear balance averaged over 30 bars (1); the open against the
//! 26-bar Ichimoku base line (2); the open against HMA(14) (1). 0.01% commission, 1 tick of slippage; version 4: starts with
//! 100,000.
//!
//! The rule asks for the chart's own timeframe through a security call, which returns the bar itself. Its divergence labels, its
//! ultimate oscillator, its TD counters and its stochastic decide nothing and are left out. The reference's `smapoint` is the
//! current bar's, not a running count, since it adds to the value its own bar declared. The Supertrend of the line is vike's,
//! judged against the last bar's bands (`Supertrend::with_previous_bands`) about the line with the open in the close's place
//! (`Supertrend::next_centred`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{
    Adx, Atr, Cci, Donchian, Ema, Hma, Macd, Mom, Psar, Sma, Stddev, Supertrend,
};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const DEPTH: usize = 12;
const DEVIATION: f64 = 5.0;
const BUY_POINTS: f64 = 15.0;
const AF_INITIAL: f64 = 0.02;
const AF_INCREMENT: f64 = 0.02;
const AF_MAXIMUM: f64 = 0.2;

/// `x[k]` of a history that ends with this bar, NaN before the first.
fn back(history: &[f64], k: usize) -> f64 {
    history.len().checked_sub(1 + k).map_or(f64::NAN, |i| history[i])
}

/// A comparison with a missing value is false, `!=` too.
fn differs(a: f64, b: f64) -> bool {
    !a.is_nan() && !b.is_nan() && a != b
}

fn vote(positive: bool, negative: bool, size: f64) -> f64 {
    if positive {
        size
    } else if negative {
        -size
    } else {
        0.0
    }
}

/// The reference's `iff(close==open, ...)` bull and bear sizes of one bar, with the bar before's close.
fn bull_bear(bar: &Bar, last_close: f64) -> f64 {
    let (o, h, l, c) = (bar.open, bar.high, bar.low, bar.close);
    let max = f64::max;
    let r1 = if last_close < o { max(o - last_close, h - l) } else { h - l };
    let r2 = if last_close > o { max(last_close - o, h - l) } else { h - l };
    let bull = if c == o {
        if h - c == c - l {
            if last_close > o { max(h - o, c - l) } else { r1 }
        } else if h - c > c - l {
            if last_close < o { max(h - last_close, c - l) } else { h - o }
        } else {
            r1
        }
    } else if c < o {
        if last_close < o { max(h - last_close, c - l) } else { max(h - o, c - l) }
    } else {
        r1
    };
    let bear = if c == o {
        if h - c == c - l {
            if last_close < o { max(o - l, h - c) } else { r2 }
        } else if h - c > c - l {
            r2
        } else if last_close > o {
            max(last_close - l, h - c)
        } else {
            o - l
        }
    } else if c < o {
        r2
    } else if last_close > o {
        max(last_close - l, h - c)
    } else {
        max(o - l, h - c)
    };
    bull - bear
}

struct S {
    opens: Vec<f64>,
    highs: Vec<f64>,
    lows: Vec<f64>,
    closes: Vec<f64>,
    /// The 14-bar momentum of the open.
    mom: Mom,
    /// The cumulative sum of the spread-volume line.
    cumulative: f64,
    /// The standard deviation of the bar's range over 28 bars, the SMA(14) of the line, and the standard deviation of the line
    /// less it over 28 bars, which is fed only the numbers that exist.
    spread: Stddev,
    smooth: Sma,
    deviation: Stddev,
    out: Ema,
    atr: Atr,
    trend: Supertrend,
    last_low: f64,
    last_high: f64,
    zigzag_direction: Option<i32>,
    zigzag_low: f64,
    zigzag_high: f64,
    /// The 12-bar channel the zigzag reads, and the 26-bar one the base line is the middle of.
    channel: Donchian,
    base: Donchian,
    sma20: Sma,
    hl2_fast: Sma,
    hl2_slow: Sma,
    macd: Macd,
    hist: f64,
    balance: Sma,
    hma: Hma,
    mov: f64,
    weis_trend: f64,
    wave: f64,
    volume: f64,
    cci: Cci,
    sar: Psar,
    dmi: Adx,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |n: f64| Ema::new(n).expect("a valid length").with_skip_nan(true);
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    let stddev = |n: f64| Stddev::new(n).expect("a valid length");
    let donchian = |n: f64| Donchian::new(n).expect("a valid length");
    Box::new(S {
        opens: Vec::new(),
        highs: Vec::new(),
        lows: Vec::new(),
        closes: Vec::new(),
        mom: Mom::new(14.0).expect("a valid length"),
        cumulative: 0.0,
        spread: stddev(28.0),
        smooth: sma(14.0),
        deviation: stddev(28.0),
        out: ema(5.0),
        atr: Atr::new(7.0).expect("a valid length"),
        trend: Supertrend::new(7.0, 3.0).expect("valid parameters").with_previous_bands(true),
        last_low: 0.0,
        last_high: 0.0,
        zigzag_direction: None,
        zigzag_low: f64::NAN,
        zigzag_high: f64::NAN,
        channel: donchian(DEPTH as f64),
        base: donchian(26.0),
        sma20: sma(20.0),
        hl2_fast: sma(5.0),
        hl2_slow: sma(34.0),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        hist: f64::NAN,
        balance: sma(30.0),
        hma: Hma::new(14.0).expect("a valid length"),
        mov: f64::NAN,
        weis_trend: f64::NAN,
        wave: f64::NAN,
        volume: f64::NAN,
        cci: Cci::new(21.0).expect("a valid length"),
        // The rule's own SAR: the bar's extreme and factor first, then the move, the clamp and the reversal test.
        sar: Psar::new(AF_INCREMENT, AF_MAXIMUM)
            .and_then(|p| p.with_start(AF_INITIAL))
            .expect("valid factors")
            .with_extreme_first(true),
        dmi: Adx::new(5.0)
            .expect("a valid length")
            .with_first_bar_moves(true)
            .with_held_lines(true),
    })
}

impl S {
    /// The open `k` bars back.
    fn source_at(&self, k: usize) -> f64 {
        back(&self.opens, k)
    }

    /// The parabolic SAR of this bar, read against the open.
    fn sar_point(&mut self, src: f64) -> f64 {
        let sar = self.sar.next_hl(back(&self.highs, 0), back(&self.lows, 0));
        if src - sar > 0.0 { 3.0 } else { -3.0 }
    }

    /// The zigzag's side, 1 or -1, its least move counted in ticks of `tick`.
    fn zigzag(&mut self, highest: f64, lowest: f64, tick: f64) -> f64 {
        let (h, l) = (back(&self.highs, 0), back(&self.lows, 0));
        let min_move = DEVIATION * tick;
        // getLow: the lowest of the last bars against the one the rule kept.
        let kept = nan_to_zero(self.last_low);
        let mut v = lowest;
        let m = (!v.is_nan() && v == kept) || l - v > min_move;
        let mut e = kept;
        if differs(v, kept) {
            e = v;
        }
        if m {
            v = 0.0;
        }
        let z_bb = differs(v, 0.0);
        self.last_low = e;
        // getHigh, the mirror.
        let kept = nan_to_zero(self.last_high);
        let mut v = highest;
        let m = (!v.is_nan() && v == kept) || v - h > min_move;
        let mut e = kept;
        if differs(v, kept) {
            e = v;
        }
        if m {
            v = 0.0;
        }
        let z_ss = differs(v, 0.0);
        self.last_high = e;

        let was = self.zigzag_direction;
        let direction = if z_bb {
            0
        } else if z_ss {
            1
        } else {
            was.unwrap_or(-1)
        };
        self.zigzag_direction = Some(direction);
        let same = was == Some(direction);
        let virtual_low = self.zigzag_low + 1.0;
        let virtual_high = self.zigzag_high + 1.0;
        let at =
            |history: &[f64], k: f64| if k.is_nan() { f64::NAN } else { back(history, k as usize) };
        if !z_bb || (same && l > at(&self.lows, virtual_low)) {
            self.zigzag_low = nan_to_zero(self.zigzag_low) + 1.0;
        } else {
            self.zigzag_low = 0.0;
        }
        if !z_ss || (same && h < at(&self.highs, virtual_high)) {
            self.zigzag_high = nan_to_zero(self.zigzag_high) + 1.0;
        } else {
            self.zigzag_high = 0.0;
        }
        if self.zigzag_low > self.zigzag_high { 1.0 } else { -1.0 }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let sizing = strategy.sizing();
        let src = bar.open;
        let last_close = back(&self.closes, 0);
        let last_source = back(&self.opens, 0);
        self.opens.push(bar.open);
        self.highs.push(bar.high);
        self.lows.push(bar.low);
        self.closes.push(bar.close);
        // The volume-and-spread line, its Supertrend.
        let range = bar.high - bar.low;
        let spreadvol = (bar.close - bar.open) * 100.0 * (bar.volume / (range * 100.0));
        self.cumulative += spreadvol;
        let vp = spreadvol + self.cumulative;
        let spread = self.spread.next_value(range);
        let smooth = self.smooth.next_value(vp);
        let deviation =
            if smooth.is_nan() { f64::NAN } else { self.deviation.next_value(vp - smooth) };
        let shadow = (vp - smooth) / deviation * spread;
        let out1 = if shadow > 0.0 { bar.high + shadow } else { bar.low + shadow };
        let vpt = self.out.next_value(out1);
        let atr = self.atr.next(bar);
        let trend = if self.trend.next_centred(src, vpt, atr)[1] > 0.0 { 1 } else { -1 };
        let stt = if trend == 1 { 3.0 } else { -3.0 };

        let [channel_high, _, channel_low] = self.channel.next(bar);
        let zig = self.zigzag(channel_high, channel_low, tick);

        // The readings.
        let smapoint = if src > self.sma20.next_value(src) { 1.0 } else { -1.0 };
        let hl2 = (bar.high + bar.low) / 2.0;
        let ao = self.hl2_fast.next_value(hl2) - self.hl2_slow.next_value(hl2);
        let aopoint = vote(ao > 0.0, ao < 0.0, 1.0);
        let mom = self.mom.next_value(src);
        let mompoint = vote(mom > 0.0, mom < 0.0, 1.0);
        let [_, _, hist] = self.macd.next_value(src);
        let histpoint = if hist > self.hist { 3.0 } else { -3.0 };
        self.hist = hist;
        let bbpoint =
            if self.balance.next_value(bull_bear(bar, last_close)) > 0.0 { 1.0 } else { -1.0 };
        let [_, base_line, _] = self.base.next(bar);
        let icpoint = if src > base_line { 2.0 } else { -2.0 };
        let hmapoint = if src > self.hma.next_value(src) { 1.0 } else { -1.0 };

        // The Weis wave.
        let mov = if src > last_source {
            1.0
        } else if src < last_source {
            -1.0
        } else {
            0.0
        };
        self.weis_trend =
            if mov != 0.0 && differs(mov, self.mov) { mov } else { nan_to_zero(self.weis_trend) };
        self.mov = mov;
        let rising = (0..4).all(|k| self.source_at(k) > self.source_at(k + 1));
        let falling = (0..4).all(|k| self.source_at(k) < self.source_at(k + 1));
        let was_wave = self.wave;
        if differs(self.weis_trend, nan_to_zero(was_wave)) && (rising || falling) {
            self.wave = self.weis_trend;
        } else {
            self.wave = nan_to_zero(was_wave);
        }
        self.volume =
            if self.wave == was_wave { nan_to_zero(self.volume) + bar.volume } else { bar.volume };
        let (up1, down1) = if self.wave == 1.0 { (self.volume, 0.0) } else { (0.0, self.volume) };
        let weis = if up1 > down1 { 2.0 } else { -2.0 };

        let roc = if src > self.source_at(20) { 1.0 } else { -1.0 };
        // The typical price of three equal prices is that price, so this is the CCI of the open.
        let cci = if self.cci.next_hlc(src, src, src) > 0.0 { 1.0 } else { -1.0 };

        // The directional index of 5 bars: the first bar's moves count as 0 while it has no true range, and a line holds its
        // last number.
        let [_, plus, minus] = self.dmi.next(bar);
        let dmi = if plus - minus >= 0.0 { 2.0 } else { -2.0 };

        let sar = self.sar_point(src);
        let total = sar
            + stt
            + zig
            + roc
            + dmi
            + cci
            + weis
            + smapoint
            + aopoint
            + mompoint
            + histpoint
            + bbpoint
            + icpoint
            + hmapoint;
        if (total > BUY_POINTS && trend == 1) || (zig == 1.0 && trend == 1) {
            strategy.entry("BUY", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if (total < -BUY_POINTS && trend == -1) || (zig == -1.0 && trend == -1) {
            strategy.entry("SELL", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
