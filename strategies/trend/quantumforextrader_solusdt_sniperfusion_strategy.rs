// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "QuantumForexTrader_SOLUSDT_SniperFusion_Strategy_v2" by finnp17, https://www.tradingview.com/script/oq1Ju4Ha/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2026-09-16, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A long-only score of trend, momentum and volume. Every bar it counts the last 72 closes' rises and falls, the older ones
//! weighing less (`1 + (72 - i) / 72` for the close `i` bars back), which gives a direction, its confidence (the larger
//! share, at least 0.54 to score) and its momentum (the shares' difference, at least 0.08), and a memory of the direction
//! decaying by 0.88 a bar. A sniper count of seven reads, each side (the close against the day's VWAP of the typical price,
//! RSI(14) against 50, MACD(12, 26, 9) against its signal, EMA(9) against EMA(21), ADX(14) over 25 with the close beyond
//! EMA(9), the volume over its 20-bar average on a candle of that colour, and the 5-minute RSI against 50), makes a bias; a
//! hybrid score adds the direction, the memory, an EMA(34) trend with its slope, the same trend on 5-minute bars, the volume
//! over its 24-bar average, ATR(14) at least 0.95 of its 21-bar average, a body of at least 0.35 of the range, and the bias.
//! A long signal is a score of 8 or more leading the short score by 2 or more, with the bias at least 57% bullish (or 40
//! points ahead) and the close at least 0.3 ATR above the last entry's close; or EMA(9) crossing up through EMA(21) with that
//! bias, the close above the VWAP, the MACD above its signal and the 5-minute RSI above 50. A short signal, the mirror, wins
//! over a long one when the bias leans that way.
//!
//! Flat on a long signal, a long of the account's whole worth at this close. From the next bar a stop rests 1.5 ATR below
//! the average fill, ATR as of that bar, raised to 1.5 ATR under the highest high once the high has run 0.5 ATR past the
//! fill; five targets of 20% each at 1 to 5 times that risk are issued after the stop, which already holds the whole
//! position, so none of them gets a share. The early exits (a cross under EMA(9), RSI falling back under 75, a bearish
//! engulfing, or a short signal) wait for three bars in the trade counted from the entry bar, which the rule forgets on the
//! bar it enters (it clears its trade while still flat), so they never close a trade; they are kept as written. Orders fill
//! at the close of the bar that places them. Shorts are off by default and left out. 0.05% commission and 2 ticks of
//! slippage; starts with 10,000, the language's default margin of 100%.
//!
//! A lower timeframe is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close:
//! its RSI and its EMA(34) over its bars from the rule's first bar on, each read one 5-minute bar back as the rule asks, as
//! they stand after the last of them inside the chart bar.
//!
//! Left out, as no order reads them: the session filter (off by default), the dashboard, the lines, labels and plots, and
//! the webhook messages.
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

use vike_indicators::indicators::{Adx, Atr, Ema, Macd, Rsi, Sma, Vwap, VwapSource};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIVE_MINUTES: &str = "5m";
const HISTORY: usize = 72;
const CONFIDENCE: f64 = 0.54;
const MOMENTUM: f64 = 0.08;
const MEMORY_DECAY: f64 = 0.88;
const MEMORY_THRESHOLD: f64 = 0.05;
const TREND_LENGTH: f64 = 34.0;
const VOLUME_LOOKBACK: f64 = 24.0;
const VOLUME_MULTIPLIER: f64 = 1.0;
const ATR_LENGTH: f64 = 14.0;
const ATR_AVERAGE: f64 = 21.0;
const ATR_MULTIPLIER: f64 = 0.95;
const BODY_RATIO: f64 = 0.35;
const SNIPER_MIN: f64 = 57.0;
const STRONG_BIAS: f64 = 40.0;
const SCORE_MIN: i32 = 8;
const EDGE_MIN: i32 = 2;
const REARM_ATR: f64 = 0.30;
const HOLD_BARS: i64 = 3;
const STOP_ATR: f64 = 1.5;
const TARGET_STEP: f64 = 1.0;
const TARGET_SHARE: f64 = 0.2;
const TRAIL_ATR: f64 = 1.5;
const TRAIL_ACTIVATION_ATR: f64 = 0.5;
const RSI_OVERBOUGHT: f64 = 75.0;
const ADX_KEY: f64 = 25.0;
const TARGETS: [&str; 5] =
    ["long target 1", "long target 2", "long target 3", "long target 4", "long target 5"];

