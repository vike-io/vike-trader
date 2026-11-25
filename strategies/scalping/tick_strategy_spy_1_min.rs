// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "TICK strategy, SPY 1 min" by PtGambler, https://www.tradingview.com/script/yiBwexdm/
// The original's notice: © platsn
// Changed: rewritten by hand in Rust for vike.

//! Trades a US index fund on 5-minute bars by NYSE's TICK, the RSI, the VIX and an EMA. TICK's 5-minute high, close and low
//! are read on each bar; its close is smoothed by an EMA(21) and by a Hull average (WMA(12) of twice WMA(67) less WMA(135):
//! the original's `135 / 2` read as 67 whole bars, its `round(sqrt(135))` 12) with an SMA(25) of the Hull line beside it.
//! Divergences are taken at TICK's pivots (10 bars left, 5 right, reported on the bar that confirms them, vike's
//! `ConfirmedPivot` of the close): a bullish one when a confirmed pivot low of TICK is higher than the pivot low before
//! while the price's low at it is lower, the bar before's pivot 25 to 300 bars back counted from the bar after it; a
//! bearish one the mirror at pivot highs. The RSI is the original's own over 70 bars of close-to-close moves (Wilder
//! averages; 100 with no losses, 0 with no gains), its signal a WMA(6) of it; the stochastic RSI's %K is SMA(3) of the
//! 14-bar stochastic of RSI(14); the trend an EMA(20) of the close.
//!
//! Entries, on bars opening 09:30 to 15:44 New York time, Monday to Friday, 350 shares each (the original's options
//! stand-in, the floor of 0.7% of the starting 5,000 times 10), up to three entries a side: long when the Hull line
//! crosses over its average, the RSI signal is over 50, the RSI under 65, the VIX over 20, TICK's EMA over 0 and the close
//! over the trend; while flat and within 12 bars of that signal, it is replaced by %K under 50 and rising. Flat only, also
//! long on a bullish divergence with the Hull line over its average, the RSI over its signal, the VIX over 20, TICK's EMA
//! over 0 and the close over the trend; and the chase: within 10 bars of the last trade's exit, the close above the close
//! of that exit's bar, the Hull line over its average and rising, the RSI signal over 50, the RSI under 65, the VIX over 20,
//! TICK's EMA rising, the close over the trend and %K under 80. Short is the mirror of each.
//!
//! Exits, issued only on bars opening 09:30 to 15:54 and more than 6 bars after the last entry filled: a stop and a
//! target for each side. The long stop is the lowest low of the last 5 bars less 9% of the daily ATR(4) as it stood at the
//! end of the bar the last entry filled on, or the bar before's low less that when lower; the long target the first open
//! trade's entry plus 17.5% of the daily ATR; a short's the mirror. A long is closed at the next open when the Hull line
//! is under its average and the RSI signal falls, on a bearish divergence, or when TICK's high is over 1,000; a short the
//! mirror (TICK's low under -1,000). On the bar opening 15:50 everything is closed on Monday, Wednesday and Friday, and half
//! the position on Tuesday and Thursday (the other half is held overnight, its stop and target resting). 0.0625
//! commission per share on every fill; starts with 5,000, language version 5 (no margin call).
//!
//! The daily ATR is the rule's call for the daily series without lookahead: the last day closed by this bar's close, over
//! the symbol's daily input, its history before the window included (`vike_tactics::higher::HigherTimeframeSourceCalendar`):
//! the 4-day Wilder average of the true range, the first day's range missing. The VIX on a bar is the close of the last of
//! its 1-minute bars inside the bar, kept from the last bar that had one; TICK's values are its 5-minute bar of the same time,
//! kept from the last one there was. Left out: the plots, background colours, labels and the TICK EMA(102) they draw, the
//! fast RSI and the exit it feeds (commented out in the original), the date range (2011 to 2050), reinvesting (off), the
//! indicator-only switch (off) and the options it leaves at their defaults (no pause at 10:00, the target on, the overnight
//! half on).
//!
//! **Data:** the reference ran on AMEX:SPY 5m with USI:TICK (NYSE's TICK) and VIX (20 trades, net -8303.75, 2025-09-08 to
//! 2026-09-22). Neither TICK nor VIX is held here, so the file is refused (`refuse`) and was never measured. TICK is the input
//! `user-tick:dukascopy:TICK:5m`, a user import lane's name (`LANE:VENUE:SYMBOL:INTERVAL`, decision 0271): no such series
//! exists, and a profile's `[inputs]` table remaps the name to a source once one is held. VIX is Dukascopy's VOLIDX.US/USD
//! CFD at one minute, not yet in the store.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use std::ops::Range;

