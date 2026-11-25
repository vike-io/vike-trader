// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "SuperTrend Regime Confluence" by DefinedEdge, https://www.tradingview.com/script/mpjNqADq/
// The original's notice: © DefinedEdge
// Changed: rewritten by hand in Rust for vike.

//! A SuperTrend on the bar's midpoint whose multiplier follows a regime: volatile when ATR(10) is over 1.4 times its 40-bar
//! mean (3 widened by 0.4 of the excess), ranging when the rule's ADX(14) is under 20 and that ratio under 0.9 (3 times
//! 0.85), trending otherwise (3), held between 1.5 and 6. When the band flips, more than 5 bars after the last entry, outside a
//! ranging regime, with volume above its 20-bar mean, the close on the flip's side of EMA(50) and a score of at least 65 of 100
//! (volume, distance past the band, the trend, the regime, a distance from the band "one bar back"), it enters that way.
//! The size risks 5% of the account's worth over a stop 6 ATR away, at most 90% of the worth; an opposite position closes
//! first. With it, a stop 6 ATR and a target 15 ATR from the signal close, from the fill. 0.06% commission and 2 ticks of
//! slippage; starts with 10,000.
//!
//! The score is a function the script calls only on a flip, and a value one bar back inside a function is the value at that
//! call's previous run, not the bar before: here the close and band of the last flip scored on the same side, 0 before the
//! first.
//!
//! The rule's ADX averages the moves from the first bar (0 there) and the true range from the second, reads a DI of 0 while
//! the range has no average and a DX of 0 while the DI lines sum to nothing, and averages the DX from the first bar: vike's
//! `Adx` with the first bar's moves and a DX from the first bar (its sums against the rule's means, to their rounding).
//! The one band is vike's SuperTrend carried on the trend (`Supertrend::with_carry_on_trend`), fed this bar's multiplier
//! (`Supertrend::next_centred_with_mult`).
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Supertrend};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::rolling::RollingSum;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const UP: &str = "long";
const DOWN: &str = "short";
const BASE: f64 = 3.0;
const MIN_SCORE: f64 = 65.0;
const COOLDOWN: i64 = 5;
const STOP_ATRS: f64 = 6.0;
const REWARD: f64 = 2.5;
const RISK: f64 = 5.0;

struct S {
    atr: IndicatorLine,
    atr_mean: RollingSum,
    adx: Adx,
    trend: IndicatorLine,
    volume: RollingSum,
    regime: i32,
    line: Supertrend,
    band: f64,
    dir: i32,
    bar: i64,
    last_entry: i64,
    /// The close and the band at each side's previous score, short then long, NaN before its first.
    scored: [(f64, f64); 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        atr: IndicatorLine::new("atr", &[10.0]),
        atr_mean: RollingSum::new(40),
        adx: Adx::new(14.0)
            .expect("a valid length")
            .with_first_bar_moves(true)
            .with_dx_from_first_bar(true),
        trend: IndicatorLine::new("ema", &[50.0]),
        volume: RollingSum::new(20),
        regime: 1,
        line: Supertrend::default().with_carry_on_trend(true),
        band: f64::NAN,
        dir: 1,
        bar: 0,
        last_entry: 0,
        scored: [(f64::NAN, f64::NAN); 2],
    })
}

/// `steps` holds `(at least, points)` from the highest threshold down; under all of them scores `rest`.
fn tier(x: f64, steps: &[(f64, f64)], rest: f64) -> f64 {
    steps.iter().find(|&&(at, _)| x >= at).map_or(rest, |&(_, points)| points)
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.06), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let n = self.bar;
        self.bar += 1;
        let (h, l, c) = (bar.high, bar.low, bar.close);
        let atr = self.atr.next(bar)[0];
        let safe_atr = if atr.is_nan() { 0.001 } else { atr };
        let atr_mean = self.atr_mean.next(atr) / 40.0;
        let ratio = if atr_mean > 0.0 { atr / atr_mean } else { 1.0 };
        let adx = self.adx.next(bar)[0];

        self.regime = if ratio > 1.4 {
            2
        } else if adx < 20.0 && ratio < 0.9 {
            0
        } else {
            1
        };
        let mult = match self.regime {
            2 => BASE * (1.0 + (ratio - 1.0) * 0.4),
            0 => BASE * 0.85,
            _ => BASE,
        };
        let mult = mult.clamp(BASE * 0.5, BASE * 2.0);

        let mid = (h + l) / 2.0;
        let dir_before = self.dir;
        let [band, direction] = self.line.next_centred_with_mult(c, mid, atr, mult);
        // The direction is `NaN` until the ATR exists, while the trend is still the starting up.
        (self.band, self.dir) = (band, if direction < 0.0 { -1 } else { 1 });
        let flip = n > 0 && self.dir != dir_before;

        let trend = self.trend.next(bar)[0];
        let (above, below) = (c > trend, c < trend);
        let volume_mean = self.volume.next(bar.volume) / 20.0;

        if !(flip && n - self.last_entry > COOLDOWN) {
            return;
        }
        let bull = self.dir == 1;
        let ratio_volume = if volume_mean > 0.0 { bar.volume / volume_mean } else { 1.0 };
        let mut score = tier(ratio_volume, &[(2.5, 20.0), (1.5, 14.0), (1.0, 8.0)], 3.0);
        let past = if bull { c - self.band } else { self.band - c };
        let past_atr = if safe_atr > 0.0 { past / safe_atr } else { 0.0 };
        score += if past_atr > 0.0 {
            tier(past_atr, &[(1.5, 25.0), (0.8, 18.0), (0.3, 12.0)], 5.0)
        } else {
            0.0
        };
        let aligned = (bull && above) || (!bull && below);
        let distance = (c - trend).abs() / safe_atr;
        score += if aligned && distance > 0.5 {
            20.0
        } else if aligned {
            14.0
        } else if distance < 0.3 {
            8.0
        } else {
            2.0
        };
        score += match self.regime {
            1 => 15.0,
            2 => 8.0,
            _ => 3.0,
        };
        let (close_then, band_then) =
            std::mem::replace(&mut self.scored[usize::from(bull)], (c, self.band));
        let held_off =
            if band_then.is_nan() { 0.0 } else { (close_then - band_then).abs() / safe_atr };
        score += tier(held_off, &[(2.0, 20.0), (1.0, 14.0), (0.5, 8.0)], 3.0);
        let score = score.round().min(100.0);
        let pass = score >= MIN_SCORE
            && (if bull { above } else { below })
            && self.regime != 0
            && bar.volume > volume_mean;
        if !pass {
            return;
        }

        let stop_dist = atr * STOP_ATRS;
        let worth = strategy.equity();
        let mut q = if stop_dist > 0.0 { worth * RISK / 100.0 / stop_dist } else { f64::NAN };
        if !q.is_nan() && c > 0.0 {
            q = q.min(0.90 * worth / c);
        }
        if q.is_nan() || q <= 0.0 {
            return;
        }
        let q = sizing.floor(q);
        let take = stop_dist * REWARD;
        if bull {
            if strategy.position() < 0.0 {
                strategy.close(DOWN);
            }
            strategy.entry(UP, Long).size(q);
            strategy.exit("long exit").from(UP).take(c + take).stop(c - stop_dist);
        } else {
            if strategy.position() > 0.0 {
                strategy.close(UP);
            }
            strategy.entry(DOWN, Short).size(q);
            strategy.exit("short exit").from(DOWN).take(c - take).stop(c + stop_dist);
        }
        self.last_entry = n;
    }
}
