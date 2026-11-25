// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Heikin/Kaufman Non-Repaint" by TradersVenue, https://www.tradingview.com/script/KPDPfaQK/
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-06-13, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Two lines of Heikin-Ashi candles, crossing. The slow line is an EMA(21) of a Kaufman adaptive average of the chart's own
//! Heikin-Ashi candles as it stood one bar before: the average of their hlc3 (high, low and close over three), its
//! efficiency the move of the hlc3 over 10 bars against the sum of its 10 one-bar moves, its smoothing the square of that
//! efficiency scaled between 2/(5+1) and 2/(20+1), started from 0. The fast line is the hlc3 of a 25-minute Heikin-Ashi
//! candle, the one before the last 25-minute bar the chart bar's close ended. Long 1 when the fast line crosses over the
//! slow, short 1 when it crosses under, each reversing the other. Commission 0.02% of each fill; starts with 100,000, no
//! margin stated. The two plots and the background colour are left out.
//!
//! The 25-minute frame is built from an input of the run's own symbol, its 5-minute bars (decision 0271), from the rule's
//! first bar on: each UTC day's from its 00:00 in steps of 25 minutes, the day's last one 15 minutes long, their
//! Heikin-Ashi candles from the first of them, every price on the tick (as the chart's own candles).
//!
//! The rule runs again after each of its fills inside a bar (`calc_on_order_fills`), on that bar's final values as its close
//! will read them (`Rule::after_fill`), so a cross the bar's close makes is traded at that fill's price, inside the bar. The
//! slow line stands from the bar before; the 25-minute candle is the one the bar's close reads.
//!
//! **It looks into the future.** On one chart bar in five (the bars whose close ends a 25-minute bar: 02:00, 04:30, ...,
//! 23:30) that candle ends up to 5 minutes after the bar's open, and the pass after an open fill reads it at its final
//! values, which no live run can know at the fill: the 5-minute input's whole series serves it
//! (`vike_tactics::Tactics::input_whole`, read once). So it runs in a backtest only, and its backtest is better than
//! reality. With the input only as of the bar's open, the pass sees no cross on those bars: 818 trades and -19238.09
//! against the reference's 818 and -21772.41.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = "looks into the future: reads, after a fill inside a bar, the 25-minute candle in progress at its final values; a backtest is better than reality"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;

use vike_indicators::indicators::{Ema, HeikinAshi};
use vike_indicators::strategy_kit::HeikinAshiCandles;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{back, nan_to_zero, push};
use vike_tactics::higher::HigherTimeframeSourceUnseen;
use vike_tactics::rolling::RollingSum;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const FIVE_MINUTES: &str = "5m";
const FIVE_MINUTES_MS: i64 = 300_000;
const FRAME_MS: i64 = 1_500_000;
const DAY_MS: i64 = 86_400_000;
/// The Kaufman average's efficiency window, and its fastest and slowest smoothing lengths.
const LENGTH: usize = 10;
const FAST_END: f64 = 5.0;
const SLOW_END: f64 = 20.0;
const SLOW: f64 = 21.0;

/// The 25-minute period `ts` lies in, its start and end: from the UTC day's 00:00, the day's last one cut at midnight.
fn period(ts: i64) -> (i64, i64) {
    let day = ts - ts.rem_euclid(DAY_MS);
    let start = ts - (ts - day) % FRAME_MS;
    (start, (start + FRAME_MS).min(day + DAY_MS))
}

/// The Kaufman average of the candles' hlc3, from 0 (NaN until its smoothing exists).
struct Kaufman {
    /// The hlc3 of the last `LENGTH + 1` candles, newest first.
    sources: VecDeque<f64>,
    noise: RollingSum,
    value: f64,
}

impl Kaufman {
    fn next(&mut self, source: f64) -> f64 {
        let noise = self.noise.next((source - back(&self.sources, 0)).abs());
        push(&mut self.sources, source, LENGTH + 1);
        let signal = (source - back(&self.sources, LENGTH)).abs();
        let efficiency = if noise != 0.0 { signal / noise } else { 0.0 };
        let (fastest, slowest) = (2.0 / (FAST_END + 1.0), 2.0 / (SLOW_END + 1.0));
        let scaled = efficiency * (fastest - slowest) + slowest;
        let smoothing = scaled * scaled;
        let before = nan_to_zero(self.value);
        self.value = before + smoothing * (source - before);
        self.value
    }
}

/// The 25-minute Heikin-Ashi candles of the 5-minute input, each candle's hlc3 kept with its bar's end.
#[derive(Clone)]
struct Frame {
    unseen: HigherTimeframeSourceUnseen,
    /// The bar in progress and its period's end.
    building: Option<(i64, Bar)>,
    candles: Option<HeikinAshi>,
    /// The last closed candles' ends and hlc3, newest first.
    closed: VecDeque<(i64, f64)>,
    /// The open of the last 5-minute bar taken.
    last: Option<i64>,
}

impl Frame {
    /// Takes the 5-minute bars closed since the last call, those opening before `from` left out.
    fn feed(&mut self, strategy: &Tactics<'_>, from: i64) {
        let fives = self.unseen.next(strategy.input(FIVE_MINUTES));
        for five in &fives[fives.partition_point(|b| b.timestamp < from)..] {
            self.add(five, strategy.tick());
        }
    }