use vike_indicators::indicators::{ConfirmedPivot, Ema, Rsi, Sma, Smma, Stochrsi, TrueRange, Wma};
use vike_indicators::strategy_kit;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, known, us_minute, weekday};
use vike_tactics::higher::{self, HigherTimeframeSourceCalendar};
use vike_tactics::{HigherTimeframePeriod, Rule, RuleDefaults, Tactics};

/// The VIX: Dukascopy's CFD of it (`VOL.IDX/USD`, named `VIX.CFD` by the operator's row), at one minute.
const VOLATILITY: &str = "dukascopy:VIX.CFD:1m";
/// NYSE's TICK at five minutes, a user import lane's name: no such series is held (the module doc).
const TICK: &str = "user-tick:dukascopy:TICK:5m";
const COMMISSION: f64 = 0.0625;
/// The floor of 0.7% of the starting 5,000, times 10 contracts.
const SIZE: f64 = 35.0 * 10.0;
const STOP_ATR: f64 = 9.0 * 0.01;
const TARGET_ATR: f64 = 17.5 * 0.01;
const VOLATILITY_FLOOR: f64 = 20.0;
const RSI_BAND: f64 = 15.0;
/// The offsets from 50 of %K for an entry and for a chase.
const STOCHASTIC_ENTRY: f64 = 0.0;
const STOCHASTIC_CHASE: f64 = 30.0;
const ENTRY_LOOKBACK: usize = 12;
const CHASE_LOOKBACK: usize = 10;
const MIN_HOLD: usize = 6;
const TICK_EXTREME: f64 = 1_000.0;
const PIVOT_LEFT: usize = 10;
const PIVOT_RIGHT: usize = 5;
const NEAREST: usize = 25;
const FARTHEST: usize = 300;
/// The bars a stop looks back over.
const STOP_LOOKBACK: usize = 5;
/// The bars of the chart kept, newest first: a stop's, a pivot's price and the close a chase compares with.
const KEPT: usize = CHASE_LOOKBACK;
/// The sessions, in New York minutes of the day: entries, exits, and the closing bar.
const ENTRY_SESSION: Range<i64> = 570..945;
const EXIT_SESSION: Range<i64> = 570..955;
const CLOSING: Range<i64> = 950..951;
/// Friday, counted from Monday 0; the days the closing bar closes half.
const FRIDAY: i64 = 4;
const HALF_DAYS: [i64; 2] = [1, 3];

/// A confirmed pivot of TICK: the bar that confirmed it, TICK there, and the price's low or high there.
#[derive(Clone, Copy)]
struct Pivot {
    bar: usize,
    tick: f64,
    price: f64,
}

/// Whether the bars from the bar after the pivot before up to this bar are in the original's range.
fn in_range(before: Pivot, now: usize) -> bool {
    (NEAREST..=FARTHEST).contains(&(now - before.bar - 1))
}

