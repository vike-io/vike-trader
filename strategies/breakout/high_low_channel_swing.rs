// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "High/Low channel swing" by exlux, https://www.tradingview.com/script/UXKktYt6/
// The original's notice: © exlux99
// Changed: rewritten by hand in Rust for vike.

//! A channel from two means of seven 12-bar averages (SMA, EMA, WMA, ALMA 0.85 / 6, RMA, linear regression, VWMA), one of the
//! highs and one of the lows. Long 1 unit when the close is over the upper line, short 1 when it is under the lower, each
//! reversing the other, only on the bars from 2000-01-01 to 2021-12-31 (the rule's window). A long closes when the low crosses
//! under the lower line, a short when the high crosses over the upper. Every bar of the window each held entry's exit is asked
//! for again: a target of 6% (long) or 4.5% (short) and a stop of 5% of that bar's close, from the entry's price; an entry
//! still to fill gets them from its fill on, with the distances of its signal bar. After the window the last exits rest. No
//! costs; starts with 100,000. **Does not reproduce its reference** (1342 trades, net 33429.488281 on BTCUSDT 4h,
//! 2017-08-17..2026-10-01): this file gives 1343 / 34164.870000, one trade more and the net 735.38 over, the same with its
//! exits unbound, no cause found (vike's ALMA does not floor its offset; floored to a whole bar, as the docs of the
//! reference's function read, it gives 1348 / 28329.27); the owner accepted those numbers on 2026-11-15.
//!
//! The plots are left out.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Alma, Ema, Linearreg, Sma, Smma, Vwma, Wma};
use vike_marketdata::Bar;
use vike_model::OrderSide::{self, Long, Short};
use vike_model::params::Params;
use vike_tactics::calendar::timestamp;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LENGTH: f64 = 12.0;
/// `(target, stop)` as a share of the close, a long's and a short's.
const LONG_EXIT: (f64, f64) = (0.06, 0.05);
const SHORT_EXIT: (f64, f64) = (0.045, 0.05);

/// The seven averages of one price, and their mean.
struct Seven {
    sma: Sma,
    ema: Ema,
    wma: Wma,
    alma: Alma,
    rma: Smma,
    line: Linearreg,
    vwma: Vwma,
}

impl Seven {
    fn new() -> Self {
        Seven {
            sma: Sma::new(LENGTH).expect("a valid length"),
            ema: Ema::new(LENGTH).expect("a valid length"),
            wma: Wma::new(LENGTH).expect("a valid length"),
            alma: Alma::new(LENGTH, 0.85, 6.0).expect("valid parameters"),
            rma: Smma::new(LENGTH).expect("a valid length"),
            line: Linearreg::new(LENGTH).expect("a valid length"),
            vwma: Vwma::new(LENGTH).expect("a valid length"),
        }
    }

    fn next(&mut self, price: f64, volume: f64) -> f64 {
        (self.sma.next_value(price)
            + self.ema.next_value(price)
            + self.wma.next_value(price)
            + self.alma.next_value(price)
            + self.rma.next_value(price)
            + self.line.next_value(price)
            + self.vwma.next_cv(price, volume))
            / 7.0
    }
}

struct S {
    highs: Seven,
    lows: Seven,
    /// The low and the lower line, the high and the upper line of the bar before.
    last: [f64; 4],
    /// The close of the bar that placed an entry still to fill, whose exits it takes from its fill.
    coming: Option<f64>,
    from: i64,
    to: i64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        highs: Seven::new(),
        lows: Seven::new(),
        last: [f64::NAN; 4],
        coming: None,
        from: timestamp(2000, 1, 1, 0, 0),
        to: timestamp(2021, 12, 31, 0, 0),
    })
}

/// The exit of the entry `name` on `side`, filled at `price`, from a close of `close`.
fn protect(strategy: &mut Tactics<'_>, name: &str, side: OrderSide, price: f64, close: f64) {
    let ((take, cut), exit) =
        if side == Long { (LONG_EXIT, "closelong") } else { (SHORT_EXIT, "closeshort") };
    let up = f64::from(side);
    strategy.exit(exit).from(name).take(price + up * close * take).stop(price - up * close * cut);
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.coming = None;
        let upper = self.highs.next(bar.high, bar.volume);
        let lower = self.lows.next(bar.low, bar.volume);
        let [low_before, lower_before, high_before, upper_before] = self.last;
        self.last = [bar.low, lower, bar.high, upper];
        if bar.timestamp < self.from || bar.timestamp > self.to {
            return;
        }
        let crossed_under = bar.low < lower && low_before >= lower_before;
        let crossed_over = bar.high > upper && high_before <= upper_before;
        let held: Vec<(String, OrderSide, f64)> =
            strategy.bindings().iter().map(|b| (b.name.clone(), b.side, b.price)).collect();
        let protect_held = |strategy: &mut Tactics<'_>, side: OrderSide| {
            for (name, _, price) in held.iter().filter(|h| h.1 == side) {
                protect(strategy, name, side, *price, bar.close);
            }
        };
        if bar.close > upper {
            strategy.entry("long", Long);
            self.coming = Some(bar.close);
        }
        protect_held(strategy, Long);
        if crossed_under {
            strategy.close("long");
        }
        if bar.close < lower {
            strategy.entry("short", Short);
            self.coming = Some(bar.close);
        }
        protect_held(strategy, Short);
        if crossed_over {
            strategy.close("short");
        }
    }

    /// The entry placed on the bar before gets its exit from its fill on.
    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let Some(close) = self.coming else { return };
        let Some(b) = strategy.bindings().last().filter(|b| b.entry_ms == bar.timestamp) else {
            return;
        };
        let (name, side, price) = (b.name.clone(), b.side, b.price);
        self.coming = None;
        protect(strategy, &name, side, price, close);
    }
}
