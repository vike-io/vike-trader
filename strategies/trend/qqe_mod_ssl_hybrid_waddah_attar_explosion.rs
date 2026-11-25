// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "QQE MOD + SSL Hybrid + Waddah Attar Explosion" by kevinmck100, https://www.tradingview.com/script/YCob5r03/
// The original's notice: Copyright © 2019 to present, Joris Duyck (JD)
// The original's notice: Copyright © 2018 Alex Orekhov (everget)
// The original's notice: Copyright © 2017 Jurik Research and Consulting.
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2022-05-05, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! Three signals on one bar open a trade, sized to risk 2% of the account's worth on the stop. A QQE MOD colour change (the
//! RSI(6)'s EMA(6) outside its Bollinger band of 50 bars at 0.35 deviations, and the second EMA(5) of it more than 3 off
//! 50), the close outside a Keltner channel of a 60-bar Hull average of the close (the true range's EMA(60) times 0.2) on
//! the same side of that average, and a Waddah Attar explosion (the change of EMA(20) less EMA(40), times 180, over its
//! Bollinger band width of 20 bars at 2 deviations). Entered only when flat; the stop is the lowest low of 10 bars (the
//! highest high for a short). A trade is closed when the close crosses a 15-bar Hull average of the highs or lows against it
//! while the open trades are in profit. 0.01% commission; starts with 1,000.
//!
//! The rule's Hull averages round the square root of the length (`Hma::with_rounded_root`). The QQE line is vike's `Qqe`.
//! The open trades' profit is read after the commission of each trade's entry and of closing it at this close.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Donchian, Ema, Hma, Qqe, Rsi, Sma, Stddev, TrueRange};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

/// The reference's Hull average of a length: its half length cut to a whole number (7 for 15), its last length
/// `round(sqrt(length))`.
fn hull(length: f64) -> Hma {
    Hma::new(length).expect("a valid length").with_rounded_root(true)
}

/// The commission, in percent of a fill's value.
const COMMISSION_PERCENT: f64 = 0.01;

/// An EMA that skips a missing input (the reference's, on a series that starts late).
fn late_ema(length: f64) -> Ema {
    Ema::new(length).expect("a valid length").with_skip_nan(true)
}

struct S {
    /// vike's `Qqe` (6, 6, 3) on the RS-form RSI: the smoothed RSI and its trailing level.
    qqe: Qqe,
    rsi: Rsi,
    rsi_ma2: Ema,
    band_basis: Sma,
    band_dev: Stddev,
    was_green: bool,
    was_red: bool,
    baseline: Hma,
    exit_high: Hma,
    exit_low: Hma,
    tr: TrueRange,
    range_ema: Ema,
    hlv: f64,
    close_over: Cross,
    exit_over: Cross,
    fast: Ema,
    slow: Ema,
    previous_macd: f64,
    basis: Sma,
    dev: Stddev,
    swing: Donchian,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        qqe: Qqe::new(6.0, 6.0, 3.0).expect("valid parameters").with_rs_form(true),
        rsi: Rsi::new(6.0).expect("a valid length").with_rs_form(true),
        rsi_ma2: late_ema(5.0),
        band_basis: Sma::new(50.0).expect("a valid length"),
        band_dev: Stddev::new(50.0).expect("a valid length"),
        was_green: false,
        was_red: false,
        baseline: hull(60.0),
        exit_high: hull(15.0),
        exit_low: hull(15.0),
        tr: TrueRange::default(),
        range_ema: Ema::new(60.0).expect("a valid length"),
        hlv: f64::NAN,
        close_over: Cross::new(),
        exit_over: Cross::new(),
        fast: Ema::new(20.0).expect("a valid length"),
        slow: Ema::new(40.0).expect("a valid length"),
        previous_macd: f64::NAN,
        basis: Sma::new(20.0).expect("a valid length"),
        dev: Stddev::new(20.0).expect("a valid length"),
        swing: Donchian::new(10.0).expect("a valid length"),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(COMMISSION_PERCENT), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        // QQE MOD.
        let rsi_ma2 = self.rsi_ma2.next_value(self.rsi.next_value(bar.close));
        let [rsi_ma, trail] = self.qqe.next_value(bar.close);
        let line = trail - 50.0;
        let (basis, deviation) = if line.is_nan() {
            (f64::NAN, f64::NAN)
        } else {
            (self.band_basis.next_value(line), 0.35 * self.band_dev.next_value(line))
        };
        let (upper, lower) = (basis + deviation, basis - deviation);
        let green = rsi_ma2 - 50.0 > 3.0 && rsi_ma - 50.0 > upper;
        let red = rsi_ma2 - 50.0 < -3.0 && rsi_ma - 50.0 < lower;
        let qqe_buy = green && !self.was_green;
        let qqe_sell = red && !self.was_red;
        (self.was_green, self.was_red) = (green, red);

