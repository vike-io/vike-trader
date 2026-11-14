// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "MACD+RSI" by Drun30, https://www.tradingview.com/script/b7zn25L6/
// The original's notice: © Drun30
// Changed: rewritten by hand in Rust for vike.

//! The MACD line of EMA(12) and EMA(26) of the close, smoothed twice: a triple Hull average of 36 bars (three times HMA(36)
//! less the HMA of the HMA of it), then the 10-bar regression line read one bar back. Long all of the account's worth when
//! not long, the line turns up and RSI(14) is under 90; short when not short, the line turns down and RSI(14) is over 44. A
//! long closes when the line falls or RSI is over 90, a short when the line rises or RSI is under 44, and each close goes
//! ahead of the entry the other way. 0.075% commission; starts with 100.
//!
//! "Turns up" is a rise after a bar that did not rise, "turns down" a fall after a bar that did not fall. The regression
//! line one bar back on the same fit is its value now less its slope.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Hma, Linearreg, LinearregSlope, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const OVERBOUGHT: f64 = 90.0;
const OVERSOLD: f64 = 44.0;
const HULL: f64 = 36.0;
const REGRESSION: f64 = 10.0;

struct S {
    fast: Ema,
    slow: Ema,
    hull: Hma,
    hull_of_hull: [Hma; 2],
    line: Linearreg,
    slope: LinearregSlope,
    rsi: Rsi,
    last_line: f64,
    last_change: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let hull = || Hma::new(HULL).expect("a valid length");
    Box::new(S {
        fast: Ema::new(12.0).expect("a valid length"),
        slow: Ema::new(26.0).expect("a valid length"),
        hull: hull(),
        hull_of_hull: [hull(), hull()],
        line: Linearreg::new(REGRESSION).expect("a valid length"),
        slope: LinearregSlope::new(REGRESSION).expect("a valid length"),
        rsi: Rsi::new(14.0).expect("a valid length"),
        last_line: f64::NAN,
        last_change: f64::NAN,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.075), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let macd = self.fast.next(bar) - self.slow.next(bar);
        let hull = self.hull.next_value(macd);
        let twice = self.hull_of_hull[0].next_value(hull);
        let thrice = self.hull_of_hull[1].next_value(twice);
        let smooth = 3.0 * hull - thrice;
        let line = self.line.next_value(smooth) - self.slope.next_value(smooth);
        let change = line - self.last_line;
        let last_change = self.last_change;
        self.last_line = line;
        self.last_change = change;
        let rsi = self.rsi.next(bar);

        let rising = change > 0.0;
        let falling = change < 0.0;
        let close_short = rsi < OVERSOLD || rising;
        let close_long = rsi > OVERBOUGHT || falling;
        let open_long = rising && last_change <= 0.0 && rsi < OVERBOUGHT;
        let open_short = falling && last_change >= 0.0 && rsi > OVERSOLD;
        let held = strategy.position();
        if close_short {
            strategy.close("short");
        }
        if held <= 0.0 && open_long {
            strategy.entry("long", Long).size(share_of_worth(&sizing, 100.0, bar.close));
        }
        if close_long {
            strategy.close("long");
        }
        if held >= 0.0 && open_short {
            strategy.entry("short", Short).size(share_of_worth(&sizing, 100.0, bar.close));
        }
    }
}
