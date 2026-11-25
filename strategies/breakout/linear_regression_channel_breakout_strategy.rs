// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Linear Regression Channel Breakout Strategy" by chanu_lev10k, https://www.tradingview.com/script/7USLDe0j/
// The original's notice: © LonesomeTheBlue © chanu_lev10k
// Changed: rewritten by hand in Rust for vike.

//! A channel around the 20-bar least-squares line of the close: long 1 when the close ends above the line's newest point
//! plus 2.3 deviations, short 1 when it ends below that point less 2.3, each reversing the other and neither on a flat
//! line. While held, a target 11% past the average entry, issued from the bar after the fill. No costs; starts with
//! 1,000,000.
//!
//! The rule's line is its own: the slope is the regression line at this bar less the line one bar back, the intercept the
//! mean less ten slopes plus half a slope, and the deviation measures each close against `slope * (20 - x) + intercept`, x
//! bars back, which is one bar ahead of the line it draws.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::rolling::RollingSum;
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const LEN: usize = 20;
const DEVIATIONS: f64 = 2.3;
const TAKE: f64 = 11.0 / 100.0;

struct S {
    /// The last `LEN` closes, oldest first.
    closes: VecDeque<f64>,
    sum: RollingSum,
    /// `1 + 2 + ... + LEN` and `LEN * sum(x^2) - sum_x^2`, summed in that order.
    sum_x: f64,
    denominator: f64,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    let (mut sx, mut sx2) = (0.0, 0.0);
    for i in 1..=LEN {
        let x = i as f64;
        sx += x;
        sx2 += x * x;
    }
    Box::new(S {
        closes: VecDeque::with_capacity(LEN + 1),
        sum: RollingSum::new(LEN),
        sum_x: sx,
        denominator: LEN as f64 * sx2 - sx * sx,
    })
}

impl S {
    /// The reference's linear regression of the window, evaluated `offset` bars back from its newest point.
    fn linreg(&self, offset: usize) -> f64 {
        let n = LEN as f64;
        let (mut sum_y, mut sum_xy, mut x) = (0.0, 0.0, 0.0);
        for &y in &self.closes {
            x += 1.0;
            sum_y += y;
            sum_xy += y * x;
        }
        let slope = (n * sum_xy - self.sum_x * sum_y) / self.denominator;
        let average = sum_y / n;
        let intercept = average - slope * self.sum_x / n + slope;
        intercept + slope * (n - 1.0 - offset as f64)
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let c = bar.close;
        let mid = self.sum.next(c) / LEN as f64;
        self.closes.push_back(c);
        if self.closes.len() > LEN {
            self.closes.pop_front();
        }
        let held = strategy.position();
        let avg = strategy.avg_cost();
        if self.closes.len() == LEN {
            let slope = self.linreg(0) - self.linreg(1);
            let intercept = mid - slope * (LEN / 2) as f64 + 0.5 * slope;
            let end = intercept + slope * (LEN - 1) as f64;
            let mut dev = 0.0;
            for (x, &y) in self.closes.iter().rev().enumerate() {
                let r = y - (slope * (LEN - x) as f64 + intercept);
                dev += r * r;
            }
            let dev = (dev / LEN as f64).sqrt();
            if slope != 0.0 && c > end + dev * DEVIATIONS {
                strategy.entry("Long", Long).size(1.0);
            }
            if held > 0.0 {
                strategy.exit("Long Take Profit").from("Long").take(avg * (1.0 + TAKE));
            }
            if slope != 0.0 && c < end - dev * DEVIATIONS {
                strategy.entry("Short", Short).size(1.0);
            }
            if held < 0.0 {
                strategy.exit("Short Take Profit").from("Short").take(avg * (1.0 - TAKE));
            }
        }
    }
}