        // SSL hybrid.
        let baseline = self.baseline.next_value(bar.close);
        let range = self.range_ema.next_value(self.tr.next(bar));
        let upper_k = baseline + range * 0.2;
        let lower_k = baseline - range * 0.2;
        let ssl_buy = bar.close > upper_k && bar.close > baseline;
        let ssl_sell = bar.close < lower_k && bar.close < baseline;
        let (exit_high, exit_low) =
            (self.exit_high.next_value(bar.high), self.exit_low.next_value(bar.low));
        if bar.close > exit_high {
            self.hlv = 1.0;
        } else if bar.close < exit_low {
            self.hlv = -1.0;
        }
        let ssl_exit = if self.hlv < 0.0 { exit_high } else { exit_low };
        let base_cross_long = matches!(self.close_over.step(bar.close, ssl_exit), Signal::Long);
        let base_cross_short = matches!(self.exit_over.step(ssl_exit, bar.close), Signal::Long);

        // Waddah Attar explosion.
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let t1 = (macd - self.previous_macd) * 180.0;
        self.previous_macd = macd;
        let (mid, sd) = (self.basis.next_value(bar.close), 2.0 * self.dev.next_value(bar.close));
        let explosion = (mid + sd) - (mid - sd);
        let trend_up = if t1 >= 0.0 { t1 } else { 0.0 };
        let trend_down = if t1 < 0.0 { -t1 } else { 0.0 };
        let wae_buy = trend_up > 0.0 && trend_up > explosion;
        let wae_sell = trend_down > 0.0 && trend_down > explosion;

        // The trades.
        let [swing_high, _, swing_low] = self.swing.next_hl(bar.high, bar.low);
        let held = strategy.position();
        let risk = strategy.equity() * 2.0 / 100.0;
        if qqe_buy && ssl_buy && wae_buy && held == 0.0 {
            let stop_pct = ((1.0 - swing_low / bar.close) * 100.0).abs();
            let qty = sizing.floor((risk / stop_pct * 100.0).abs() / bar.close);
            if qty.is_finite() {
                strategy.entry("Long", Long).size(qty);
                strategy.exit("Long  SL/TP").from("Long").stop(swing_low);
            }
        }
        if qqe_sell && ssl_sell && wae_sell && held == 0.0 {
            let stop_pct = ((1.0 - swing_high / bar.close) * 100.0).abs();
            let qty = sizing.floor((risk / stop_pct * 100.0).abs() / bar.close);
            if qty.is_finite() {
                strategy.entry("Short", Short).size(qty);
                strategy.exit("Short  SL/TP").from("Short").stop(swing_high);
            }
        }
        // A trade's profit as the open trades report it: after the commission of its entry and of closing it at this close.
        let profit: f64 = strategy
            .lots()
            .iter()
            .map(|l| {
                f64::from(l.side) * l.qty * (bar.close - l.price)
                    - l.qty * (l.price + bar.close) * (COMMISSION_PERCENT / 100.0)
            })
            .sum();
        if held > 0.0 && base_cross_short && profit > 0.0 {
            strategy.close("Long");
        }
        if held < 0.0 && base_cross_long && profit > 0.0 {
            strategy.close("Short");
        }
    }
}