/// The 5-minute reads: `ta.rsi(close, 14)[1]` and `(close > ema and ema > ema[1])[1]` (and the mirror) of EMA(34).
struct FiveMinutes {
    rsi: Rsi,
    trend: Ema,
    trend_before: f64,
    /// This 5-minute bar's values and the one's before: `(rsi, bull, bear)`.
    now: (f64, bool, bool),
    before: (f64, bool, bool),
}

impl FiveMinutes {
    fn next(&mut self, bar: &Bar) {
        let rsi = self.rsi.next(bar);
        let trend = self.trend.next(bar);
        let bull = bar.close > trend && trend > self.trend_before;
        let bear = bar.close < trend && trend < self.trend_before;
        self.trend_before = trend;
        self.before = std::mem::replace(&mut self.now, (rsi, bull, bear));
    }
}

/// The trade's levels as the rule keeps them.
#[derive(Clone, Copy)]
struct Trade {
    entry: f64,
    stop: f64,
    trailing: f64,
    highest: f64,
    trailing_active: bool,
    anchored: bool,
    entry_bar: Option<i64>,
    targets: [f64; 5],
    hits: [bool; 5],
}

impl Trade {
    const NONE: Trade = Trade {
        entry: f64::NAN,
        stop: f64::NAN,
        trailing: f64::NAN,
        highest: f64::NAN,
        trailing_active: false,
        anchored: false,
        entry_bar: None,
        targets: [f64::NAN; 5],
        hits: [false; 5],
    };

    /// The stop and the targets `risk` from `price`.
    fn levels(&mut self, price: f64, risk: f64) {
        self.entry = price;
        self.stop = price - risk;
        for (k, target) in self.targets.iter_mut().enumerate() {
            *target = price + risk * TARGET_STEP * (k + 1) as f64;
        }
    }
}

struct S {
    from: Option<i64>,
    bar_index: i64,
    /// The closes before this bar, newest first, at most `HISTORY + 1`.
    closes: VecDeque<f64>,
    open_before: f64,
    close_before: f64,
    fast: Ema,
    slow: Ema,
    vwap: Vwap,
    atr: Atr,
    rsi: Rsi,
    rsi_before: f64,
    macd: Macd,
    dmi: Adx,
    volume_average: Sma,
    trend: Ema,
    trend_before: f64,
    volume_lookback: Sma,
    atr_base: Sma,
    fives: HigherTimeframeSourceUnseen,
    five_minutes: FiveMinutes,
    memory: f64,
    sniper_cross: Cross,
    exit_cross: Cross,
    last_signal_bar: Option<i64>,
    last_signal_long: Option<bool>,
    last_signal_price: f64,
    trade: Trade,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let ema = |length: f64| Ema::new(length).expect("a valid length");
    let sma = |length: f64| Sma::new(length).expect("a valid length");
    Box::new(S {
        from: None,
        bar_index: 0,
        closes: VecDeque::with_capacity(HISTORY + 2),
        open_before: f64::NAN,
        close_before: f64::NAN,
        fast: ema(9.0),
        slow: ema(21.0),
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        atr: Atr::new(ATR_LENGTH).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        rsi_before: f64::NAN,
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        dmi: Adx::new(14.0).expect("a valid length").with_held_lines(true),
        volume_average: sma(20.0),
        trend: ema(TREND_LENGTH),
        trend_before: f64::NAN,
        volume_lookback: sma(VOLUME_LOOKBACK),
        atr_base: sma(ATR_AVERAGE),
        fives: HigherTimeframeSourceUnseen::new(),
        five_minutes: FiveMinutes {
            rsi: Rsi::new(14.0).expect("a valid length"),
            trend: ema(TREND_LENGTH),
            trend_before: f64::NAN,
            now: (f64::NAN, false, false),
            before: (f64::NAN, false, false),
        },
        memory: 0.0,
        sniper_cross: Cross::new(),
        exit_cross: Cross::new(),
        last_signal_bar: None,
        last_signal_long: None,
        last_signal_price: f64::NAN,
        trade: Trade::NONE,
    })
}

/// 1 when `condition` holds, else 0.
fn one(condition: bool) -> i32 {
    i32::from(condition)
}

