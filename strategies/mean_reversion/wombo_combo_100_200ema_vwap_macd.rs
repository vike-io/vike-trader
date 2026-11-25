// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "WOMBO COMBO: 100/200EMA & VWAP & MACD" by MakeMoneyCoESTB2020, https://www.tradingview.com/script/2mVl2d9h/
// The original's notice: © MakeMoneyCoESTB2020
// Changed: rewritten by hand in Rust for vike.

//! A Schaff trend cycle on EMA(23) - EMA(50) with a 40-bar cycle and 3-bar smoothing, filtered by EMA(200) and the day's VWAP of
//! the typical price. Long 5% of the account's worth when the cycle crosses up through 15 with the close over both lines; short
//! when it crosses down through 85 with the close under both. Up to two entries a side, an entry reversing the other side. While
//! held, a stop 15% and a target 25% from the average cost. 2 ticks; starts with 20,000.
//!
//! A market entry is sized at this close moved the adverse way by the slippage.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use vike_indicators::indicators::{Ema, Stochf, Vwap, VwapSource};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::sizing::share_of_worth;
use vike_tactics::{Rule, RuleDefaults, Tactics};

const LONG_NAME: &str = "HC-Long";
const SHARE: f64 = 5.0;
const SHORT_NAME: &str = "HC-Short";
const CYCLE: f64 = 40.0;
const STOP: f64 = 0.15;
const TAKE: f64 = 0.25;

/// The reference's `stoch(x, x, x, length)`: vike's `Stochf` %K of `x` over the last `length` bars, a NaN skipped, NaN while `x`
/// is and before bar `length - 1`. A window with no range is 0 there and NaN in the reference; here that happens only in the
/// warm-up, where `Held` reads both as its 0.
fn stoch() -> Stochf {
    Stochf::new(CYCLE, 1.0).expect("valid lengths")
}

/// The last value that was not NaN, 0 before there is one: the reference's `nz(fixnan(x))`.
#[derive(Default)]
struct Held(f64);

impl Held {
    fn next(&mut self, x: f64) -> f64 {
        if !x.is_nan() {
            self.0 = x;
        }
        self.0
    }
}

/// The Schaff trend cycle: a stochastic of the MACD, smoothed, a stochastic of that, smoothed again, held in 0..=100.
/// Not vike's `Stc`: the script smooths by an EMA(3) seeded with its first three values' mean, where `Stc` halves from the
/// first bar on.
struct Stc {
    fast: Ema,
    slow: Ema,
    first: Stochf,
    first_held: Held,
    first_smooth: Ema,
    second: Stochf,
    second_held: Held,
    second_smooth: Ema,
}

impl Stc {
    fn new() -> Self {
        let ema = |n: f64| Ema::new(n).expect("a valid length");
        Stc {
            fast: ema(23.0),
            slow: ema(50.0),
            first: stoch(),
            first_held: Held::default(),
            first_smooth: ema(3.0),
            second: stoch(),
            second_held: Held::default(),
            second_smooth: ema(3.0),
        }
    }

    fn next(&mut self, close: f64) -> f64 {
        let macd = self.fast.next_value(close) - self.slow.next_value(close);
        let k = self.first_held.next(self.first.next_hlc(macd, macd, macd)[0]);
        let d = self.first_smooth.next_value(k);
        let kd = self.second_held.next(self.second.next_hlc(d, d, d)[0]);
        self.second_smooth.next_value(kd).clamp(0.0, 100.0)
    }
}

struct S {
    stc: Stc,
    ema: IndicatorLine,
    vwap: Vwap,
    buy: Cross,
    sell: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        stc: Stc::new(),
        ema: IndicatorLine::new("ema", &[200.0]),
        vwap: Vwap::default().with_source(VwapSource::TypicalDivided),
        buy: Cross::new(),
        sell: Cross::new(),
    })
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let sizing = strategy.sizing();
        let stc = self.stc.next(bar.close);
        let ema = self.ema.next(bar)[0];
        let vwap = self.vwap.next(bar);
        let up = matches!(self.buy.step(stc, 15.0), Signal::Long);
        let down = matches!(self.sell.step(stc, 85.0), Signal::Short);
        if up && bar.close > ema && bar.close > vwap {
            strategy.entry(LONG_NAME, Long).size(share_of_worth(
                &sizing,
                SHARE,
                sizing.slipped(Long, bar.close),
            ));
        }
        if down && bar.close < ema && bar.close < vwap {
            strategy.entry(SHORT_NAME, Short).size(share_of_worth(
                &sizing,
                SHARE,
                sizing.slipped(Short, bar.close),
            ));
        }
        let held = strategy.position();
        let cost = strategy.avg_cost();
        if held > 0.0 {
            strategy
                .exit("Close Long")
                .from(LONG_NAME)
                .take(cost * (1.0 + TAKE))
                .stop(cost * (1.0 - STOP));
        }
        if held < 0.0 {
            strategy
                .exit("Close Short")
                .from(SHORT_NAME)
                .take(cost * (1.0 - TAKE))
                .stop(cost * (1.0 + STOP));
        }
    }
}
