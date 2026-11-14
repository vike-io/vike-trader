// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACandles-LinearRegression-Strategy" by Trendoscope, https://www.tradingview.com/script/7ZmLpnJY/
// The original's notice: © HeWhoMustNotBeNamed
// Changed: rewritten by hand in Rust for vike.

//! Momentum of Hull-average candles: the close's HMA(60) less its EMA(20) is the difference, and the 20-bar linear regression of
//! that difference, read at the newest bar, is the line. The line is green above 0 and rising (lime above 0 otherwise), red at
//! or under 0 and falling (orange otherwise). The difference is lime at or over its 3-deviation upper band (20 bars) or 85% of
//! its 40-bar high, orange at or under the lower band or 101% of its 40-bar low, silver between.
//!
//! Long all of the account's worth when the line turns orange after red, or green after any other colour, while a supertrend of
//! SMA(200) candles points up (ATR the SMA(200) of their true range, multiplier 1, judged on closes). Closed when the difference
//! is orange on this bar and the last, after silver, and the line is not green. 0.01% commission; starts with 100,000.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::{max, min};
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

use vike_indicators::indicators::{Donchian, Ema, Hma, Linearreg, Sma, Stddev, Supertrend};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;

const NAME: &str = "Long";
const CANDLE: f64 = 60.0;
const MOMENTUM: f64 = 20.0;
const RANGE: f64 = 40.0;
const PERCENT_HIGH: f64 = 0.85;
const PERCENT_LOW: f64 = 1.01;
const MULTIPLIER: f64 = 3.0;
const TREND: f64 = 200.0;

#[derive(Clone, Copy, PartialEq)]
enum Colour {
    Green,
    Lime,
    Red,
    Orange,
    Silver,
}

/// The supertrend of SMA(200) candles, judged on their closes, as the rule writes it: vike's `Supertrend` judged against the
/// last bar's bands with gated turns (`Supertrend::with_previous_bands`, `Supertrend::with_gated_turns`), its bands one ATR
/// about the candle's close (`Supertrend::next_centred`).
struct Trend {
    open: Sma,
    close: Sma,
    high: Sma,
    low: Sma,
    range: Sma,
    last_close: f64,
    line: Supertrend,
}

impl Trend {
    fn new() -> Self {
        let sma = || Sma::new(TREND).expect("a valid length");
        Trend {
            open: sma(),
            close: sma(),
            high: sma(),
            low: sma(),
            range: sma(),
            last_close: f64::NAN,
            // The period reads nothing under `next_centred`, which takes this ATR.
            line: Supertrend::new(10.0, 1.0)
                .expect("valid parameters")
                .with_previous_bands(true)
                .with_gated_turns(true),
        }
    }

    fn next(&mut self, bar: &Bar) -> i32 {
        self.open.next_value(bar.open);
        let c = self.close.next_value(bar.close);
        let h = self.high.next_value(bar.high);
        let l = self.low.next_value(bar.low);
        let pc = self.last_close;
        let tr = max(h, pc) - min(l, pc);
        // The average starts at the range's first number, so its warm-up NaNs never enter its window.
        let atr = if tr.is_nan() { f64::NAN } else { self.range.next_value(tr) };
        let up = self.line.next_centred(c, c, atr)[1] > 0.0;
        self.last_close = c;
        if up { 1 } else { -1 }
    }
}

struct S {
    candle: Hma,
    average: Ema,
    line: Linearreg,
    dev: Stddev,
    mid: Sma,
    range: Donchian,
    trend: Trend,
    last_line: f64,
    /// The line's colours, newest first, two.
    line_colours: VecDeque<Colour>,
    /// The difference's colours, newest first, three.
    colours: VecDeque<Colour>,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        candle: Hma::new(CANDLE).expect("a valid length"),
        average: Ema::new(MOMENTUM).expect("a valid length").with_skip_nan(true),
        line: Linearreg::new(MOMENTUM).expect("a valid length"),
        dev: Stddev::new(MOMENTUM).expect("a valid length"),
        mid: Sma::new(MOMENTUM).expect("a valid length"),
        range: Donchian::new(RANGE).expect("a valid length"),
        trend: Trend::new(),
        last_line: f64::NAN,
        line_colours: VecDeque::new(),
        colours: VecDeque::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.01), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let candle = self.candle.next_value(bar.close);
        let diff = candle - self.average.next_value(candle);
        let line = self.line.next_value(diff);
        let last = std::mem::replace(&mut self.last_line, line);
        let last = nan_to_zero(last);
        let line_colour = match (line > 0.0, line > last, line < last) {
            (true, true, _) => Colour::Green,
            (true, false, _) => Colour::Lime,
            (false, _, true) => Colour::Red,
            (false, _, false) => Colour::Orange,
        };
        let dev = MULTIPLIER * self.dev.next_value(diff);
        let mid = self.mid.next_value(diff);
        let [high, _, low] = self.range.next_hl(diff, diff);
        let colour = if diff >= mid + dev || diff >= high * PERCENT_HIGH {
            Colour::Lime
        } else if diff <= mid - dev || diff <= low * PERCENT_LOW {
            Colour::Orange
        } else {
            Colour::Silver
        };
        let dir = self.trend.next(bar);
        self.line_colours.push_front(line_colour);
        self.line_colours.truncate(2);
        self.colours.push_front(colour);
        self.colours.truncate(3);

        let before = self.line_colours.get(1).copied();
        let turned = (line_colour == Colour::Orange && before == Some(Colour::Red))
            || (line_colour == Colour::Green && before != Some(Colour::Green));
        if turned && dir > 0 {
            strategy.entry(NAME, Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        let c = |i: usize| self.colours.get(i).copied();
        let exit = c(0) == Some(Colour::Orange)
            && c(1) == Some(Colour::Orange)
            && c(2) == Some(Colour::Silver)
            && line_colour != Colour::Green;
        if exit {
            strategy.close(NAME);
        }
    }
}
