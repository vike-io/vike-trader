// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend AI Strategy [Adaptive]" by DefinedEdge, https://www.tradingview.com/script/kZVrTReu/
// The original's notice: © DefinedEdge
// Changed: rewritten by hand in Rust for vike.

//! A SuperTrend on the median price whose ATR(10) multiplier adapts to the regime, 80% of the account's worth. On a flip of
//! the trend, more than 5 bars after the last entry, a long (up flip) or a short (down flip) is taken when its score is at
//! least 65, the close is on its side of EMA(50), the regime is not ranging and the volume is above its 20-bar SMA. An entry
//! closes the other side first and carries an exit: a stop 6 ATR(10) from this close and a target 2.5 times as far the other
//! way, issued again on every entry signal. 0.06% commission, 2 ticks of slippage; starts with 10,000.
//!
//! The regime: volatile when ATR(10) is over 1.4 times its 40-bar SMA, ranging when ADX(14) is under 20 and that ratio under
//! 0.9, trending otherwise. The multiplier is 3, scaled by `1 + 0.4 * (ratio - 1)` when volatile and by 0.85 when ranging,
//! held within 1.5 to 6. The ADX is the rule's own: Wilder averages (SMA-seeded) of the moves and the true range, a DI of 0
//! while the range average has none, and a Wilder average of the DX, 0 while the DI sum is not positive, from the first bar
//! (`Adx::with_first_bar_moves`, `Adx::with_dx_from_first_bar`). The score adds five parts: volume against its average, the
//! close's distance past the new band, alignment with the EMA, the regime, and the "last" close's distance from the "last"
//! band, each in ATRs, rounded and capped at 100. The one band is vike's SuperTrend carried on the trend
//! (`Supertrend::with_carry_on_trend`), fed this bar's multiplier (`Supertrend::next_centred_with_mult`).
//!
//! That last part reads `close[1]` and `band[1]` inside the scoring function, which runs only on a flip past the cooldown.
//! The script keeps that history per call (one for the long score, one for the short), so `[1]` is the close and band of the
//! call before, none on the first.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Sma, Supertrend};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_indicators::strategy_kit::{max, min};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const BASE_MULTIPLIER: f64 = 3.0;
const MIN_SCORE: i64 = 65;
const COOLDOWN: usize = 5;
const STOP_ATRS: f64 = 6.0;
const REWARD: f64 = 2.5;
const LONG_NAME: &str = "Long";
const SHORT_NAME: &str = "Short";

struct S {
    atr: IndicatorLine,
    atr_mean: RollingSum,
    dmi: Adx,
    line: Supertrend,
    band: f64,
    up: bool,
    trend: IndicatorLine,
    volume: Sma,
    bars: usize,
    last_entry: usize,
    /// The close and band at the scoring function's last call, short score first: its `[1]`.
    called: [(f64, f64); 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[10.0]),
        atr_mean: RollingSum::new(40),
        dmi: Adx::new(14.0)
            .expect("a valid length")
            .with_first_bar_moves(true)
            .with_dx_from_first_bar(true),
        line: Supertrend::default().with_carry_on_trend(true),
        band: f64::NAN,
        up: true,
        trend: IndicatorLine::new("ema", &[50.0]),
        volume: Sma::new(20.0).expect("a valid length"),
        bars: 0,
        last_entry: 0,
        called: [(f64::NAN, f64::NAN); 2],
    })
}