impl S {
    /// The weighted shares of rises and falls among the last `HISTORY` closes before this bar.
    fn probabilities(&self) -> (f64, f64) {
        let (mut up, mut down) = (0.0, 0.0);
        for i in 1..=HISTORY.min(self.closes.len().saturating_sub(1)) {
            let change = self.closes[i - 1] - self.closes[i];
            let weight = 1.0 + (HISTORY - i) as f64 / HISTORY as f64;
            if change > 0.0 {
                up += weight;
            } else if change < 0.0 {
                down += weight;
            }
        }
        let total = up + down;
        if total > 0.0 { (up / total, down / total) } else { (0.0, 0.0) }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIVE_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let fives = self.fives.next(strategy.input(FIVE_MINUTES));
        for five in &fives[fives.partition_point(|b| b.timestamp < from)..] {
            self.five_minutes.next(five);
        }
        let (rsi_five, five_bull, five_bear) = self.five_minutes.before;

        let (open, high, low, close, volume) = (bar.open, bar.high, bar.low, bar.close, bar.volume);
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let vwap = self.vwap.next(bar);
        let atr = self.atr.next(bar);
        let rsi = self.rsi.next(bar);
        let [macd, signal, _] = self.macd.next(bar);
        let [adx, _, _] = self.dmi.next(bar);
        let volume_average = self.volume_average.next_value(volume);

        let (bull_probability, bear_probability) = self.probabilities();
        let direction = if bull_probability > bear_probability {
            1
        } else if bear_probability > bull_probability {
            -1
        } else {
            0
        };
        let momentum = (bull_probability - bear_probability).abs();
        let confidence = one(bull_probability.max(bear_probability) >= CONFIDENCE);
        let momentum_score = one(momentum >= MOMENTUM);

        let trend = self.trend.next(bar);
        let slope = trend - self.trend_before;
        self.trend_before = trend;
        let trend_bull = close > trend && slope > 0.0;
        let trend_bear = close < trend && slope < 0.0;
        let volume_sma = self.volume_lookback.next_value(volume);
        let volume_ok = volume > volume_sma * VOLUME_MULTIPLIER;
        let atr_base = self.atr_base.next_value(atr);
        let atr_ok = atr >= atr_base * ATR_MULTIPLIER;
        let range = (high - low).max(strategy.tick().unwrap_or(0.0));
        let body = (close - open).abs() / range;
        let bull_candle = close > open && body >= BODY_RATIO;
        let bear_candle = close < open && body >= BODY_RATIO;

        let bull_count = one(close > vwap)
            + one(rsi > 50.0)
            + one(macd > signal)
            + one(fast > slow)
            + one(adx > ADX_KEY && close > fast)
            + one(volume > volume_average && close > open)
            + one(rsi_five > 50.0);
        let bear_count = one(close < vwap)
            + one(rsi < 50.0)
            + one(macd < signal)
            + one(fast < slow)
            + one(adx > ADX_KEY && close < fast)
            + one(volume > volume_average && close < open)
            + one(rsi_five < 50.0);
        let bull_pct = f64::from(bull_count) / 7.0 * 100.0;
        let bear_pct = f64::from(bear_count) / 7.0 * 100.0;
        let bias = bull_pct - bear_pct;
        let long_bias = bull_pct > bear_pct && bull_pct >= SNIPER_MIN;
        let short_bias = bear_pct > bull_pct && bear_pct >= SNIPER_MIN;
        let sniper_cross = self.sniper_cross.step(fast, slow);
        let long_trigger = matches!(sniper_cross, Signal::Long)
            && long_bias
            && close > vwap
            && macd > signal
            && rsi_five > 50.0;
        let short_trigger = matches!(sniper_cross, Signal::Short)
            && short_bias
            && close < vwap
            && macd < signal
            && rsi_five < 50.0;

        let memory_input = f64::from(direction) * (0.18 + momentum * 0.30);
        self.memory = (self.memory * MEMORY_DECAY + memory_input).clamp(-1.0, 1.0);
        let memory_bias = if self.memory > MEMORY_THRESHOLD {
            1
        } else if self.memory < -MEMORY_THRESHOLD {
            -1
        } else {
            0
        };
        let preferred = if direction != 0 { direction } else { memory_bias };
        let shared =
            confidence + momentum_score + one(volume_ok) + one(atr_ok) + one(adx > ADX_KEY);
        let long_score = 2 * one(preferred == 1)
            + one(direction == 1)
            + one(memory_bias == 1)
            + shared
            + one(trend_bull)
            + one(five_bull)
            + one(bull_candle)
            + 2 * one(bull_pct > bear_pct)
            + one(bias >= STRONG_BIAS)
            + one(close > vwap)
            + one(macd > signal)
            + one(rsi_five > 50.0);
        let short_score = 2 * one(preferred == -1)
            + one(direction == -1)
            + one(memory_bias == -1)
            + shared
            + one(trend_bear)
            + one(five_bear)
            + one(bear_candle)
            + 2 * one(bear_pct > bull_pct)
            + one(-bias >= STRONG_BIAS)
            + one(close < vwap)
            + one(macd < signal)
            + one(rsi_five < 50.0);

        // The signal interval is one bar on this chart, so it only waits out the entry bar itself.
        let new_check = self.last_signal_bar.is_none_or(|b| self.bar_index - b >= 1);
        let last = self.last_signal_price;
        let long_rearm =
            last.is_nan() || self.last_signal_long != Some(true) || close >= last + atr * REARM_ATR;
        let short_rearm = last.is_nan()
            || self.last_signal_long != Some(false)
            || close <= last - atr * REARM_ATR;
        let mut long_signal = (new_check
            && long_rearm
            && (long_bias || bias >= STRONG_BIAS)
            && long_score >= SCORE_MIN
            && long_score - short_score >= EDGE_MIN)
            || long_trigger;
        let mut short_signal = (new_check
            && short_rearm
            && (short_bias || -bias >= STRONG_BIAS)
            && short_score >= SCORE_MIN
            && short_score - long_score >= EDGE_MIN)
            || short_trigger;
        if long_signal && short_signal {
            if bull_pct >= bear_pct {
                short_signal = false;
            } else {
                long_signal = false;
            }
        }

        let held = strategy.position();
        let (in_long, flat) = (held > 0.0, held == 0.0);
        let crosses_under = matches!(self.exit_cross.step(close, fast), Signal::Short);
        let exhausted = self.rsi_before > RSI_OVERBOUGHT && rsi < RSI_OVERBOUGHT;
        let bearish_engulfing = close < open
            && self.close_before > self.open_before
            && close < self.open_before
            && open > self.close_before;
        let bars_in_trade = self.trade.entry_bar.map_or(0, |b| self.bar_index - b);
        let hold_passed = bars_in_trade >= HOLD_BARS;
        let early_exit = (crosses_under || exhausted || bearish_engulfing)
            && in_long
            && hold_passed
            && self.trade.hits[0];

        let enter = long_signal && flat;
        if enter {
            self.last_signal_bar = Some(self.bar_index);
            self.last_signal_long = Some(true);
            self.last_signal_price = close;
            self.trade = Trade { highest: high, entry_bar: Some(self.bar_index), ..Trade::NONE };
            self.trade.levels(close, atr * STOP_ATR);
            let sizing = strategy.sizing();
            let size = share_of_worth(&sizing, 100.0, sizing.slipped(Long, close));
            strategy.entry("long", Long).size(size);
        }
        if in_long && !self.trade.anchored {
            self.trade.levels(strategy.avg_cost(), atr * STOP_ATR);
            self.trade.anchored = true;
        }
        if in_long {
            let trade = &mut self.trade;
            trade.highest = if trade.highest.is_nan() { high } else { trade.highest.max(high) };
            if trade.highest - trade.entry >= atr * TRAIL_ACTIVATION_ATR {
                trade.trailing_active = true;
            }
            if trade.trailing_active {
                let next = trade.highest - atr * TRAIL_ATR;
                trade.trailing =
                    if trade.trailing.is_nan() { next } else { trade.trailing.max(next) };
            }
            let stop = if trade.trailing_active && !trade.trailing.is_nan() {
                trade.stop.max(trade.trailing)
            } else {
                trade.stop
            };
            strategy.exit("long stop").from("long").stop(stop);
            for (name, target) in TARGETS.into_iter().zip(self.trade.targets) {
                strategy.exit(name).from("long").size(held * TARGET_SHARE).take(target);
            }
        }
        if in_long && (early_exit || (short_signal && hold_passed)) {
            strategy.close("long");
        }
        // The rule clears its trade whenever it reads flat, the bar it enters on included.
        if flat && !self.trade.entry.is_nan() {
            self.trade = Trade::NONE;
        }
        if in_long {
            for (hit, target) in self.trade.hits.iter_mut().zip(self.trade.targets) {
                *hit = *hit || high >= target;
            }
        }

        self.closes.push_front(close);
        self.closes.truncate(HISTORY + 1);
        (self.open_before, self.close_before, self.rsi_before) = (open, close, rsi);
        self.bar_index += 1;
    }
}
