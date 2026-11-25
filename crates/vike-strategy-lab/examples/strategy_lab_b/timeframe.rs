//! The reference's call for a higher timeframe of the chart's own symbol: the higher bars, UTC-aligned, and the bar a rule's
//! chart bar sees. The frame is the order language's `vike_tactics::HigherTimeframe` (`crates/vike-tactics/src/frame.rs`), whose options are the
//! rules below; `Higher` binds it to the venue's files and to this harness's controls, which flip those options.
//! `Higher::next` takes each chart bar and hands back the higher bars that closed by its close; a rule computes
//! its expression on those bars, so a value it keeps is the expression's value at the last higher bar closed by this close. On
//! the chart bar that ends a period that is the period's own bar; on every other, the one before. `Higher::looking_ahead` is
//! the call with lookahead on. `HeikinAshi` is the reference's Heikin-Ashi ticker of the chart's own bars.
//!
//! A rule the strategy gallery took reads the same readings through its inputs (`vike_tactics::higher`: a day is its
//! daily input, `vike_tactics::Tactics::input`, an intraday frame is built from its 30-minute input, its candles on the tick
//! it reads), which no switch here reaches. So a control runs such a rule on THESE frames (`LabFrames`, installed through
//! `vike_tactics::higher::with_higher_timeframe_source`, and `vike_indicators::strategy_kit::with_heikin_ashi_candles_source` for its candles): every frame and
//! candle series it makes is then this module's, reading the
//! controls' switches as a rule kept here does, and the control applies to it exactly as it did before the rule moved. Its own
//! run reads its inputs; `main.rs`'s `runs_of` checks that this module's frames, every switch off, give it the same numbers.
//!
//! # What settled it, measured (the alternatives against the reference)
//!
//! 1. **The last higher bar closed by this close, never the one in progress.** Every row that reads a higher value without
//!    lookahead matches with it and misses with the bar in progress at its final values: 296 trades against 214
//!    (`previous_day_high_and_low_breakout_strategy`), 728 against 404 (`hull_moving_average_vs_candle`), 3 against 2
//!    (`rsi_crossover_strategy_with_compounding_monthly`), 31 against 27 (`so_trade_top_me_dia_de_8_augusto_backes`), 238
//!    against 190 (`ichimoku_daily_candle_x_hull_ma_x_macd`), net 6699.66 against -5250.47
//!    (`rsi_macd_multi_timeframe_strategy`), 2609 against 2448 (`camarilla_pivot_points_v2_backtest`) and 401 against 407
//!    (`combo_backtest_123_reversal_smart_money_index_sm`). For a read of the open or of `[1]` (the first, second, fifth and
//!    last of these) the bar in progress as of this close gives the same values. The control `higher bar in progress` holds it.
//! 2. **A day, a week and a month reach back before the chart's window, to the venue's own history; a frame of minutes or
//!    hours starts at the window.** The chart sees no bar before its window (`main.rs`'s rule 1). The daily context does: the
//!    daily bars from 2017-08-17, weeks and months built from its days. Built from the window's bars alone: net -16730.84
//!    against -13897.51 (`previous_day_high_and_low_breakout_strategy`), -8605.75 against -8570.58 (`ichimoku_daily_...`),
//!    2446 trades against 2448 (`camarilla_...`), net -66777.07 against -65462.57 (`combo_...`); even from the 30-minute bars
//!    the lab reads (from 2024-12-01) the monthly row has no trade (2) and the weekly one 20 (27). The control `higher bars from
//!    the window only` holds it. An intraday frame does not: its bars are the window's own, from the first chart bar. Fed the
//!    venue's history before the window (its 4-hour bars from 2017, its 30-minute bars from 2024-12-01 for 1, 2 and 3 hours):
//!    240 trades against 225 (`mtf_screener_ma_cross_system`, a Hull average of 200 4-hour closes), 1871 against 1870
//!    (`ajay_r5_41`, 240 minutes with lookahead on), 245 against 247 (`mtf_trailing_sl_strategy_quantnomad`, 2, 3 and 4 hours
//!    with lookahead on and `[1]`). The control `intraday higher bars and Heikin-Ashi candles from the venue's history` holds
//!    it. Inside the 30-minute bars' span, a day and a 4-hour bar built from them equal the venue's own (699 days and
//!    4199 4-hour bars, every price), so the two sources agree within the window.
//! 3. **An expression in the call is computed on the higher bars.** `high[1]` is the higher bar before, `ta.rsi(close, 14)`
//!    an RSI of the higher closes, and a call whose expression reads another call's result on the same timeframe
//!    (`ta.sma(rsiMonthly, 14)`) is that indicator of the other call's higher-bar values: all 8 rows above match so.
//! 4. **With lookahead on, the call sees the higher bar in progress at its final values** (`Higher::looking_ahead`: each higher
//!    bar on the first chart bar of its period), and `[1]` there is the bar before it on every chart bar of the period, its
//!    last one included: `strategy_template_v2` (a weekly Donchian of `[1]`), `mtf_trailing_sl_strategy_quantnomad`,
//!    `ajay_r5_41` and `optimized_heikin_ashi_strategy_with_buy_sell_opt` match so. Rule 2 holds for it as for the call
//!    without: a week reaches back, 240 minutes starts at the window.
//! 5. **The Heikin-Ashi ticker's prices are on the symbol's tick, and its candles start at the window** (`HeikinAshi`): the close
//!    `(open + high + low + close) / 4` rounded, the open the rounded mean of the last candle's rounded open and close (the
//!    first's, the bar's `(open + close) / 2`), the high and low the bar's widened to take both in. `random`: unrounded 1163
//!    trades and -911.971696 against the reference's 1162 and -911.268127, which the rule gives to the last decimal; the
//!    control `Heikin-Ashi candles off the tick` holds it. Where they start no row separates: fed the 30-minute history from
//!    2024-12-01, the open's recursion (half its distance from its start gone each bar) meets the window's on the tick within
//!    the first bars, and every Heikin-Ashi row gives the same numbers.
//!
//! A call on the chart's own timeframe (`""`) is the expression on the chart: `rsi_on_vwap_upgraded_strategy` matches so.
//!
//! The venue's bars come through the datahub (`main.rs`'s `read_bars`), each series of the run's market once (`venue_bars`).
//! shortcut: only the timeframes the market's daily, 4-hour and 30-minute series build (a day, a week, a month, a multiple of
//! 30 minutes), and the first bar fed must start a bar of the series the frame is built from; another needs its own series here.