struct S {
    days: HigherTimeframeSourceCalendar,
    day_range: TrueRange,
    day_atr: Smma,
    /// The daily ATR of the last day closed.
    daily: f64,
    /// The VIX's last close.
    volatility: f64,
    tick_average: Ema,
    tick_average_before: f64,
    half: Wma,
    full: Wma,
    hull: Wma,
    hull_average: Sma,
    /// The Hull line and its average on the bar before.
    hull_before: (f64, f64),
    pivots: ConfirmedPivot,
    last_low: Option<Pivot>,
    last_high: Option<Pivot>,
    rsi: Rsi,
    rsi_signal: Wma,
    rsi_signal_before: f64,
    stochastic: Stochrsi,
    k: Sma,
    k_before: f64,
    trend: Ema,
    since_long: BarsSince,
    since_short: BarsSince,
    /// The last `KEPT` bars, newest first.
    bars: VecDeque<Bar>,
    /// This bar's count from the rule's first.
    count: usize,
    /// The newest open lot's fill time and the count of the bar it filled on.
    entry: Option<(i64, usize)>,
    /// The long and short stops as the bar the newest lot filled on left them.
    entry_stops: (f64, f64),
    /// The closed trades seen, and the count of the bar the last one closed on.
    closed: usize,
    exit_bar: Option<usize>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let wma = |length: f64| Wma::new(length).expect("a valid length");
    let sma = |length: f64| Sma::new(length).expect("a valid length").with_fresh_sum(true);
    Box::new(S {
        days: HigherTimeframeSourceCalendar::new(HigherTimeframePeriod::Day),
        day_range: TrueRange::default().with_first_bar_missing(true),
        day_atr: Smma::new(4.0).expect("a valid length").with_skip_nan(true).with_alpha_steps(true),
        daily: f64::NAN,
        volatility: f64::NAN,
        tick_average: Ema::new(21.0).expect("a valid length").with_skip_nan(true),
        tick_average_before: f64::NAN,
        half: wma(67.0),
        full: wma(135.0),
        hull: wma(12.0),
        hull_average: sma(25.0),
        hull_before: (f64::NAN, f64::NAN),
        pivots: ConfirmedPivot::new(PIVOT_LEFT as f64, PIVOT_RIGHT as f64).expect("valid spans"),
        last_low: None,
        last_high: None,
        rsi: Rsi::new(70.0).expect("a valid length").with_rs_form(true).with_zero_saturation(true),
        rsi_signal: wma(6.0),
        rsi_signal_before: f64::NAN,
        stochastic: Stochrsi::new(14.0, 14.0, 3.0).expect("valid lengths").with_rs_form(true),
        k: sma(3.0),
        k_before: f64::NAN,
        trend: Ema::new(20.0).expect("a valid length"),
        since_long: BarsSince::default(),
        since_short: BarsSince::default(),
        bars: VecDeque::with_capacity(KEPT + 1),
        count: 0,
        entry: None,
        entry_stops: (f64::NAN, f64::NAN),
        closed: 0,
        exit_bar: None,
    })
}

