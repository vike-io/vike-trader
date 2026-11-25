// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Open Open Cross Strategy R7.1b revised by MZ" by MauricioZuniga, https://www.tradingview.com/script/ZLyHPGfK/
// The original's notice: © 2013  John F. Ehlers
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-06-24, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Long only. A TEMA(8) of the open on 90-minute bars (three times the gap between the EMA and the EMA of that, plus the EMA of
//! the EMA of the EMA) against the same TEMA of the 90-minute bar before's open; an entry signal when the first crosses up
//! through the second while the ADX of the 5-minute bars (DMI(14) smoothed over 40) is from 20 to 100. Flat, a signal buys all
//! of the account's worth at its bar's close. A signal while flat sets the take-profit level 4% over its bar's close, kept
//! while held. Held, the position closes at the close of the bar on which the TEMA(8) of the chart's own open falls under the
//! average cost less 6%; and once a close is 4% over the average cost, a trailing exit arms at that level, rounded down to the
//! tick, and follows the best price by 0.5% of it, cut to whole ticks. Orders fill at the close of the bar that placed them.
//! Commission 0.08%; version 5: starts with 1,000,000. The RSI, ATR, moving-average and second ADX filters are off, so are the
//! uptrend-only filter and the short side. The labels, plots and alert comments are left out.
//!
//! The rule states two defaults for its fixed stop, a number (6) and then a `defval` named (1); the first is taken, measured:
//! with it the reference's 7 trades, with the named one 25 trades and 171344.12 against 233632.83. Its guard against the bar in progress reads the bar before only while a bar is
//! unconfirmed, which a closed bar never is; it recomputes the 90-minute pair only on two bars of every three counted from the
//! first bar, holding it on the third, where no new 90-minute bar has closed, so it reads the same as each bar's own. It trades
//! only on the last 10,000 bars before the moment it is run, and every bar it can trade on here (none before its 5-minute
//! series starts) is inside them.
//!
//! The 90-minute frame is built from the run's own symbol's 30-minute bars from the rule's first bar on
//! (`vike_tactics::higher::HigherTimeframeSourceIntraday`), each bar of it read once its period has closed. The reference's
//! record lists its 90-minute series from 2023-01-01; fed the 30-minute history from 2024-12-01 instead of the window, the run
//! gives the same numbers (the TEMA has long forgotten where it started before the first trade). The 5-minute frame
//! is an input of the run's own symbol at that bar length (decision 0271), read at the chart bar's close: the ADX over its bars
//! from the rule's first bar on, as it stands after the last of them inside the chart bar.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Ema};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::higher::{self, HigherTimeframeSourceIntraday, HigherTimeframeSourceUnseen};
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics, TacticsTrail};

const FIVE_MINUTES: &str = "5m";
const FRAME_MINUTES: i64 = 90;
const BASIS: f64 = 8.0;
const DMI_LENGTH: f64 = 14.0;
const ADX_SMOOTHING: f64 = 40.0;
const ADX_MIN: f64 = 20.0;
const ADX_MAX: f64 = 100.0;
const TAKE_PROFIT: f64 = 4.0 / 100.0;
const TAKE_PROFIT_TRAIL: f64 = 0.5 / 100.0;
const LOSS: f64 = 6.0 / 100.0;
const LONG: &str = "long";

/// The rule's TEMA, `3 * (ema - ema(ema)) + ema(ema(ema))`, each average seeded by the mean of its first `BASIS` values and a
/// missing value skipped.
struct Tema {
    first: Ema,
    second: Ema,
    third: Ema,
}

impl Tema {
    fn new() -> Self {
        let ema = || Ema::new(BASIS).expect("a valid length").with_skip_nan(true);
        Tema { first: ema(), second: ema(), third: ema() }
    }

    /// The TEMA after `value`, `bar` lending the rest of the bar each average is fed.
    fn next(&mut self, value: f64, bar: &Bar) -> f64 {
        let of = |close: f64| Bar { close, ..bar.clone() };
        let first = self.first.next(&of(value));
        let second = self.second.next(&of(first));
        let third = self.third.next(&of(second));
        3.0 * (first - second) + third
    }
}

struct S {
    /// The rule's first bar's open: no 5-minute bar before it is fed.
    from: Option<i64>,
    fives: HigherTimeframeSourceUnseen,
    dmi: Adx,
    adx: f64,
    frames: HigherTimeframeSourceIntraday,
    frame_close: Tema,
    frame_open: Tema,
    frame_open_before: f64,
    close_series: f64,
    open_series: f64,
    cross: Cross,
    chart: Tema,
    /// The take-profit level a signal while flat set, kept while held.
    take_profit: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let dmi = Adx::new(DMI_LENGTH)
        .and_then(|a| a.with_adx_period(ADX_SMOOTHING))
        .expect("valid lengths")
        .with_held_lines(true);
    Box::new(S {
        from: None,
        fives: HigherTimeframeSourceUnseen::new(),
        dmi,
        adx: f64::NAN,
        frames: HigherTimeframeSourceIntraday::new(FRAME_MINUTES),
        frame_close: Tema::new(),
        frame_open: Tema::new(),
        frame_open_before: f64::NAN,
        close_series: f64::NAN,
        open_series: f64::NAN,
        cross: Cross::new(),
        chart: Tema::new(),
        take_profit: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.08), ..RuleDefaults::PLAIN }
    }

    fn inputs(&self) -> Vec<String> {
        vec![FIVE_MINUTES.to_string(), higher::HALF_HOUR.to_string()]
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let from = *self.from.get_or_insert(bar.timestamp);
        let Some(tick) = strategy.tick() else { return };
        let fives = self.fives.next(strategy.input(FIVE_MINUTES));
        for five in &fives[fives.partition_point(|b| b.timestamp < from)..] {
            self.adx = self.dmi.next(five)[0];
        }
        for frame in self.frames.next(bar, strategy.input(higher::HALF_HOUR)) {
            self.close_series = self.frame_close.next(frame.open, &frame);
            self.open_series = self.frame_open.next(self.frame_open_before, &frame);
            self.frame_open_before = frame.open;
        }
        let crossed = matches!(self.cross.step(self.close_series, self.open_series), Signal::Long);
        let entry = crossed && (ADX_MIN..=ADX_MAX).contains(&self.adx);
        let chart_series = self.chart.next(bar.open, bar);

        let held = strategy.position();
        let cost = strategy.avg_cost();
        let exit = chart_series < cost * (1.0 - LOSS);
        self.take_profit = if !(entry || held > 0.0) {
            f64::NAN
        } else if (entry && held <= 0.0) || self.take_profit.is_nan() {
            bar.close * (1.0 + TAKE_PROFIT)
        } else {
            self.take_profit
        };

        if entry && held == 0.0 {
            let size = share_of_worth(&strategy.sizing(), 100.0, bar.close);
            strategy.entry(LONG, Long).size(size);
        }
        if exit {
            strategy.close(LONG);
        }
        if (held > 0.0 && bar.close > cost * (1.0 + TAKE_PROFIT)) || exit {
            // The arming level rounds down to the tick (within 1e-4 of one), the offset truncates.
            let arm = (self.take_profit / tick + 1e-4).floor();
            let offset = (self.take_profit * TAKE_PROFIT_TRAIL / tick).trunc();
            let after = arm - (cost / tick).round();
            let trail = TacticsTrail { after_ticks: after, by_ticks: offset };
            strategy.exit("Long Trailing Profit").from(LONG).trail(trail);
        }
    }
}