use std::collections::BTreeMap;
use std::sync::Mutex;

use vike_marketdata::Bar;
use vike_tactics::{HigherTimeframe, HigherTimeframeHistoryBefore, HigherTimeframePeriod};

use crate::markets::{self, Market};
use crate::{BAR_MS, Switch};

const DAY_MS: i64 = 86_400_000;
const FOUR_HOURS_MS: i64 = 14_400_000;

// The controls' switches are per thread (`Switch`): `main.rs` runs rows on several threads.
thread_local! {
    /// The control `higher bar in progress` in `main.rs` only: `HigherTimeframe::looking_ahead` on for a call without
    /// lookahead, each higher bar handed out at its period's first chart bar, at its final values, which the reference must
    /// refute.
    pub static IN_PROGRESS: Switch = const { Switch::off() };
    /// The control `higher bars from the window only` in `main.rs` only: no venue history for a day, a week or a month
    /// (`HigherTimeframeHistoryBefore::calendar` off).
    pub static WINDOW_ONLY: Switch = const { Switch::off() };
    /// The control `intraday higher bars and Heikin-Ashi candles from the venue's history` in `main.rs` only
    /// (`HigherTimeframeHistoryBefore::intraday` on).
    pub static INTRADAY_HISTORY: Switch = const { Switch::off() };
    /// The control `Heikin-Ashi candles off the tick` in `main.rs` only.
    pub static UNROUNDED_CANDLES: Switch = const { Switch::off() };
}

/// Rule 2: a day, a week and a month see the venue's history before the window; minutes start at the window. Each kind reads
/// its own control alone: `runs_of` runs a control only for a row whose run read it.
fn history(every: HigherTimeframePeriod) -> HigherTimeframeHistoryBefore {
    match every {
        HigherTimeframePeriod::Minutes(_) => HigherTimeframeHistoryBefore {
            calendar: false,
            intraday: INTRADAY_HISTORY.with(Switch::get),
        },
        HigherTimeframePeriod::Day
        | HigherTimeframePeriod::Week
        | HigherTimeframePeriod::Month
        | HigherTimeframePeriod::Quarter => HigherTimeframeHistoryBefore {
            calendar: !WINDOW_ONLY.with(Switch::get),
            intraday: false,
        },
    }
}

