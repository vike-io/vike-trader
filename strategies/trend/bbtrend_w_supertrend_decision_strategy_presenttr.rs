// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "BBTrend w SuperTrend decision - Strategy [presentTrading]" by PresentTrading, https://www.tradingview.com/script/7C1kIEOn/
// The original's notice: © PresentTrading
// Changed: rewritten by hand in Rust for vike.

//! BBTrend: the distance between the lower bands of the 20-bar and 50-bar Bollinger bands of the close (2 deviations), less
//! the distance between their upper bands, in percent of the 20-bar average. A Supertrend of factor 7 over a 10-bar Wilder
//! average runs on a bar made from it (opening at its previous value, closing at this one). Long a tenth of the account's
//! worth while that Supertrend is up, closed at the next open once it turns down. 0.1% commission and one tick of slippage on
//! each fill; starts with 10,000.
//!
//! The Supertrend is vike's that starts down (`Supertrend::with_down_start`: on the first bar with an ATR the bands the bar
//! before lacked read as 0, as the rule's `nan_to_zero` reads them), fed the made bar from the first that is there (`nan_to_zero` opens it at
//! 0); while it is missing the rule reads down. The direction is `-1` up and `1` down, as the rule writes it.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Stddev, Supertrend};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::helpers::nan_to_zero;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const NAME: &str = "Long Entry";
const SHARE: f64 = 10.0;
const MULTIPLIER: f64 = 2.0;
const LENGTH: f64 = 10.0;
const FACTOR: f64 = 7.0;

struct S {
    short_mid: IndicatorLine,
    short_dev: Stddev,
    long_mid: IndicatorLine,
    long_dev: Stddev,
    previous: f64,
    /// The Supertrend on the made bar.
    trend: Supertrend,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        short_mid: IndicatorLine::new("sma", &[20.0]),
        short_dev: Stddev::new(20.0).expect("a valid length"),
        long_mid: IndicatorLine::new("sma", &[50.0]),
        long_dev: Stddev::new(50.0).expect("a valid length"),
        previous: f64::NAN,
        trend: Supertrend::new(LENGTH, FACTOR).expect("valid parameters").with_down_start(true),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.1), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let short_mid = self.short_mid.next(bar)[0];
        let short_dev = MULTIPLIER * self.short_dev.next(bar);
        let long_mid = self.long_mid.next(bar)[0];
        let long_dev = MULTIPLIER * self.long_dev.next(bar);
        let lower = ((short_mid - short_dev) - (long_mid - long_dev)).abs();
        let upper = ((short_mid + short_dev) - (long_mid + long_dev)).abs();
        let bbtrend = (lower - upper) / short_mid * 100.0;
        let open = nan_to_zero(self.previous);
        self.previous = bbtrend;
        let dir = if bbtrend.is_nan() {
            1.0
        } else {
            -self.trend.next_hlc(open.max(bbtrend), open.min(bbtrend), bbtrend)[1]
        };
        if dir < 0.0 {
            strategy.entry(NAME, Long).size(share_of_worth(
                &sizing,
                SHARE,
                sizing.slipped(Long, bar.close),
            ));
        }
        if dir > 0.0 {
            strategy.close(NAME);
        }
    }
}
