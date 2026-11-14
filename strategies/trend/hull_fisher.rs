// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Hull Fisher" by SeaSide420, https://www.tradingview.com/script/QTYaYuX7/
// The original's notice: © @SeaSide420
// Changed: rewritten by hand in Rust for vike.
// The original states no licence: published on 2020-11-10, it is MPL-2.0 by the Terms of Use of the site that publishes it.

//! A Fisher transform of HMA(14) of the open, filtered by that HMA and by CCI(14) of the open. The transform: `v` is the HMA's
//! place in its 9-bar range, `0.66 * (place - 0.5) + 0.67 * v` of the bar before, held inside ±0.999; the line is
//! `0.5 * ln((1 + v) / max(1 - v, 0.001)) + 0.5 *` its value on the bar before. Long all of the account's worth when the line,
//! the HMA and the CCI all rose on this bar and the line is above 0 or below -5; short all of it when all three fell and the
//! line is below 0 or above 5. A long is closed when the line or the HMA fell, a short when either rose, before the entries
//! of the same bar. 0.25% commission; starts with 100,000 in a USD account.
//!
//! The 9-bar range is at least 0.001 wide; a value before the first is 0, in both recurrences.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Cci, Donchian, Hma};
use vike_indicators::strategy_kit::max;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

struct S {
    hma: Hma,
    range: Donchian,
    cci: Cci,
    last_hma: f64,
    last_cci: f64,
    value: f64,
    line: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        hma: Hma::new(14.0).expect("a valid length"),
        range: Donchian::new(9.0).expect("a valid length"),
        cci: Cci::new(14.0).expect("a valid length"),
        last_hma: f64::NAN,
        last_cci: f64::NAN,
        value: f64::NAN,
        line: f64::NAN,
    })
}

/// The rule's clamp: above 0.99 is 0.999, below -0.99 is -0.999.
fn held_in(v: f64) -> f64 {
    if v > 0.99 {
        0.999
    } else if v < -0.99 {
        -0.999
    } else {
        v
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.25), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let hma = self.hma.next_value(bar.open);
        let [high, _, low] = self.range.next_hl(hma, hma);
        self.value = held_in(
            0.66 * ((hma - low) / max(high - low, 0.001) - 0.5) + 0.67 * nan_to_zero(self.value),
        );
        let last_line = self.line;
        self.line = 0.5 * libm::log((1.0 + self.value) / max(1.0 - self.value, 0.001))
            + 0.5 * nan_to_zero(last_line);
        let cci = self.cci.next_hlc(bar.open, bar.open, bar.open);
        let last_hma = std::mem::replace(&mut self.last_hma, hma);
        let last_cci = std::mem::replace(&mut self.last_cci, cci);
        let line = self.line;
        if hma < last_hma || line < last_line {
            strategy.close("long");
        }
        if hma > last_hma || line > last_line {
            strategy.close("short");
        }
        // A NaN line fails the first comparison, so the range test never meets one.
        if line > last_line && !(-5.0..=0.0).contains(&line) && cci > last_cci && hma > last_hma {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if line < last_line && !(0.0..=5.0).contains(&line) && cci < last_cci && hma < last_hma {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