/// The score's step for a value: the first threshold it reaches, from the top.
fn step(value: f64, steps: &[(f64, f64)], otherwise: f64) -> f64 {
    steps.iter().find(|(at, _)| value >= *at).map_or(otherwise, |(_, points)| *points)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let n = self.bars;
        self.bars += 1;
        let atr = self.atr.next(bar)[0];
        let safe_atr = if atr.is_nan() { 0.001 } else { atr };
        let atr_mean = self.atr_mean.next(atr) / 40.0;
        let ratio = if atr_mean > 0.0 { atr / atr_mean } else { 1.0 };

        let [adx, _, _] = self.dmi.next(bar);

        let regime = if ratio > 1.4 {
            2
        } else if adx < 20.0 && ratio < 0.9 {
            0
        } else {
            1
        };
        let mult = match regime {
            2 => BASE_MULTIPLIER * (1.0 + (ratio - 1.0) * 0.4),
            0 => BASE_MULTIPLIER * 0.85,
            _ => BASE_MULTIPLIER,
        };
        let mult = max(min(mult, BASE_MULTIPLIER * 2.0), BASE_MULTIPLIER * 0.5);

        let source = (bar.high + bar.low) / 2.0;
        let was_up = self.up;
        let [band, direction] = self.line.next_centred_with_mult(bar.close, source, atr, mult);
        // The direction is `NaN` until the ATR exists, while the trend is still the starting up.
        (self.band, self.up) = (band, direction.is_nan() || direction > 0.0);
        let flip = self.up != was_up;

        let trend = self.trend.next(bar)[0];
        let trend_up = bar.close > trend;
        let trend_down = bar.close < trend;
        let volume_mean = self.volume.next_value(bar.volume);

        let score = |bull: bool, called_close: f64, called_band: f64| {
            let volume_ratio = if volume_mean > 0.0 { bar.volume / volume_mean } else { 1.0 };
            let mut score = step(volume_ratio, &[(2.5, 20.0), (1.5, 14.0), (1.0, 8.0)], 3.0);
            let past = if bull { bar.close - self.band } else { self.band - bar.close };
            let past = if safe_atr > 0.0 { past / safe_atr } else { 0.0 };
            let just_past = if past > 0.0 { 5.0 } else { 0.0 };
            score += step(past, &[(1.5, 25.0), (0.8, 18.0), (0.3, 12.0)], just_past);
            let aligned = (bull && trend_up) || (!bull && trend_down);
            let distance = (bar.close - trend).abs() / safe_atr;
            score += if aligned && distance > 0.5 {
                20.0
            } else if aligned {
                14.0
            } else if distance < 0.3 {
                8.0
            } else {
                2.0
            };
            score += match regime {
                1 => 15.0,
                2 => 8.0,
                _ => 3.0,
            };
            let before = if called_band.is_nan() {
                0.0
            } else {
                (called_close - called_band).abs() / safe_atr
            };
            score += step(before, &[(2.0, 20.0), (1.0, 14.0), (0.5, 8.0)], 3.0);
            score.round().min(100.0) as i64
        };

        let cooled = n - self.last_entry > COOLDOWN;
        let mut long = false;
        let mut short = false;
        if flip && cooled {
            let site = usize::from(self.up);
            let (called_close, called_band) = self.called[site];
            let passed = score(self.up, called_close, called_band) >= MIN_SCORE
                && if self.up { trend_up } else { trend_down }
                && regime != 0
                && bar.volume > volume_mean;
            self.called[site] = (bar.close, self.band);
            long = passed && self.up;
            short = passed && !self.up;
        }

        let stop = atr * STOP_ATRS;
        let reach = stop * REWARD;
        if long {
            if strategy.position() < 0.0 {
                strategy.close(SHORT_NAME);
            }
            let price = sizing.slipped(Long, bar.close);
            strategy.entry(LONG_NAME, Long).size(share_of_worth(&sizing, 80.0, price));
            strategy
                .exit("Long Exit")
                .from(LONG_NAME)
                .take(bar.close + reach)
                .stop(bar.close - stop);
            self.last_entry = n;
        }
        if short {
            if strategy.position() > 0.0 {
                strategy.close(LONG_NAME);
            }
            let price = sizing.slipped(Short, bar.close);
            strategy.entry(SHORT_NAME, Short).size(share_of_worth(&sizing, 80.0, price));
            strategy
                .exit("Short Exit")
                .from(SHORT_NAME)
                .take(bar.close - reach)
                .stop(bar.close + stop);
            self.last_entry = n;
        }
    }
}