/// The venue's own bars a frame of `every` is built from, and their length.
fn venue_of(every: HigherTimeframePeriod) -> (&'static [Bar], i64) {
    match every {
        HigherTimeframePeriod::Day
        | HigherTimeframePeriod::Week
        | HigherTimeframePeriod::Month
        | HigherTimeframePeriod::Quarter => (days(), DAY_MS),
        HigherTimeframePeriod::Minutes(m) if m * 60_000 % FOUR_HOURS_MS == 0 => {
            (venue("4h"), FOUR_HOURS_MS)
        }
        HigherTimeframePeriod::Minutes(m) if m * 60_000 % BAR_MS == 0 => half_hours(),
        HigherTimeframePeriod::Minutes(m) => panic!("no venue bars for {m} minutes"),
    }
}

/// `market`'s own bars at `interval` (`crate::read_bars`), each series read once per process.
pub fn venue_bars(market: Market, interval: &str) -> &'static [Bar] {
    type Read = BTreeMap<(&'static str, &'static str, String), &'static [Bar]>;
    static READ: Mutex<Read> = Mutex::new(BTreeMap::new());
    let mut read = vike_utility::mutex::lock(&READ);
    read.entry((market.venue, market.symbol, interval.to_string()))
        .or_insert_with(|| Box::leak(crate::read_bars(market, interval).into_boxed_slice()))
}

/// The ms to add to `timestamp` so that the FX day's start, 17:00 New York (the US daylight rule, `vike_tactics::helpers::us_minute`),
/// falls on a UTC midnight: 2 hours in winter, 3 in summer.
fn from_fx_day_start(timestamp: i64) -> i64 {
    let utc_minute = timestamp.rem_euclid(DAY_MS) / 60_000;
    let new_york =
        (vike_tactics::helpers::us_minute(timestamp, -5) - utc_minute).rem_euclid(1440) - 1440;
    (new_york + 7 * 60) * 60_000
}

/// Bars of `bar_ms` built from the `finer` bars that tile them, as the venue would serve them (`markets::Market::built_from`): a
/// FX market's bars are aligned on its day's start, 17:00 New York, so a day runs from it and a 4-hour bar from it or 4, 8, ...
/// hours later (a 30-minute bar is aligned on the epoch either way). Each opens at its first finer bar's open, closes at its
/// last one's close, its high and low the extremes between; a span with no finer bar has none
/// (`vike_marketdata::consolidator::resample`, on the times moved by `from_fx_day_start`, which never changes inside a bar: the
/// clocks move on a Sunday, when the market is shut).
pub fn built(finer: &[Bar], bar_ms: i64) -> Vec<Bar> {
    let moved: Vec<Bar> = finer
        .iter()
        .map(|b| Bar { timestamp: b.timestamp + from_fx_day_start(b.timestamp), ..b.clone() })
        .collect();
    vike_marketdata::consolidator::resample(&moved, bar_ms)
        .into_iter()
        .map(|bar| {
            let first = &finer[moved.partition_point(|b| b.timestamp < bar.timestamp)];
            Bar { timestamp: bar.timestamp - from_fx_day_start(first.timestamp), ..bar }
        })
        .collect()
}

/// The run's market's bars at `interval`.
fn venue(interval: &str) -> &'static [Bar] {
    venue_bars(markets::current().market, interval)
}

/// The venue's daily bars, from the symbol's first (BTCUSDT's 2017-08-17).
pub fn days() -> &'static [Bar] {
    venue("1d")
}

/// The venue's 30-minute bars (from 2024-12-01) and their length.
pub fn half_hours() -> (&'static [Bar], i64) {
    (venue("30m"), BAR_MS)
}

/// The chart's own venue bars, which a frame that starts at the window and looks ahead is built from.
fn chart_bars() -> &'static [Bar] {
    venue(markets::current().interval)
}