    fn add(&mut self, bar: &Bar, tick: Option<f64>) {
        self.last = Some(bar.timestamp);
        let (start, end) = period(bar.timestamp);
        if self.building.as_ref().is_some_and(|(_, b)| b.timestamp != start) {
            let (end, done) = self.building.take().expect("checked");
            self.close(end, &done, tick);
        }
        match &mut self.building {
            Some((_, b)) => {
                b.high = b.high.max(bar.high);
                b.low = b.low.min(bar.low);
                b.close = bar.close;
                b.volume += bar.volume;
            }
            None => self.building = Some((end, Bar { timestamp: start, ..bar.clone() })),
        }
        if bar.timestamp + FIVE_MINUTES_MS >= end {
            let (end, done) = self.building.take().expect("just added");
            self.close(end, &done, tick);
        }
    }

    fn close(&mut self, end: i64, bar: &Bar, tick: Option<f64>) {
        let candles = self.candles.get_or_insert_with(|| {
            let plain = HeikinAshi::default();
            match tick {
                Some(tick) => plain.with_tick(tick).expect("a positive tick"),
                None => plain,
            }
        });
        let [_, high, low, close] = candles.next(bar);
        self.closed.push_front((end, (high + low + close) / 3.0));
        self.closed.truncate(3);
    }

    /// The hlc3 a chart bar closing at `close` reads: of the candle before the last 25-minute bar ended by then, NaN while
    /// that candle has not ended.
    fn fast(&self, close: i64) -> f64 {
        let (start, end) = period(close - 1);
        let last_start = if end <= close { start } else { period(start - 1).0 };
        self.closed.iter().find(|c| c.0 == last_start).map_or(f64::NAN, |c| c.1)
    }
}

struct S {
    /// The rule's first bar's open: no 5-minute bar before it is fed.
    from: Option<i64>,
    /// The chart bar's length: the least time between two chart bars seen.
    chart_ms: Option<i64>,
    bar_before: Option<i64>,
    frame: Frame,
    candles: HeikinAshiCandles,
    kaufman: Kaufman,
    /// The Kaufman average as it stood on the bar before.
    kaufman_before: f64,
    slow: Ema,
    /// The two lines on the bar before.
    fast_before: f64,
    slow_before: f64,
    /// The 5-minute input's whole series, the bars still to come included, read on the rule's first bar.
    whole: Vec<Bar>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        from: None,
        chart_ms: None,
        bar_before: None,
        frame: Frame {
            unseen: HigherTimeframeSourceUnseen::new(),
            building: None,
            candles: None,
            closed: VecDeque::new(),
            last: None,
        },
        whole: Vec::new(),
        candles: HeikinAshiCandles::default(),
        kaufman: Kaufman {
            sources: VecDeque::new(),
            noise: RollingSum::new(LENGTH),
            value: f64::NAN,
        },
        kaufman_before: f64::NAN,
        slow: Ema::new(SLOW).expect("a valid length").with_skip_nan(true),
        fast_before: f64::NAN,
        slow_before: f64::NAN,
    })
}

impl S {
    /// The entry the two lines' cross from the bar before to this one makes, if any.
    fn trade(&self, strategy: &mut Tactics<'_>, fast: f64, slow: f64) {
        if fast > slow && self.fast_before <= self.slow_before {
            strategy.entry("Buy", Long).size(1.0);
        }
        if fast < slow && self.fast_before >= self.slow_before {
            strategy.entry("Sell", Short).size(1.0);
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.02), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIVE_MINUTES.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if self.from.is_none() {
            let whole = strategy.input_whole(FIVE_MINUTES).unwrap_or_else(|| {
                panic!(
                    "this rule reads the whole series of its input {FIVE_MINUTES:?}, which a backtest serves \
                     a file whose header says runs_on = \"backtest\" alone"
                )
            });
            self.whole = whole.to_vec();
        }
        let from = *self.from.get_or_insert(bar.timestamp);
        if let Some(before) = self.bar_before {
            let gap = bar.timestamp - before;
            self.chart_ms = Some(self.chart_ms.map_or(gap, |ms| ms.min(gap)));
        }
        self.bar_before = Some(bar.timestamp);
        self.frame.feed(strategy, from);
        let fast = self.chart_ms.map_or(f64::NAN, |ms| self.frame.fast(bar.timestamp + ms));

        let candle = self.candles.next(bar, strategy.tick());
        let average = self.kaufman.next((candle.high + candle.low + candle.close) / 3.0);
        let slow = self.slow.next_value(self.kaufman_before);
        self.kaufman_before = average;
        self.trade(strategy, fast, slow);
        (self.fast_before, self.slow_before) = (fast, slow);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let (Some(from), Some(ms)) = (self.from, self.chart_ms) else { return };
        self.frame.feed(strategy, from);
        // The bar's final candle: the 5-minute bars its close will have closed, from the whole series, on a copy of the frame.
        let close = bar.timestamp + ms;
        let mut ahead = self.frame.clone();
        let next = from.max(ahead.last.map_or(i64::MIN, |last| last + 1));
        let tick = strategy.tick();
        for five in self.whole[self.whole.partition_point(|b| b.timestamp < next)..]
            .iter()
            .take_while(|b| b.timestamp + FIVE_MINUTES_MS <= close)
        {
            ahead.add(five, tick);
        }
        let fast = ahead.fast(close);
        let slow = self.slow.clone().next_value(self.kaufman_before);
        self.trade(strategy, fast, slow);
    }
}
