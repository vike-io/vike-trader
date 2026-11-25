// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "2Mars - MA / BB / SuperTrend" by facejungle, https://www.tradingview.com/script/Y68meyX1/
// The original's notice: ©facejungle
// Ported into this file: the library "utils" (facejungle/utils/17), MPL-2.0; its notice: © facejungle
// Changed: rewritten by hand in Rust for vike.

//! Bands of the close around its 30-bar weighted average (the library's: weights 30 * 30 down to 1 * 30), 3 population
//! standard deviations (30 bars) wide and scaled by 0.618, 1 and 1.618 on each side; the library's 96- and 89-bar simple
//! averages of the close (each a sum of the closes over the length, newest first); a supertrend (20 bars, factor 4).
//!
//! While the supertrend is up, a long of 5% of the account's worth at the bar's close on the second close in a row over the
//! band 1.618 deviations under the average, on the second over the band 1 deviation under it, and on the second bar in a row
//! with the 89-bar average over the 96-bar one; a short on the mirror (closes under the upper bands, the 89-bar average
//! under the 96-bar one) while the supertrend is down. An entry closes the other side first. Up to 3 entries a side.
//! Held long, a close crossing up through the 1-deviation or the 1.618-deviation upper band of the bar before, over the
//! average price, takes 5% of the position at the close (the later of the two on one bar), the 1.618 one also setting the
//! stop again; a short the mirror. The stop is set at each entry: the bar's low less 6 ATR(12), rounded to the cent, for a
//! long; the high plus 4.3 ATR(12) for a short. With the supertrend turned against the position and the bar's open-close
//! midpoint past the stop, the side is closed at the close. 0.1% commission; starts with 10,000 in a USD account, margined
//! at 100%. **Does not reproduce its reference** (707 trades, net -626.084717 on BTCUSDT 30m, 2025-01-01..2026-08-28): this
//! file gives 708 / -621.913137, one trade more and the net 4.17 over, no cause found (no other reading of the entry below
//! was run); the owner accepted those numbers on 2026-11-15.
//!
//! The original places each entry as a stop order at that stop price, which is already through the market when it is
//! placed: under its process-on-close it fills at the bar's close, as a market order would.
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
use vike_indicators::indicators::{Atr, Stddev, Supertrend};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BAND_LENGTH: usize = 30;
const BAND_DEVIATIONS: f64 = 3.0;
const BASIS_LENGTH: usize = 96;
const SIGNAL_LENGTH: usize = 89;
const CONFIRM: u32 = 2;
const SHARE: f64 = 5.0;
const TAKE_PROFIT: f64 = 5.0 / 100.0;
const STOP_LONG: f64 = 6.0;
const STOP_SHORT: f64 = 4.3;

/// Consecutive bars a condition held, 0 on a bar it does not.
#[derive(Default)]
struct Counter(u32);

impl Counter {
    fn step(&mut self, holds: bool) -> u32 {
        self.0 = if holds { self.0 + 1 } else { 0 };
        self.0
    }
}

/// A crossover of `a` through `b`: over it now, at or under it on the bar before.
#[derive(Default)]
struct Crossing {
    before: Option<(f64, f64)>,
}

impl Crossing {
    fn step(&mut self, a: f64, b: f64, up: bool) -> bool {
        let crossed = self
            .before
            .is_some_and(|(pa, pb)| if up { a > b && pa <= pb } else { a < b && pa >= pb });
        self.before = Some((a, b));
        crossed
    }
}

struct S {
    /// The last closes, newest first.
    closes: VecDeque<f64>,
    deviation: Stddev,
    atr: Atr,
    trend: Supertrend,
    over_lower3: Counter,
    over_lower2: Counter,
    signal_over: Counter,
    under_upper3: Counter,
    under_upper2: Counter,
    signal_under: Counter,
    /// The bands of the bar before: upper 1, upper 1.618, lower 1, lower 1.618 deviations.
    bands_before: [f64; 4],
    take_long2: Crossing,
    take_long3: Crossing,
    take_short2: Crossing,
    take_short3: Crossing,
    stop_long: f64,
    stop_short: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        closes: VecDeque::with_capacity(BASIS_LENGTH + 1),
        deviation: Stddev::new(BAND_LENGTH as f64).expect("a valid length"),
        atr: Atr::new(12.0).expect("a valid length"),
        trend: Supertrend::new(20.0, 4.0).expect("valid parameters").with_down_start(true),
        over_lower3: Counter::default(),
        over_lower2: Counter::default(),
        signal_over: Counter::default(),
        under_upper3: Counter::default(),
        under_upper2: Counter::default(),
        signal_under: Counter::default(),
        bands_before: [f64::NAN; 4],
        take_long2: Crossing::default(),
        take_long3: Crossing::default(),
        take_short2: Crossing::default(),
        take_short3: Crossing::default(),
        stop_long: f64::NAN,
        stop_short: f64::NAN,
    })
}