/// The higher bars of one timeframe, built as the chart's bars come: `vike_simulator::HigherTimeframe` over the venue's files.
pub struct Higher {
    every: HigherTimeframePeriod,
    /// Read with lookahead on (or under `IN_PROGRESS`): each higher bar on the first chart bar of its period.
    ahead: bool,
    /// The frame, made on the first chart bar fed, which reads rule 2's control.
    frame: Option<HigherTimeframe>,
}

impl Higher {
    pub fn new(every: HigherTimeframePeriod) -> Self {
        Higher { every, ahead: IN_PROGRESS.with(Switch::get), frame: None }
    }

    /// A higher timeframe read with lookahead on: each higher bar is handed out on the first chart bar of its period, at its final
    /// values, which is what the control `higher bar in progress` does to the rows that read without lookahead. It reads no
    /// `IN_PROGRESS`, which cannot change it.
    pub fn looking_ahead(every: HigherTimeframePeriod) -> Self {
        Higher { every, ahead: true, frame: None }
    }

    /// Feeds one chart bar and returns the higher bars closed by its close, oldest first: on the first bar fed of a frame that
    /// reaches back, every one of the venue's history before it as well. The bar in progress then holds what of its period came
    /// before that bar. The frame is built from the venue's bars when it reaches back or reads no bar ahead, from the chart's own
    /// when it looks ahead from the window.
    pub fn next(&mut self, bar: &Bar) -> Vec<Bar> {
        if let Some(frame) = &mut self.frame {
            return frame.next(bar, None);
        }
        let chart_ms = markets::current().bar_ms;
        let frame = HigherTimeframe::new(self.every, chart_ms)
            .looking_ahead(self.ahead)
            .with_history(history(self.every));
        let base = if self.ahead && !frame.reaches_back() {
            (chart_bars(), chart_ms)
        } else {
            venue_of(self.every)
        };
        self.frame.insert(frame).next(bar, Some(base))
    }
}

/// The reference's Heikin-Ashi ticker of the chart's own bars, read on the chart's timeframe (rule 5): candles from the window's
/// first bar, every price on the symbol's tick, the open's recursion on the rounded prices (vike's `HeikinAshi::with_tick`).
/// Orders still fill at the real prices.
#[derive(Default)]
pub struct HeikinAshi {
    /// Whether the first chart bar has come.
    started: bool,
    /// The candles, made on the first one (the tick is the market's).
    candles: Option<vike_indicators::indicators::HeikinAshi>,
}

impl HeikinAshi {
    /// This chart bar's candle, its time and volume the bar's.
    pub fn next(&mut self, bar: &Bar) -> Bar {
        if !std::mem::replace(&mut self.started, true) && INTRADAY_HISTORY.with(Switch::get) {
            assert_eq!(markets::current().bar_ms, BAR_MS, "the history is the 30-minute file");
            let (bars, _) = half_hours();
            for b in bars.iter().take_while(|b| b.timestamp < bar.timestamp) {
                self.candle(b);
            }
        }
        self.candle(bar)
    }

    fn candle(&mut self, bar: &Bar) -> Bar {
        let candles = self.candles.get_or_insert_with(|| {
            let plain = vike_indicators::indicators::HeikinAshi::default();
            if UNROUNDED_CANDLES.with(Switch::get) {
                plain
            } else {
                plain.with_tick(markets::current().market.tick).expect("a positive tick")
            }
        });
        let [open, high, low, close] = candles.next(bar);
        Bar { open, high, low, close, ..bar.clone() }
    }
}

/// The gallery rows whose original reads its higher timeframe with lookahead on and `[1]` of it, the period before, as the
/// rules kept here read `Higher::looking_ahead`: `LabFrames` builds their frames so. Every other row's frame is `Higher::new`.
pub const AHEAD_ROWS: &[&str] = &[
    "btc_bot",
    "emperorbtc_s_vwap_strategy",
    "mtf_trailing_sl_strategy_quantnomad",
    "strategy_template_v2",
    "volume_vigilante_long_only_opening_range_breakou",
];

/// This module's frames and candles as a gallery rule's (`vike_tactics::higher::HigherTimeframeSource` and
/// `vike_indicators::strategy_kit::HeikinAshiCandlesSource`, the module doc): a day, a
/// week, a month or a frame of minutes is `Higher`'s (`Higher::looking_ahead` when `ahead`, a row of `AHEAD_ROWS`), the
/// candles `HeikinAshi`, each reading this harness's switches and market as a rule kept here does.
pub struct LabFrames {
    pub ahead: bool,
    /// Set when the rule made a frame or a candle series: a row that made none reads nothing a control here flips.
    pub made: std::cell::Cell<bool>,
}