/// The owner's ruling of 2026-11-14: the rule needs NYSE's TICK, and no source of it is held (none is bought), so the file
/// is refused whatever its params. A backtest, a paper mount and a live mount each ask this before creating the strategy,
/// so the file never runs until a TICK source exists and this refusal is removed.
pub fn refuse(_params: &Params) -> Option<String> {
    Some("needs USI:TICK, no source".to_string())
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults {
            pyramiding: 3,
            fee: Commission::per_contract(COMMISSION),
            ..RuleDefaults::PLAIN
        }
    }

    fn inputs(&self) -> Vec<String> {
        vec![higher::DAY.to_string(), VOLATILITY.to_string(), TICK.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        for day in self.days.next(bar, strategy.input(higher::DAY)) {
            self.daily = self.day_atr.next_value(self.day_range.next(&day));
        }
        let stop_margin = self.daily * STOP_ATR;
        let target_margin = self.daily * TARGET_ATR;
        if let Some(last) = strategy.input(VOLATILITY).since(bar.timestamp).last() {
            self.volatility = last.close;
        }
        let volatile = self.volatility > VOLATILITY_FLOOR;
        let (tick_high, tick_close, tick_low) = strategy
            .input(TICK)
            .bar()
            .map_or((f64::NAN, f64::NAN, f64::NAN), |b| (b.high, b.close, b.low));
        let now = self.count;
        let close = bar.close;
        self.bars.push_front(bar.clone());
        self.bars.truncate(KEPT);

        let tick_average = self.tick_average.next_value(tick_close);
        let tick_rising = tick_average > self.tick_average_before;
        let tick_falling = tick_average < self.tick_average_before;
        self.tick_average_before = tick_average;
        let half = self.half.next_value(tick_close);
        let full = self.full.next_value(tick_close);
        let hull = self.hull.next_value(2.0 * half - full);
        let hull_average = self.hull_average.next_value(hull);
        let (hull_before, hull_average_before) = self.hull_before;
        self.hull_before = (hull, hull_average);
        let crossed_up = hull > hull_average && hull_before <= hull_average_before;
        let crossed_down = hull < hull_average && hull_before >= hull_average_before;

        let [pivot_high, pivot_low, _, _] = self.pivots.next_hl(tick_close, tick_close);
        let (pivot_bar_low, pivot_bar_high) =
            self.bars.get(PIVOT_RIGHT).map_or((f64::NAN, f64::NAN), |b| (b.low, b.high));
        let mut bullish = false;
        if let Some(tick) = known(pivot_low) {
            if let Some(before) = self.last_low
                && in_range(before, now)
            {
                bullish = pivot_bar_low < before.price && tick > before.tick;
            }
            self.last_low = Some(Pivot { bar: now, tick, price: pivot_bar_low });
        }
        let mut bearish = false;
        if let Some(tick) = known(pivot_high) {
            if let Some(before) = self.last_high
                && in_range(before, now)
            {
                bearish = pivot_bar_high > before.price && tick < before.tick;
            }
            self.last_high = Some(Pivot { bar: now, tick, price: pivot_bar_high });
        }

        let rsi = self.rsi.next_value(close);
        let rsi_signal = self.rsi_signal.next_value(rsi);
        let rsi_bull = rsi_signal > self.rsi_signal_before;
        let rsi_bear = rsi_signal < self.rsi_signal_before;
        self.rsi_signal_before = rsi_signal;
        let [stochastic, _] = self.stochastic.next_value(close);
        let k = self.k.next_value(stochastic);
        let k_before = self.k_before;
        self.k_before = k;
        let trend = self.trend.next_value(close);

        // The newest open lot and the bar it filled on; the bar the last closed trade closed on.
        let held = strategy.position();
        let open = held != 0.0 && !strategy.lots().is_empty();
        let mut entry_bar = false;
        let mut first_price = f64::NAN;
        if open {
            let lots = strategy.lots();
            first_price = lots[0].price;
            let filled = lots[lots.len() - 1].entry_timestamp;
            if self.entry.is_none_or(|(at, _)| at != filled) {
                self.entry = Some((filled, now));
            }
            entry_bar = self.entry.is_some_and(|(_, at)| at == now);
        } else {
            self.entry = None;
        }
        let since_entry = self.entry.map(|(_, at)| now - at);
        let closed = strategy.closed_trades().map_or(0, <[_]>::len);
        if closed > self.closed {
            self.closed = closed;
            self.exit_bar = Some(now);
        }
        let since_exit = self.exit_bar.filter(|_| !open).map(|at| now - at);

        let long_signal = crossed_up
            && rsi_signal > 50.0
            && rsi < 50.0 + RSI_BAND
            && volatile
            && tick_average > 0.0
            && close > trend;
        let short_signal = crossed_down
            && rsi_signal < 50.0
            && rsi > 50.0 - RSI_BAND
            && volatile
            && tick_average < 0.0
            && close < trend;
        let since_long = self.since_long.step(long_signal);
        let since_short = self.since_short.step(short_signal);
        let (mut long, mut short) = (long_signal, short_signal);
        if !open {
            if since_long.is_some_and(|n| n < ENTRY_LOOKBACK) {
                long = k < 50.0 + STOCHASTIC_ENTRY && k > k_before;
            }
            if since_short.is_some_and(|n| n < ENTRY_LOOKBACK) {
                short = k > 50.0 - STOCHASTIC_ENTRY && k < k_before;
            }
        }
        let long_divergence = bullish
            && hull > hull_average
            && rsi > rsi_signal
            && volatile
            && tick_average > 0.0
            && close > trend;
        let short_divergence = bearish
            && hull < hull_average
            && rsi < rsi_signal
            && volatile
            && tick_average < 0.0
            && close < trend;
        let exit_close = since_exit
            .filter(|&n| n < CHASE_LOOKBACK)
            .and_then(|n| self.bars.get(n))
            .map_or(f64::NAN, |b| b.close);
        let long_chase = close > exit_close
            && hull > hull_average
            && hull > hull_before
            && rsi_signal > 50.0
            && rsi < 50.0 + RSI_BAND
            && volatile
            && tick_rising
            && close > trend
            && k < 50.0 + STOCHASTIC_CHASE;
        let short_chase = close < exit_close
            && hull < hull_average
            && hull < hull_before
            && rsi_signal < 50.0
            && rsi > 50.0 - RSI_BAND
            && volatile
            && tick_falling
            && close < trend
            && k > 50.0 - STOCHASTIC_CHASE;

        let minute = us_minute(bar.timestamp, -5);
        let day = weekday(bar.timestamp);
        let weekday_in = |session: &Range<i64>| day <= FRIDAY && session.contains(&minute);
        if weekday_in(&ENTRY_SESSION) {
            if long {
                strategy.entry("Long", Long).size(SIZE);
            }
            if (long_divergence || long_chase) && !open {
                strategy.entry("Long", Long).size(SIZE);
            }
            if short {
                strategy.entry("Short", Short).size(SIZE);
            }
            if (short_divergence || short_chase) && !open {
                strategy.entry("Short", Short).size(SIZE);
            }
        }

        let recent = self.bars.iter().take(STOP_LOOKBACK);
        let (lowest, highest) = if self.bars.len() >= STOP_LOOKBACK {
            (
                recent.clone().fold(f64::INFINITY, |a, b| a.min(b.low)),
                recent.fold(f64::NEG_INFINITY, |a, b| a.max(b.high)),
            )
        } else {
            (f64::NAN, f64::NAN)
        };
        let mut long_stop = lowest - stop_margin;
        let mut short_stop = highest + stop_margin;
        if open {
            let (low_before, high_before) =
                self.bars.get(1).map_or((f64::NAN, f64::NAN), |b| (b.low, b.high));
            let (long_entry, short_entry) =
                if entry_bar { (long_stop, short_stop) } else { self.entry_stops };
            long_stop = strategy_kit::min(long_entry, low_before - stop_margin);
            short_stop = strategy_kit::max(short_entry, high_before + stop_margin);
        }
        if entry_bar {
            self.entry_stops = (long_stop, short_stop);
        }
        let (long_target, short_target) = if open {
            (first_price + target_margin, first_price - target_margin)
        } else {
            (close + target_margin, close - target_margin)
        };

        if since_entry.is_some_and(|n| n > MIN_HOLD) && weekday_in(&EXIT_SESSION) {
            let exit = strategy.exit("Exit Long").from("Long");
            if let Some(stop) = known(long_stop) {
                exit.stop(stop);
            }
            if let Some(target) = known(long_target) {
                exit.take(target);
            }
            let exit = strategy.exit("Exit Short").from("Short");
            if let Some(stop) = known(short_stop) {
                exit.stop(stop);
            }
            if let Some(target) = known(short_target) {
                exit.take(target);
            }
            if (hull < hull_average && rsi_bear) || bearish || tick_high > TICK_EXTREME {
                strategy.close("Long");
            }
            if (hull > hull_average && rsi_bull) || bullish || tick_low < -TICK_EXTREME {
                strategy.close("Short");
            }
        }
        if weekday_in(&CLOSING) {
            if !HALF_DAYS.contains(&day) {
                strategy.close_all();
            } else if held != 0.0 {
                let half = (held.abs() * 0.5).floor();
                strategy.close("Long").size(half);
                strategy.close("Short").size(half);
            }
        }
        self.count += 1;
    }
}