/// The cent a stop price is rounded to.
fn cents(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

impl S {
    /// The library's simple average: the sum of `close / length` over the last `length` closes, newest first.
    fn average(&self, length: usize) -> f64 {
        if self.closes.len() < length {
            return f64::NAN;
        }
        let divisor = length as f64;
        self.closes.iter().take(length).fold(0.0, |sum, &x| sum + x / divisor)
    }

    /// The library's weighted average: weight `(length - i) * length` on the close `i` bars back.
    fn weighted(&self, length: usize) -> f64 {
        if self.closes.len() < length {
            return f64::NAN;
        }
        let (mut norm, mut sum) = (0.0, 0.0);
        for (i, &x) in self.closes.iter().take(length).enumerate() {
            let weight = ((length - i) * length) as f64;
            norm += weight;
            sum += x * weight;
        }
        sum / norm
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), pyramiding: 3, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let close = bar.close;
        self.closes.push_front(close);
        self.closes.truncate(BASIS_LENGTH);
        let direction = self.trend.next(bar)[1];
        let (up, down) = (direction > 0.0, direction < 0.0);
        let basis = self.weighted(BAND_LENGTH);
        let spread = BAND_DEVIATIONS * self.deviation.next(bar);
        let (upper2, upper3) = (basis + spread, basis + spread * 1.618);
        let (lower2, lower3) = (basis - spread, basis - spread * 1.618);
        let ma_basis = self.average(BASIS_LENGTH);
        let ma_signal = self.average(SIGNAL_LENGTH);
        let atr = self.atr.next(bar);
        let calc_stop_long = cents(bar.low - atr * STOP_LONG);
        let calc_stop_short = cents(bar.high + atr * STOP_SHORT);

        let position = strategy.position();
        let held_long = position > 0.0;
        let held_short = position < 0.0;
        let long3 = self.over_lower3.step(close > lower3) == CONFIRM;
        let long2 = self.over_lower2.step(close > lower2) == CONFIRM;
        let ma_long = self.signal_over.step(ma_signal > ma_basis) == CONFIRM;
        let short3 = self.under_upper3.step(close < upper3) == CONFIRM;
        let short2 = self.under_upper2.step(close < upper2) == CONFIRM;
        let ma_short = self.signal_under.step(ma_signal < ma_basis) == CONFIRM;

        let sizing = strategy.sizing();
        let entries_long = [long3 && up, long2 && up, ma_long && up];
        for _ in entries_long.iter().filter(|&&go| go) {
            self.stop_long = calc_stop_long;
            if held_short {
                strategy.cancel("Short");
                strategy.close("Short");
            }
            strategy.entry("Long", Long).size(share_of_worth(&sizing, SHARE, close));
        }
        let entries_short = [short3 && down, short2 && down, ma_short && down];
        for _ in entries_short.iter().filter(|&&go| go) {
            self.stop_short = calc_stop_short;
            if held_long {
                strategy.cancel("Long");
                strategy.close("Long");
            }
            strategy.entry("Short", Short).size(share_of_worth(&sizing, SHARE, close));
        }

        // The take profits cross the bands of the bar before.
        let [upper2_before, upper3_before, lower2_before, lower3_before] = self.bands_before;
        self.bands_before = [upper2, upper3, lower2, lower3];
        let cost = strategy.avg_cost();
        let take_long2 = self.take_long2.step(close, upper2_before, true) && held_long;
        let take_long3 = self.take_long3.step(close, upper3_before, true) && held_long;
        let take_short2 = self.take_short2.step(close, lower2_before, false) && held_short;
        let take_short3 = self.take_short3.step(close, lower3_before, false) && held_short;
        let step = sizing.qty_step();
        let part = floor_share(position.abs() * TAKE_PROFIT, position.abs(), step);
        if take_long2 && cost < close {
            strategy.close("Long").size(part);
        }
        if take_short2 && cost > close {
            strategy.close("Short").size(part);
        }
        if take_long3 && cost < close {
            self.stop_long = calc_stop_long;
            strategy.close("Long").size(part);
        }
        if take_short3 && cost > close {
            self.stop_short = calc_stop_short;
            strategy.close("Short").size(part);
        }

        let midpoint = (bar.open + close) / 2.0;
        if midpoint < self.stop_long && held_long && down {
            strategy.cancel("Long");
            strategy.close("Long");
        }
        if midpoint > self.stop_short && held_short && up {
            strategy.cancel("Short");
            strategy.close("Short");
        }
    }
}