impl LabFrames {
    fn higher(&self, every: HigherTimeframePeriod) -> Higher {
        self.made.set(true);
        if self.ahead { Higher::looking_ahead(every) } else { Higher::new(every) }
    }
}

impl vike_tactics::higher::HigherTimeframeSource for LabFrames {
    fn calendar(&self, every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
        let mut frame = self.higher(every);
        Box::new(move |bar| frame.next(bar))
    }

    fn intraday(&self, minutes: i64) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
        let mut frame = self.higher(HigherTimeframePeriod::Minutes(minutes));
        Box::new(move |bar| frame.next(bar))
    }

    /// A frame the rule reads with lookahead on (`vike_tactics::higher::HigherTimeframeSourceAhead`, the files of
    /// `strategies/looks_into_the_future/`) is `Higher::looking_ahead` over the venue's files, whatever `ahead` says: the
    /// reading those rows were measured on when the lab kept them, under rule 2's controls.
    fn ahead(&self, every: HigherTimeframePeriod) -> Box<dyn FnMut(&Bar) -> Vec<Bar> + Send> {
        self.made.set(true);
        let mut frame = Higher::looking_ahead(every);
        Box::new(move |bar| frame.next(bar))
    }
}

impl vike_indicators::strategy_kit::HeikinAshiCandlesSource for LabFrames {
    fn candles(&self) -> Box<dyn FnMut(&Bar) -> Bar + Send> {
        self.made.set(true);
        let mut candles = HeikinAshi::default();
        Box::new(move |bar| candles.next(bar))
    }
}

pub fn self_check() {
    let jan_2025 = 1_735_689_600_000;
    // Rule 5: on the tick, the open's recursion on the rounded prices.
    let bar = |timestamp: i64, open: f64, high: f64, low: f64, close: f64| Bar {
        timestamp,
        open,
        high,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    let mut candles = HeikinAshi { started: true, candles: None };
    let first = candles.next(&bar(jan_2025, 10.0, 10.013, 9.99, 10.004));
    assert_eq!((first.open, first.close, first.high, first.low), (10.0, 10.0, 10.013, 9.99));
    let second = candles.next(&bar(jan_2025 + BAR_MS, 10.0, 10.03, 10.0, 10.02));
    assert_eq!((second.open, second.close), (10.0, 10.01));
    // A FX day starts at 17:00 New York: 22:00 UTC in January, 21:00 in July; a 4-hour bar at it or 4, 8, ... hours later.
    let starts = |finer: &[(i64, f64)], bar_ms: i64| -> Vec<(i64, f64, f64)> {
        let finer: Vec<Bar> =
            finer.iter().map(|&(timestamp, c)| bar(timestamp, c, c, c, c)).collect();
        built(&finer, bar_ms).iter().map(|b| (b.timestamp, b.open, b.close)).collect()
    };
    let (jan_2, jul_2) = (1_704_153_600_000, 1_719_878_400_000);
    let hour = 3_600_000;
    let january = [
        (jan_2 + 21 * hour + 1_800_000, 1.0),
        (jan_2 + 21 * hour + 2_700_000, 2.0),
        (jan_2 + 22 * hour, 3.0),
    ];
    assert_eq!(
        starts(&january, DAY_MS),
        [(jan_2 - 2 * hour, 1.0, 2.0), (jan_2 + 22 * hour, 3.0, 3.0)]
    );
    let july = [(jul_2 + 20 * hour + 2_700_000, 1.0), (jul_2 + 21 * hour, 2.0)];
    assert_eq!(
        starts(&july, DAY_MS),
        [(jul_2 - 3 * hour, 1.0, 1.0), (jul_2 + 21 * hour, 2.0, 2.0)]
    );
    let night = [(jan_2 + 25 * hour + 2_700_000, 1.0), (jan_2 + 26 * hour, 2.0)];
    assert_eq!(
        starts(&night, FOUR_HOURS_MS),
        [(jan_2 + 22 * hour, 1.0, 1.0), (jan_2 + 26 * hour, 2.0, 2.0)]
    );
}
