// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Heikin Ashi Supertrend" by jordanfray, https://www.tradingview.com/script/9z16eauD/
// The original's notice: © jordanfray
// Changed: rewritten by hand in Rust for vike.

//! A Supertrend of the chart's Heikin-Ashi candles (from the rule's first bar, every price on the instrument's tick:
//! `vike_indicators::strategy_kit::HeikinAshiCandles`). The bands sit 2.7 times the ATR(10) of those candles above and below their middle, and the trend
//! flips when the Heikin-Ashi close crosses the last bar's opposite band. Long when it flips up and short when it flips down,
//! each at the signal bar's close for `100 / close` BTC cut to 3 decimals, reversing the other. Held, on each bar: the
//! position is closed at the close when the close is 47% past the average cost; a stop at 21% from the cost, moved to 1% on
//! the profit side once the high (low) has gone 4% past it, is placed when the close is through the level 0.1% on the profit
//! side of it, from the third bar in the trade on. Levels are rounded to 3 decimals. 0.035% commission; version 5: starts
//! with 100.
//!
//! The rule also imports a library of alert messages and a lot-size function (a library by jordanfray, MPL-2.0); only the
//! function's dollar case, `round(amount / close, 3)`, is used here. Webhooks, drawings and tables are off or decide nothing. The
//! stop placed once the move has happened is a different order from the one before it, so both can rest at once.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::HeikinAshiCandles;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The length of the 30-minute bar the rule was written on.
const BAR_MS: i64 = 1_800_000;
const MULTIPLIER: f64 = 2.7;
const DOLLARS: f64 = 100.0;
const TARGET: f64 = 47.0;
const STOP: f64 = 21.0;
const OFFSET: f64 = 0.10;
const MOVE_TO: f64 = 1.0;
const MOVE_AFTER: f64 = 4.0;

struct S {
    /// The rule's Supertrend of the candles, judged against the last bar's bands: up from the first bar, the ATR's first range
    /// the candle's `high - low`.
    supertrend: Supertrend,
    candles: HeikinAshiCandles,
    /// The last bar's direction, 0 before the first.
    direction: i32,
    moved: bool,
    last_average: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        supertrend: Supertrend::new(10.0, MULTIPLIER)
            .expect("valid factors")
            .with_previous_bands(true),
        candles: HeikinAshiCandles::default(),
        direction: 0,
        moved: false,
        last_average: f64::NAN,
    })
}

/// `math.round(x, 3)`.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.035), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let candle = self.candles.next(bar, strategy.tick());
        let direction = self.supertrend.next(&candle)[1] as i32;
        let changed = (self.direction != 0).then_some(direction - self.direction);
        self.direction = direction;
        let flip_up = changed.is_some_and(|d| d < 0);
        let flip_down = changed.is_some_and(|d| d > 0);

        let held = strategy.position();
        let (long, short) = (held > 0.0, held < 0.0);
        let average = if held != 0.0 { strategy.avg_cost() } else { bar.close };
        let just_closed = nan_to_zero(average - self.last_average) != 0.0;
        self.last_average = average;
        let since_open =
            strategy.lots().first().map_or(0, |lot| (bar.timestamp - lot.entry_timestamp) / BAR_MS);

        let side = if long { 1.0 } else { -1.0 };
        let target = average + side * average * (TARGET / 100.0);
        let mut limit = round3(average - side * average * STOP / 100.0);
        let mut trigger = round3(limit + side * limit * OFFSET / 100.0);
        let after = round3(average + side * average * (MOVE_AFTER / 100.0));
        let to = round3(average + side * average * (MOVE_TO / 100.0));
        if (long && bar.high > after || short && bar.low < after) && !self.moved {
            self.moved = true;
        }
        if self.moved {
            limit = to;
            trigger = round3(limit + side * limit * OFFSET / 100.0);
        }
        if held == 0.0 || just_closed {
            self.moved = false;
        }

        let size = round3(DOLLARS / bar.close);
        if flip_down {
            strategy.entry("Long", Long).size(size);
        }
        if long && bar.close > target {
            strategy.close("Long");
        }
        let stop_out = |through: bool| through && since_open > 2;
        if stop_out(long && bar.close < trigger) {
            let name = if self.moved { "Moved Stop" } else { "Stop Loss" };
            strategy.exit(name).from("Long").stop(limit);
        }
        if flip_up {
            strategy.entry("Short", Short).size(size);
        }
        if short && bar.close < target {
            strategy.close("Short");
        }
        if stop_out(short && bar.close > trigger) {
            let name = if self.moved { "Moved Stop" } else { "Stop Loss" };
            strategy.exit(name).from("Short").stop(limit);
        }
    }
}
