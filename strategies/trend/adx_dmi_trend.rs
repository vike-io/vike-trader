// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "ADX | DMI Trend" by millerrh, https://www.tradingview.com/script/4aQD9gqq/
// The original's notice: © millerrh with inspiration from @9e52f12edd034d28bdd5544e7ff92e
// Changed: rewritten by hand in Rust for vike.

//! Trend entries off ADX(14) and the DI lines, all of the account's worth: long when the ADX is above 23 and higher than 3
//! bars ago, +DI above -DI, the close at or above EMA(200) and the volume above twice its 20-bar SMA; short the mirror (-DI
//! above +DI, the close at or below the EMA). A long closes when the ADX is lower than a bar ago, or the low touches its
//! trailing ATR support or the parabolic SAR; a short when the ADX is lower than a bar ago, or the high touches its trailing
//! ATR resistance or the SAR. The calls go entry long, close long, entry short, close short. The rule's date window covers
//! every bar. 0.04% commission; starts with 10,000 in a USD account.
//!
//! The trailing support is `low - 2 * ATR(14)`, never moving down while the lows stay above it, and reset to this bar's
//! value when the low touches it; the resistance is the mirror on the highs. The SAR is 0.02 / 0.02 / 0.2.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Adx, Atr, Ema, Psar, Sma};
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const KEY_LEVEL: f64 = 23.0;
const ENTRY_LOOKBACK: usize = 3;
const VOLUME_MULTIPLIER: f64 = 2.0;
const ATR_MULTIPLIER: f64 = 2.0;

struct S {
    adx: Adx,
    /// The ADX of this bar and the 3 before it, newest first.
    adx_seen: [f64; ENTRY_LOOKBACK + 1],
    volume: Sma,
    trend: Ema,
    atr: Atr,
    support: f64,
    resistance: f64,
    sar: Psar,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        adx: Adx::new(14.0).expect("a valid length"),
        adx_seen: [f64::NAN; ENTRY_LOOKBACK + 1],
        volume: Sma::new(20.0).expect("a valid length"),
        trend: Ema::new(200.0).expect("a valid length"),
        atr: Atr::new(14.0).expect("a valid length"),
        support: f64::NAN,
        resistance: f64::NAN,
        // The rule's SAR: a reversal tested before the clamp, the trend from the first two closes.
        sar: Psar::new(0.02, 0.2)
            .expect("valid factors")
            .with_start(0.02)
            .expect("a valid start")
            .with_test_first(true)
            .with_direction_from_closes(true)
            .with_textbook_steps(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.04), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let [adx, plus, minus] = self.adx.next(bar);
        self.adx_seen.rotate_right(1);
        self.adx_seen[0] = adx;
        let volume_check = self.volume.next_value(bar.volume) * VOLUME_MULTIPLIER;
        let trend = self.trend.next(bar);

        let reach = self.atr.next(bar) * ATR_MULTIPLIER;
        let (atr_low, atr_high) = (bar.low - reach, bar.high + reach);
        if self.support.is_nan() || atr_low >= self.support {
            self.support = atr_low;
        }
        if self.resistance.is_nan() || atr_high <= self.resistance {
            self.resistance = atr_high;
        }
        let support_hit = bar.low <= self.support;
        let resistance_hit = bar.high >= self.resistance;
        if support_hit {
            self.support = atr_low;
        }
        if resistance_hit {
            self.resistance = atr_high;
        }
        let sar = self.sar.next(bar);

        let rising = adx - self.adx_seen[ENTRY_LOOKBACK] > 0.0;
        let falling = adx - self.adx_seen[1] < 0.0;
        let strong = rising && adx > KEY_LEVEL && bar.volume > volume_check;
        let buy = strong && plus > minus && bar.close >= trend;
        let short = strong && plus < minus && bar.close <= trend;
        let sell = falling || support_hit || bar.low <= sar;
        let cover = falling || resistance_hit || bar.high >= sar;
        if buy {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if sell {
            strategy.close("long");
        }
        if short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if cover {
            strategy.close("short");
        }
    }
}
