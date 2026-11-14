// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy Myth-Busting #12 - OSGFC+SuperTrend - [MYN]" by myncrypto, https://www.tradingview.com/script/eg9wcYuj/
// The original's notice: © myn
// Changed: rewritten by hand in Rust for vike.

//! A one-sided Gaussian filter of the close and a Supertrend(10, 3) agree. Long all of the account's worth, as `worth / close`
//! floored to the step, when the filter turns up and the Supertrend is up; short it when the filter turns down and the
//! Supertrend is down; neither enters a side already held, each reverses the other. Every turn of the filter, up or down,
//! also issues a close of everything. Orders fill at the next open. 0.075% commission; starts with 1,000, kept in USD.
//!
//! The filter is the library `loxxexpandedsourcetypes` (version 4, MPL-2.0, by loxx) imported for the plain close, and the
//! rule's own code (`One-Sided Gaussian Filter w/ Channels`, by Loxx): the last 11 closes weighted by a Gaussian of depth 144
//! over 11 bars (a missing close counts 0), run through an Ehlers 2-pole smoother of period 10 whose first three bars take the
//! input. A turn is the line crossing its own value of the bar before. An entry also needs the opposite turn, or the opposite
//! Supertrend, to have happened at least one bar ago, which a missing one fails. The rule's take-profits sit at twice the
//! entry price and at zero, its stop at 999%, and its ADX filter is off: none acts, so they are left out.
//!
//! An entry and the close of everything on one bar: a bar that turns the filter is also a bar that closes everything, and the
//! entry meets the close at the same open. The entry stands and the close is a no-op when flat; a bar that turns the filter
//! without an entry (the side already held, or the Supertrend against it) closes what is held.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::{BarsSince, nan_to_zero};
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const DEPTH: usize = 11;
const GAUSS_SIZE: f64 = 144.0;
const SMOOTH_PERIOD: f64 = 10.0;

struct S {
    /// The normalised Gaussian weights of the filter's 11 bars.
    weights: [f64; DEPTH],
    /// The last closes, newest first.
    closes: [f64; DEPTH],
    /// The smoother's last two outputs, newest first, and the line's last two values.
    filt: [f64; 2],
    line: [f64; 2],
    bar_index: usize,
    trend: Supertrend,
    since_up: BarsSince,
    since_down: BarsSince,
    since_trend_up: BarsSince,
    since_trend_down: BarsSince,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let raw: Vec<f64> = (0..DEPTH)
        .map(|i| {
            let x = i as f64;
            libm::exp(-x * x * 9.0 / ((GAUSS_SIZE + 1.0) * (GAUSS_SIZE + 1.0)))
        })
        .collect();
    let mut sum = 0.0;
    for w in &raw {
        sum += w;
    }
    let mut weights = [0.0; DEPTH];
    for (w, r) in weights.iter_mut().zip(&raw) {
        *w = r / sum;
    }
    Box::new(S {
        weights,
        closes: [f64::NAN; DEPTH],
        filt: [f64::NAN; 2],
        line: [f64::NAN; 2],
        bar_index: 0,
        trend: Supertrend::new(10.0, 3.0).expect("valid parameters").with_down_start(true),
        since_up: BarsSince::default(),
        since_down: BarsSince::default(),
        since_trend_up: BarsSince::default(),
        since_trend_down: BarsSince::default(),
    })
}

impl S {
    /// The filter's line on this bar.
    // Left: the one-sided Gaussian weighting is one script's own design, published nowhere else, so vike has no row.
    fn gaussian(&mut self, close: f64) -> f64 {
        self.closes.rotate_right(1);
        self.closes[0] = close;
        let mut smooth = 0.0;
        for (w, c) in self.weights.iter().zip(&self.closes) {
            smooth += w * nan_to_zero(*c);
        }
        let a1 = libm::exp(-1.414 * std::f64::consts::PI / SMOOTH_PERIOD);
        let b1 = 2.0 * a1 * libm::cos(1.414 * std::f64::consts::PI / SMOOTH_PERIOD);
        let (coef2, coef3) = (b1, -a1 * a1);
        let coef1 = 1.0 - coef2 - coef3;
        let filt =
            coef1 * smooth + coef2 * nan_to_zero(self.filt[0]) + coef3 * nan_to_zero(self.filt[1]);
        let filt = if self.bar_index < 3 { smooth } else { filt };
        self.filt = [filt, self.filt[0]];
        filt
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let line = self.gaussian(bar.close);
        let [last, before] = self.line;
        let go_long = line > last && last <= before;
        let go_short = line < last && last >= before;
        self.line = [line, last];
        self.bar_index += 1;
        // Each count runs on every bar, so each is stepped before any `&&` can skip it.
        let older = |since: Option<usize>| matches!(since, Some(n) if n > 0);
        let (after_down, after_up) = (self.since_down.step(go_short), self.since_up.step(go_long));
        let (long_filter, short_filter) =
            (go_long && older(after_down), go_short && older(after_up));

        // The rule's direction: 1 down, -1 up.
        let direction = -self.trend.next(bar)[1];
        let (trend_up, trend_down) = (direction < 0.0, direction > 0.0);
        let after_trend_down = self.since_trend_down.step(trend_down);
        let after_trend_up = self.since_trend_up.step(trend_up);
        let long_trend = trend_up && older(after_trend_down);
        let short_trend = trend_down && older(after_trend_up);

        let held = strategy.position();
        let size = sizing.floor(sizing.equity() / bar.close);
        let mut entered = false;
        if long_filter && long_trend && held <= 0.0 {
            strategy.entry("Long", Long).size(size);
            entered = true;
        }
        if short_filter && short_trend && held >= 0.0 {
            strategy.entry("Short", Short).size(size);
            entered = true;
        }
        if (go_long || go_short) && !entered {
            strategy.close_all();
        }
    }
}
