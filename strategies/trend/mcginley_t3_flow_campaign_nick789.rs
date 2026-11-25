// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "McGinley T3 Flow Campaign [NICK789]" by NICK789, https://www.tradingview.com/script/Hrc7Gfqe/
// The original's notice: © NICK789
// Changed: rewritten by hand in Rust for vike.

//! A trail around the McGinley line of the close (length 65, vike's `Mcginley`), never more than 1.2 ATR(28) from it. From 2025-12-31
//! 13:00 UTC on: when the trail crosses up through its value 2 bars back, close any short and go long 30% of the account's worth;
//! the mirror for a short. Each entry sets three targets 1, 2 and 3 steps past its signal close, a step being the distance of
//! the close from the trail but at least a quarter of ATR(200). While held, the first bar whose high is over a long's target
//! (low under a short's) closes 30%, 30% and 40% of what is held for targets 1, 2 and 3, each once; reaching target 3 drops any
//! target still open. Orders fill at this bar's close. 0.05% commission; starts with 100,000.
//!
//! The reference gives each close statement of a version-6 script its own order, so targets reached on one bar are separate
//! trades, each sized from the position the bar began with, floored to the quantity step but never under one step. Here each is
//! a free limit at this close under its own name, as closes of one name replace each other. The rule's stop is drawn only.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! fill = "close"
//!
//! [run]
//! bound_exits = true
//! stacked_closes = true
//! ```

use vike_indicators::indicators::Mcginley;
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::{floor_share, share_of_worth};
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "Long";
const SHORT_NAME: &str = "Short";
/// 2026-01-01 00:00 at UTC+11.
const TRADE_FROM_MS: i64 = 1_767_186_000_000;
const LENGTH: f64 = 65.0;
const TRAIL_ATRS: f64 = 1.2;
const SHARE: f64 = 30.0;
/// The share of the position each target closes, in percent.
const TARGET_SHARES: [f64; 3] = [30.0, 30.0, 40.0];

struct S {
    mcginley: Mcginley,
    atr: IndicatorLine,
    atr_base: IndicatorLine,
    /// The trail now and on the three bars before.
    trail: [f64; 4],
    up: Cross,
    long_targets: [f64; 3],
    short_targets: [f64; 3],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        mcginley: Mcginley::new(LENGTH).expect("a valid length"),
        atr: IndicatorLine::new("atr", &[28.0]),
        atr_base: IndicatorLine::new("atr", &[200.0]),
        trail: [f64::NAN; 4],
        up: Cross::new(),
        long_targets: [f64::NAN; 3],
        short_targets: [f64::NAN; 3],
    })
}

impl S {
    /// Closes `share` percent of the position the bar began with, as its own order at this close. The rule states the share as
    /// `share / 100 * 100` and the reference takes `share * 0.01` of it; a size within a few ulps of a whole step keeps it.
    fn take(strategy: &mut Tactics<'_>, lots: &str, held: f64, share: f64) {
        let sizing = strategy.sizing();
        strategy.close(lots).size(floor_share(
            held * (share / 100.0 * 100.0 * 0.01),
            held,
            sizing.qty_step(),
        ));
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let basis = self.mcginley.next(bar);
        let distance = self.atr.next(bar)[0] * TRAIL_ATRS;
        let atr_base = self.atr_base.next(bar)[0];
        let mut trail = if self.trail[0].is_nan() { basis } else { self.trail[0] };
        if basis - distance > trail {
            trail = basis - distance;
        }
        if basis + distance < trail {
            trail = basis + distance;
        }
        self.trail = [trail, self.trail[0], self.trail[1], self.trail[2]];
        // Crossing the value 2 bars back: now over it, last bar at or under the one 2 bars before that.
        let signal = self.up.step(trail - self.trail[2], 0.0);
        let (long, short) = (matches!(signal, Signal::Long), matches!(signal, Signal::Short));
        if bar.timestamp < TRADE_FROM_MS {
            return;
        }
        let held = strategy.position();
        // f64's `max` would drop a NaN the rule keeps.
        let step = if atr_base.is_nan() {
            f64::NAN
        } else {
            (bar.close - trail).abs().max(atr_base * 0.25)
        };
        if long && held <= 0.0 {
            strategy.close(SHORT_NAME);
            strategy.entry(LONG_NAME, Long).size(share_of_worth(&sizing, SHARE, bar.close));
            self.long_targets = [1.0, 2.0, 3.0].map(|k| bar.close + step * k);
            self.short_targets = [f64::NAN; 3];
        }
        if short && held >= 0.0 {
            strategy.close(LONG_NAME);
            strategy.entry(SHORT_NAME, Short).size(share_of_worth(&sizing, SHARE, bar.close));
            self.short_targets = [1.0, 2.0, 3.0].map(|k| bar.close - step * k);
            self.long_targets = [f64::NAN; 3];
        }
        if held > 0.0 {
            for (target, &share) in self.long_targets.iter_mut().zip(&TARGET_SHARES) {
                if bar.high > *target {
                    Self::take(strategy, LONG_NAME, held, share);
                    *target = f64::NAN;
                }
            }
            if self.long_targets[2].is_nan() {
                self.long_targets = [f64::NAN; 3];
            }
        }
        if held < 0.0 {
            for (target, &share) in self.short_targets.iter_mut().zip(&TARGET_SHARES) {
                if bar.low < *target {
                    Self::take(strategy, SHORT_NAME, -held, share);
                    *target = f64::NAN;
                }
            }
            if self.short_targets[2].is_nan() {
                self.short_targets = [f64::NAN; 3];
            }
        }
    }
}
