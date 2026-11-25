// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Joint Conditions Strategy Suite + TradingConnector alerts bot" by Peter_O, https://www.tradingview.com/script/2SMDLIj7/
// The original's notice: © Peter_O
// Ported into this file: the library "MTFindicators" (Peter_O/MTFindicators/8), MPL-2.0 by the Terms of Use of the site that publishes it; it states no notice
// Changed: rewritten by hand in Rust for vike.

//! Three joint conditions a side, at the original's defaults. Long when EMA(50) of the close crosses up through EMA(200),
//! the MACD(12, 26, 9) histogram is over 0 and the ADX(14, 14) is over 25; short when EMA(50) crosses down through EMA(200),
//! the histogram is under 0 and the ADX is over 25. Each side enters only while not already on it, reversing the other,
//! within 09:00 to 16:00 New York time (the US daylight rule), filled at the next open: 100 shares. Each entry carries a stop
//! and a target 100 ticks from its fill (100 points, the original's default), live from that fill. Commission 0.0003 per
//! order; starts with 1,000,000; margin 100% each side. The trailing stop is off by default and left out, as are the
//! alerts, the warning table and the plots.
//!
//! The library's moving average, MACD and ADX are ported here at the multiplier 1 the original sets for each (its other
//! indicators, the RSIs, the stochastic, the Bollinger and Keltner bands and the volume average, are computed by the
//! original but read by no default condition, so they are left out). Its ADX: the directional moves of the high and of the
//! low (missing on the first bar), a true range against the close before read as 0 on the first bar (so the first range is
//! that bar's high), Wilder averages of the three, each DI held at its last number when it has none, and `100 *` the Wilder
//! average of `|+DI - -DI| / (+DI + -DI)` (over 1 when the sum is 0).
//!
//! **Data:** the reference ran on NASDAQ:AAPL, the exchange's share, split-adjusted; this file is checked on Dukascopy's
//! AAPL.CFD, a CFD, not the exchange stock, whose bars are the quotes' mid prices of the regular session, held unadjusted
//! (AAPL's 4:1 split of 2020-08-31 shows as a close of 499.19 on 2020-08-28 and an open of 127.64 on 2020-08-31) and read
//! split-adjusted by the backtest from the operator's split row. The trade count is the reference's; the net is not, the
//! other feed's price levels.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Macd, Smma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::us_minute;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const SHARES: f64 = 100.0;
const ADX_LENGTH: f64 = 14.0;
const ADX_OVER: f64 = 25.0;
/// The stop's and the target's distance from the entry's fill, in ticks of the instrument.
const WIDTH_TICKS: f64 = 100.0;
const NEW_YORK: i64 = -5;
/// The trading session, 09:00 to 16:00, in minutes of the day.
const SESSION: (i64, i64) = (9 * 60, 16 * 60);

fn wilder() -> Smma {
    Smma::new(ADX_LENGTH).expect("a valid length").with_skip_nan(true).with_alpha_steps(true)
}

/// The library's ADX at multiplier 1 (the module doc).
struct Adx {
    before: Option<Bar>,
    range: Smma,
    plus: Smma,
    minus: Smma,
    adx: Smma,
    plus_held: f64,
    minus_held: f64,
}

impl Adx {
    fn new() -> Self {
        Adx {
            before: None,
            range: wilder(),
            plus: wilder(),
            minus: wilder(),
            adx: wilder(),
            plus_held: f64::NAN,
            minus_held: f64::NAN,
        }
    }

    fn next(&mut self, bar: &Bar) -> f64 {
        let before = self.before.replace(bar.clone());
        let (up, down) = match &before {
            None => (f64::NAN, f64::NAN),
            Some(b) => (bar.high - b.high, -(bar.low - b.low)),
        };
        let plus_move = if up.is_nan() {
            f64::NAN
        } else if up > down && up > 0.0 {
            up
        } else {
            0.0
        };
        let minus_move = if down.is_nan() {
            f64::NAN
        } else if down > up && down > 0.0 {
            down
        } else {
            0.0
        };
        let close_before = before.map_or(0.0, |b| b.close);
        let range = (bar.high - bar.low)
            .max((bar.high - close_before).abs())
            .max((bar.low - close_before).abs());
        let range = self.range.next_value(range);
        let plus = 100.0 * self.plus.next_value(plus_move) / range;
        let minus = 100.0 * self.minus.next_value(minus_move) / range;
        if !plus.is_nan() {
            self.plus_held = plus;
        }
        if !minus.is_nan() {
            self.minus_held = minus;
        }
        let sum = self.plus_held + self.minus_held;
        let spread = (self.plus_held - self.minus_held).abs() / if sum == 0.0 { 1.0 } else { sum };
        100.0 * self.adx.next_value(spread)
    }
}

struct S {
    fast: Ema,
    slow: Ema,
    macd: Macd,
    adx: Adx,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        fast: Ema::new(50.0).expect("a valid length"),
        slow: Ema::new(200.0).expect("a valid length"),
        macd: Macd::new(12.0, 26.0, 9.0).expect("valid lengths"),
        adx: Adx::new(),
        cross: Cross::new(),
    })
}

/// The stop and the target of what is held, `WIDTH_TICKS` ticks of `tick` from its entry.
fn protect(strategy: &mut Tactics<'_>, tick: f64) {
    let width = WIDTH_TICKS * tick;
    let held = strategy.position();
    let cost = strategy.avg_cost();
    if held > 0.0 {
        strategy.exit("xl").from(UP).stop(cost - width).take(cost + width);
    } else if held < 0.0 {
        strategy.exit("xs").from(DOWN).stop(cost + width).take(cost - width);
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::per_order(0.0003), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        let fast = self.fast.next(bar);
        let slow = self.slow.next(bar);
        let [_, _, histogram] = self.macd.next(bar);
        let adx = self.adx.next(bar);
        let cross = self.cross.step(fast, slow);
        let in_session = (SESSION.0..SESSION.1).contains(&us_minute(bar.timestamp, NEW_YORK));
        let go_long = matches!(cross, Signal::Long) && histogram > 0.0 && adx > ADX_OVER;
        let go_short = matches!(cross, Signal::Short) && histogram < 0.0 && adx > ADX_OVER;

        let held = strategy.position();
        if go_long && in_session && held <= 0.0 {
            strategy.entry(UP, Long).size(SHARES);
        }
        protect(strategy, tick);
        if go_short && in_session && held >= 0.0 {
            strategy.entry(DOWN, Short).size(SHARES);
        }
    }

    // The exits' levels are measured from the entry's fill, so they are issued here, after it; under the simulator's price
    // order (`[engine.in_bar] price_order`) they work from the fill's own level on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, _bar: &Bar) {
        let Some(tick) = strategy.tick() else { return };
        protect(strategy, tick);
    }
}
