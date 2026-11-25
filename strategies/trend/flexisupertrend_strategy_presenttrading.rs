// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "FlexiSuperTrend - Strategy [presentTrading]" by PresentTrading, https://www.tradingview.com/script/y9noW915/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! Twenty Supertrends of the bar's typical price `(high + low + close) / 3` over ATR(10), with factors 0.618, 1.0, 1.382 and
//! so on by 0.382; the median of the twenty distances from the price to each one's line is the oscillator. A second
//! Supertrend, of factor 15 over a 10-bar Wilder average, runs on a bar made from the oscillator (opening at its previous
//! value, closing at this one). Long 10% of the account's worth while that Supertrend is up and the oscillator above 0,
//! short 10% while it is down and the oscillator below 0, each reversing the other; a long closes when it turns down, a
//! short when it turns up. 0.1% commission and one tick of slippage on each fill; starts with 10,000 in a USD account.
//!
//! The oscillator's Supertrends are the reference's own law (`helpers::FlexiMedian`). The signal's is vike's that starts down
//! (`Supertrend::with_down_start`: on the first bar with an ATR the bands the bar before lacked read as 0, as the rule's `nan_to_zero`
//! reads them), fed the made bar from the first that is there (`nan_to_zero` opens it at 0); while it is missing the rule reads down.
//! The direction is `-1` up and `1` down, as the rule writes it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::Supertrend;
use vike_indicators::strategy_kit::FlexiMedian;
use vike_marketdata::Bar;
use vike_model::Commission;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const SHARE: f64 = 10.0;
const SIGNAL_FACTOR: f64 = 15.0;

struct S {
    median: FlexiMedian,
    median_previous: f64,
    signal: Supertrend,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        median: FlexiMedian::default(),
        median_previous: f64::NAN,
        signal: Supertrend::new(10.0, SIGNAL_FACTOR)
            .expect("valid parameters")
            .with_down_start(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let median = self.median.next(bar);
        let open = nan_to_zero(self.median_previous);
        self.median_previous = median;
        let dir = if median.is_nan() {
            1.0
        } else {
            -self.signal.next_hlc(open.max(median), open.min(median), median)[1]
        };
        if dir < 0.0 && median > 0.0 {
            strategy.entry("long", Long).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if dir > 0.0 && median < 0.0 {
            strategy.entry("short", Short).size(share_of_worth(&sizing, SHARE, bar.close));
        }
        if dir > 0.0 {
            strategy.close("long");
        }
        if dir < 0.0 {
            strategy.close("short");
        }
    }
}
