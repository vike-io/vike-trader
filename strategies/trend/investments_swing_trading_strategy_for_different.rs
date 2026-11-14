// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Investments/swing trading strategy for different assets" by StrategiesForEveryone, https://www.tradingview.com/script/OePmdMFg/
// The original's notice: Developed by © StrategiesForEveryone
// Changed: rewritten by hand in Rust for vike.

//! A long-only swing rule on a squeeze-momentum value `val` (a 20-bar linear regression of the close less the mean of the 20-bar
//! high-low midpoint and the 20-bar SMA), an EMA(200) and a MACD (16, 26, 8) histogram. Two entries, each sized to risk 2.5% of
//! the account's worth over its stop distance (the ATR(14) times 1.6, capped at the whole account), at this close, only while
//! flat: `Buy` when `val` crosses up through 0, or is over 0 and turns up after a dip, over the EMA; `Buy.` when the histogram is
//! under 0 and rising, the close rises and `val` is under 0, the bar before opened over the EMA and the close is over it. Its
//! stop is the close less the ATR times 1.6 (for `Buy.`, the last close less the last ATR times 1.6). A bar whose high passes the
//! entry's close plus its stop distance moves the stop to the average price. `val` crossing down through 0 closes at the close.
//! 0.05% commission; starts with 100,000.
//!
//! Where both entries come on one bar the first fills and the second is refused by the limit of one entry. The break-even level
//! stays set until a high passes it, as the rule's variable does, also across a flat spell.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Atr, Ema, Macd, TtmSqueeze};
use vike_marketdata::Bar;
use vike_model::OrderSide::Long;
use vike_model::params::Params;
use vike_tactics::{Rule, RuleDefaults, RuleDefaultsFee, Tactics};

const RISK: f64 = 2.5;
const MULTIPLIER: f64 = 1.6;

struct S {
    /// Its momentum line is `val`.
    squeeze: TtmSqueeze,
    ema: Ema,
    macd: Macd,
    atr: Atr,
    /// `val` one and two bars back.
    val_1: f64,
    val_2: f64,
    hist_1: f64,
    close_1: f64,
    open_1: f64,
    atr_1: f64,
    /// The break-even level the rule keeps, NaN once passed.
    be_buy: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let nan = f64::NAN;
    Box::new(S {
        squeeze: TtmSqueeze::default(),
        ema: Ema::new(200.0).expect("a valid length"),
        macd: Macd::new(16.0, 26.0, 8.0).expect("valid lengths"),
        atr: Atr::new(14.0).expect("a valid length"),
        val_1: nan,
        val_2: nan,
        hist_1: nan,
        close_1: nan,
        open_1: nan,
        atr_1: nan,
        be_buy: nan,
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { fee: RuleDefaultsFee::Percent(0.05), ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let val = self.squeeze.next(bar)[0];
        let ema = self.ema.next(bar);
        let [_, _, hist] = self.macd.next(bar);
        let atr = self.atr.next(bar);
        let close = bar.close;

        let long_stop = close - atr * MULTIPLIER;
        let past_stop = self.close_1 - self.atr_1 * MULTIPLIER;
        let contracts = strategy.equity() / close;
        let amount = |stop: f64| {
            let distance = (close - stop) / close;
            let size = contracts * (RISK / 100.0) / distance;
            sizing.floor(if size > contracts { contracts } else { size })
        };

        let (val_1, val_2) = (self.val_1, self.val_2);
        let up = (val > 0.0 && val_1 <= 0.0) || (val > 0.0 && val > val_1 && val_1 < val_2);
        let down = val < 0.0 && val_1 >= 0.0;
        let bullish = close > ema;
        let dip = hist > self.hist_1
            && close > self.close_1
            && hist < 0.0
            && val < 0.0
            && self.open_1 > ema;

        let flat = strategy.position() == 0.0;
        let broke = bar.high > self.be_buy;
        let mut bought = false;
        if up && flat && bullish {
            self.be_buy = close + (close - long_stop);
            strategy.entry("Buy", Long).size(amount(long_stop));
            strategy.exit("Cl Buy").from("Buy").stop(long_stop);
            bought = true;
        }
        if broke {
            Self::break_even(strategy, "Buy", "Cl Buy");
        }
        if down {
            strategy.close("Buy");
        }
        if dip && flat && bullish {
            self.be_buy = close + (close - past_stop);
            if !bought {
                strategy.entry("Buy.", Long).size(amount(past_stop));
                strategy.exit("Cl Buy.").from("Buy.").stop(past_stop);
            }
        }
        if broke {
            Self::break_even(strategy, "Buy.", "Cl Buy.");
        }
        if down {
            strategy.close("Buy.");
        }
        if bar.high > self.be_buy {
            self.be_buy = f64::NAN;
        }

        self.val_2 = val_1;
        self.val_1 = val;
        self.hist_1 = hist;
        self.close_1 = close;
        self.open_1 = bar.open;
        self.atr_1 = atr;
    }
}

impl S {
    /// The exit moved to the entry's average price; with nothing held there is no price and nothing to move.
    fn break_even(strategy: &mut Tactics<'_>, entry: &str, exit: &str) {
        let avg = strategy.avg_cost();
        if !avg.is_nan() {
            strategy.exit(exit).from(entry).stop(avg);
        }
    }
}
