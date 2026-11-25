// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Kahlman HullMA / WT Cross Strategy" by karakedix, https://www.tradingview.com/script/1Im3kKwi/
// The original's notice: Kahlman HullMA / WT Cross Strategy ©author [pigsq]
// Changed: rewritten by hand in Rust for vike.

//! Two Kalman-smoothed Hull averages (`a` of the bar's midpoint, `b` of a three-WMA blend of the close) and two wave-trend lines,
//! one tuned for longs (6, 23) and one for shorts (8, 19), with a stochastic RSI and a volume filter.
//!
//! When not short, long all of the account when `b` crosses up through `a` while the long wave trend crosses up (now, a bar ago,
//! or its value two bars back against its signal now), the stochastic RSI's %K is over its %D and the volume beats 0.6 of the
//! mean of its SMA, VWMA and WMA; not on a Saturday. When not long, short all of it on the mirror cross with the short wave trend
//! crossing down and %K under %D, with the same volume test. Neither ever reverses the other side.
//!
//! The four exits carry no entry name, so each applies to whatever is held, each claiming its share of the position from what the
//! ones issued before it left. The pair issued after the side's entry on its bar claims the whole position, so the other pair
//! gets nothing. A long's pair: a tenth at an 8.9% target, the rest at a 13.6% target, both behind one stop 7.1% under the entry
//! that trails 3.6% under the close, never down, while this bar's or the last bar's high reaches the first target. A short's
//! pair: half at an 8.2% target, the rest at a 13.4% target, both behind a stop 7.9% over the entry. A long also closes at the next open
//! on the first target touched with the bar's high-close midpoint back under it, the same for the second target, or the second
//! target touched while `a` is over `b`, and on the short signal or a position check while the bar's high is at or under the
//! entry or at or over 5.2% above it. A short closes on the mirror of the target tests and on the long signal while its low is
//! at or over the entry or at or under 5.9% below it. No costs; starts with 100.
//!
//! The rule's ADX filter is off and only asks the ADX to be defined, which it is from the 28th bar, before any of the rest.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_indicators::strategy_kit::max;
use vike_tactics::helpers::{known, weekday};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{GhFilter, Hma, Rsi, Sma, Stochastic, Wavetrend, Wma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const UP: &str = "LONG";
const DOWN: &str = "SHORT";
const LONG_FIRST_SHARE: f64 = 0.10;
const SHORT_FIRST_SHARE: f64 = 0.50;
const LONG_TP1: f64 = 0.089;
const LONG_TP2: f64 = 0.136;
const SHORT_TP1: f64 = 0.082;
const SHORT_TP2: f64 = 0.134;
const LONG_SL: f64 = 0.071;
const SHORT_SL: f64 = 0.079;
const TRAIL: f64 = 0.036;
const LONG_CHECK: f64 = 1.7;
const SHORT_CHECK: f64 = 1.4;
const VOLUME_MULTIPLIER: f64 = 0.6;
const SATURDAY: i64 = 5;

/// An average fed only from its source's first number on.
fn fed(x: f64, mut f: impl FnMut(f64) -> f64) -> f64 {
    if x.is_nan() { f64::NAN } else { f(x) }
}

/// The rule's Kalman step with gain 1 (`kf + d * sqrt(2 * gain) + v`, `v` its running sum of `gain * d`): vike's g-h
/// filter with `g = sqrt(2)` and `h = 1`, which also restarts from the input after a NaN.
fn kalman() -> GhFilter {
    GhFilter::new(std::f64::consts::SQRT_2, 1.0).expect("valid gains")
}

/// vike's `Wavetrend` (`n1`, `n2`) and its last four `(wt1, wt2)`, newest first.
struct WaveTrend {
    wave: Wavetrend,
    seen: VecDeque<(f64, f64)>,
}

impl WaveTrend {
    fn new(n1: f64, n2: f64) -> Self {
        WaveTrend {
            wave: Wavetrend::new(n1, n2).expect("valid lengths"),
            seen: VecDeque::with_capacity(5),
        }
    }

    fn next(&mut self, x: f64) {
        let [wt1, wt2] = self.wave.next_value(x);
        self.seen.push_front((wt1, wt2));
        self.seen.truncate(4);
    }

    fn at(&self, i: usize) -> (f64, f64) {
        self.seen.get(i).copied().unwrap_or((f64::NAN, f64::NAN))
    }

    /// `crossover(wt1[i], wt2[j])`.
    fn up(&self, i: usize, j: usize) -> bool {
        self.at(i).0 > self.at(j).1 && self.at(i + 1).0 <= self.at(j + 1).1
    }

    fn down(&self) -> bool {
        self.at(0).0 < self.at(0).1 && self.at(1).0 >= self.at(1).1
    }

    fn below(&self) -> bool {
        self.at(0).0 < self.at(0).1
    }
}

struct S {
    /// The rule's Hull average of 24, its last length `round(sqrt(24))` = 5.
    hull: Hma,
    blend_third: Wma,
    blend_half: Wma,
    blend_full: Wma,
    blend_out: Wma,
    kalman_a: GhFilter,
    kalman_b: GhFilter,
    previous_ab: (f64, f64),
    wt_long: WaveTrend,
    wt_short: WaveTrend,
    rsi: Rsi,
    stoch: Stochastic,
    volume_sma: Sma,
    volume_sq_sma: Sma,
    volume_wma: Wma,
    previous_high: f64,
    previous_stop: f64,
    /// The size of the entry placed from flat: the exits' shares are of it.
    entry_quantity: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let wma = |n: f64| Wma::new(n).expect("a valid length");
    let sma = |n: f64| Sma::new(n).expect("a valid length");
    Box::new(S {
        hull: Hma::new(24.0).expect("a valid length").with_rounded_root(true),
        blend_third: wma(4.0),
        blend_half: wma(6.0),
        blend_full: wma(12.0),
        blend_out: wma(12.0),
        kalman_a: kalman(),
        kalman_b: kalman(),
        previous_ab: (f64::NAN, f64::NAN),
        wt_long: WaveTrend::new(6.0, 23.0),
        wt_short: WaveTrend::new(8.0, 19.0),
        rsi: Rsi::new(13.0).expect("a valid length"),
        stoch: Stochastic::new(13.0, 2.0, 3.0).expect("valid lengths"),
        volume_sma: sma(3.0),
        volume_sq_sma: sma(3.0),
        volume_wma: wma(4.0),
        previous_high: f64::NAN,
        previous_stop: f64::NAN,
        entry_quantity: 0.0,
    })
}

impl S {
    /// One side's two exits, as the rule issues them on every bar: they name no entry, so each applies to every entry held or
    /// placed (`UP` and `DOWN`; the order language ignores the one that does not exist), and the first takes `share` of the
    /// entry. The order language hands the entry's quantity out in the order they are issued, an exit with no level holding its
    /// share too.
    fn exits(
        &self,
        strategy: &mut Tactics<'_>,
        names: [&str; 2],
        share: f64,
        cut: Option<f64>,
        targets: [f64; 2],
    ) {
        let sizing = strategy.sizing();
        let first = sizing.floor(self.entry_quantity * share);
        for lots in [UP, DOWN] {
            for (i, (name, target)) in names.into_iter().zip(targets).enumerate() {
                let exit = strategy.exit(name).from(lots);
                if i == 0 {
                    exit.size(first);
                }
                if let Some(target) = known(target) {
                    exit.take(target);
                }
                if let Some(cut) = cut {
                    exit.stop(cut);
                }
            }
        }
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let mid = (bar.high + bar.low) / 2.0;
        let typical = (bar.high + bar.low + bar.close) / 3.0;
        let hull = self.hull.next_value(mid);
        let blend = self.blend_third.next_value(bar.close) * 3.0
            - self.blend_half.next_value(bar.close)
            - self.blend_full.next_value(bar.close);
        let blend = fed(blend, |v| self.blend_out.next_value(v));
        let a = self.kalman_a.next_value(hull);
        let b = self.kalman_b.next_value(blend);
        let (previous_a, previous_b) = self.previous_ab;
        self.previous_ab = (a, b);
        let cross_down = a > b && previous_a < previous_b;
        let cross_up = b > a && previous_b < previous_a;

        self.wt_long.next(typical);
        self.wt_short.next(typical);
        let rsi = self.rsi.next_value(mid);
        let [k, d] = if rsi.is_nan() { [f64::NAN; 2] } else { self.stoch.next_hlc(rsi, rsi, rsi) };
        let volume_mean = self.volume_sma.next_value(bar.volume);
        let volume_weighted = self.volume_sq_sma.next_value(bar.volume * bar.volume) / volume_mean;
        let volume_line =
            (volume_mean + volume_weighted + self.volume_wma.next_value(bar.volume)) / 3.0;
        let volume_ok = bar.volume > volume_line * VOLUME_MULTIPLIER;

        let held = strategy.position();
        let cost = strategy.avg_cost();
        let long_side = held >= 0.0;
        let short_side = held <= 0.0;
        let pick = |long: f64, short: f64| cost * if long_side { 1.0 + long } else { 1.0 - short };
        let stop_level = if long_side { cost * (1.0 - LONG_SL) } else { cost * (1.0 + SHORT_SL) };
        let target1 = pick(LONG_TP1, SHORT_TP1);
        let target2 = pick(LONG_TP2, SHORT_TP2);
        let check_long = if long_side { cost * (1.0 + LONG_TP1 / LONG_CHECK) } else { f64::NAN };
        let check_short =
            if short_side { cost * (1.0 - SHORT_TP1 / SHORT_CHECK) } else { f64::NAN };

        let wl = &self.wt_long;
        let ws = &self.wt_short;
        let long_signal = cross_up && k > d && (wl.up(0, 0) || wl.up(1, 1) || wl.up(2, 0));
        let short_signal = cross_down && ws.down() && k < d;

        let trailing = bar.high >= target1 || self.previous_high >= target1;
        self.previous_high = bar.high;
        let long_stop = if long_side && trailing {
            max(bar.close * (1.0 - TRAIL), self.previous_stop)
        } else if long_side {
            stop_level
        } else {
            f64::NAN
        };
        self.previous_stop = long_stop;

        let open_close = (bar.open + bar.close) / 2.0;
        let check_long_hit = bar.high >= check_long
            && open_close < check_long
            && (wl.down()
                || ws.down()
                || (a > b && !volume_ok && (wl.below() || ws.below()))
                || cross_down);
        let high_close = (bar.high + bar.close) / 2.0;
        let long_target_exit = (bar.high >= target1 && high_close <= target1)
            || (bar.high >= target2 && high_close <= target2)
            || (bar.high >= target2 && a > b);
        let low_close = (bar.low + bar.close) / 2.0;
        let low_close_open = (bar.low + bar.close + bar.open) / 3.0;
        let short_target_exit = (bar.low <= target1 && low_close >= target1)
            || (bar.low <= target2 && low_close_open >= target2)
            || (bar.low <= target2 && a < b);
        let long_exit =
            (bar.high <= cost || bar.high >= check_long) && (short_signal || check_long_hit);
        // The short's position check asks the low to be both at or under its line and over it, so it never holds.
        let short_exit = (bar.low >= cost || bar.low <= check_short) && long_signal;

        let saturday = weekday(bar.timestamp) == SATURDAY;
        let entry = || share_of_worth(&sizing, 100.0, bar.close);
        if long_side && long_signal && volume_ok && !saturday {
            let qty = entry();
            if held == 0.0 {
                self.entry_quantity = qty;
            }
            strategy.entry(UP, Long).size(qty);
        }
        // The long pair is issued after the long entry and the short pair after the short one, in the rule's order: the pair
        // issued after an entry takes all of it, so the other side's pair gets nothing, and a pair issued before the entry
        // exists binds to nothing.
        self.exits(
            strategy,
            ["LONG", "LONG1"],
            LONG_FIRST_SHARE,
            known(long_stop),
            [target1, target2],
        );
        if long_exit || long_target_exit {
            strategy.close(UP);
        }
        if short_side && short_signal && volume_ok {
            let qty = entry();
            if held == 0.0 {
                self.entry_quantity = qty;
            }
            strategy.entry(DOWN, Short).size(qty);
        }
        let short_cut = if held <= 0.0 { known(stop_level) } else { None };
        self.exits(strategy, ["SHORT", "SHORT1"], SHORT_FIRST_SHARE, short_cut, [target1, target2]);
        if short_exit || short_target_exit {
            strategy.close(DOWN);
        }
    }
}
