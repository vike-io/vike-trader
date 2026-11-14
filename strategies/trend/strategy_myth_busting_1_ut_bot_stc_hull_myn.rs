// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Strategy Myth-Busting #1 - UT Bot+STC+Hull+ [MYN]" by myncrypto, https://www.tradingview.com/script/s1ufMqI5/
// The original's notice: © myn
// Changed: rewritten by hand in Rust for vike.

//! Three filters agree: a UT Bot trailing stop on the close, a Schaff trend cycle and a Hull average. Long all of the account's
//! worth, as `worth / close` floored to the step, when the cycle rises under 25, the Hull average is above its value two bars
//! back and the last UT Bot signal was a buy; short it when the cycle falls above 75, the Hull average is not above that
//! value and the last signal was a sell. Neither enters a side already held; each reverses the other. A long whose close is
//! more than 15% under its entry price is closed, a short more than 15% over likewise. Orders fill at the next open. 0.075%
//! commission; starts with 500, kept in USD.
//!
//! The UT Bot stop follows the close at twice the 6-bar ATR, a missing stop read as 0, which makes its first value
//! `close - loss` (vike's `AtrTrailingStop`); a buy is the close crossing up through it, a sell crossing down. The cycle is vike's `Stc` (27, 50, 80): the MACD of EMA(27) and EMA(50), its place in its
//! 80-bar range in percent (kept when the range is empty), halved toward that, and the same once more. The Hull average is
//! `WMA(7)` of `2 * WMA(27) - WMA(55)`, 55 halved down. The rule's four take-profit exits sit at twice the entry price and
//! at zero, which this window never reaches, so they are left out; its Heikin-Ashi switch is off.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{AtrTrailingStop, Hma, Stc};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const STOP_LOSS: f64 = 0.15;

struct S {
    ut: AtrTrailingStop,
    /// The UT Bot stop and source on the bar before.
    ut_last: (f64, f64),
    /// The open times of the last UT Bot buy and sell, 0 before any.
    last_buy: f64,
    last_sell: f64,
    stc: Stc,
    stc_last: f64,
    hma: Hma,
    /// The Hull average one and two bars back.
    hull: [f64; 2],
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        ut: AtrTrailingStop::new(6.0, 2.0).expect("valid parameters"),
        ut_last: (f64::NAN, f64::NAN),
        last_buy: 0.0,
        last_sell: 0.0,
        stc: Stc::new(27.0, 50.0, 80.0).expect("valid lengths"),
        stc_last: f64::NAN,
        hma: Hma::new(55.0).expect("a valid length"),
        hull: [f64::NAN; 2],
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let src = bar.close;
        let stop = self.ut.next(bar);
        let (stop1, source1) = self.ut_last;
        self.ut_last = (stop, src);
        let above = src > stop && source1 <= stop1;
        let below = stop > src && stop1 <= source1;
        if src > stop && above {
            self.last_buy = bar.timestamp as f64;
        }
        if src < stop && below {
            self.last_sell = bar.timestamp as f64;
        }

        let stc = self.stc.next(bar);
        let rising = stc > self.stc_last;
        let falling = stc < self.stc_last;
        self.stc_last = stc;

        let hull = self.hma.next_value(bar.close);
        let green = hull > self.hull[1];
        self.hull = [hull, self.hull[0]];

        let long = rising && stc < 25.0 && green && self.last_buy > self.last_sell;
        let short = falling && stc > 75.0 && !green && self.last_sell > self.last_buy;
        let held = strategy.position();
        let size = sizing.floor(sizing.equity() / bar.close);
        if long && held <= 0.0 {
            strategy.entry("Long", Long).size(size);
        }
        if short && held >= 0.0 {
            strategy.entry("Short", Short).size(size);
        }
        let avg = strategy.avg_cost();
        if held > 0.0 && bar.close < avg * (1.0 - STOP_LOSS) {
            strategy.close("Long");
        }
        if held < 0.0 && bar.close > avg * (1.0 + STOP_LOSS) {
            strategy.close("Short");
        }
    }
}
